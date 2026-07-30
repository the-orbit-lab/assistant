//! Orbit as an MCP *client*: connects to external `stdio` MCP servers
//! configured in `.orbit/project.yaml`, lists their tools, and wraps each
//! one as an [`orbit_actions::Action`] namespaced `mcp.<server>.<tool>`.
//!
//! Wrapping external tools as ordinary [`Action`] implementations means
//! they flow through the exact same [`orbit_actions::ActionRegistry`] as
//! native actions — the same permission enforcement, the same execution
//! records — with no MCP-specific logic anywhere else in Orbit.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use orbit_project::{McpConfig, McpServerConfig, McpTransport};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::Value;
use tokio::process::Command;

type ClientConnection = RunningService<RoleClient, ()>;

/// A server that was configured but could not be connected to at startup.
/// Orbit degrades gracefully: the rest of the session still works with
/// whatever did connect.
#[derive(Debug, Clone)]
pub struct McpConnectionWarning {
    pub server: String,
    pub message: String,
}

/// Namespace prefix reserved for native actions; external server names
/// that would collide with it are rejected rather than silently shadowing
/// a native action.
const RESERVED_PREFIXES: &[&str] = &["project", "command"];

fn namespaced_action_name(server: &str, tool: &str) -> String {
    format!("mcp.{server}.{tool}")
}

/// Owns every live external MCP server connection for a session, so they
/// can be shut down cleanly together.
#[derive(Default)]
pub struct McpClientManager {
    connections: HashMap<String, Arc<ClientConnection>>,
}

impl McpClientManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect every enabled, non-reserved server in `config`, returning
    /// the actions it exposed and a warning for any server that failed to
    /// start or initialize. A failed server never aborts the whole call.
    pub async fn connect_all(
        &mut self,
        config: &McpConfig,
    ) -> (Vec<Arc<dyn Action>>, Vec<McpConnectionWarning>) {
        let mut actions: Vec<Arc<dyn Action>> = Vec::new();
        let mut warnings = Vec::new();

        for (name, server) in &config.servers {
            if !server.enabled {
                continue;
            }
            if RESERVED_PREFIXES.contains(&name.as_str()) {
                warnings.push(McpConnectionWarning {
                    server: name.clone(),
                    message: format!(
                        "server name `{name}` collides with a reserved native action \
                         namespace; rename the server in `.orbit/project.yaml`"
                    ),
                });
                continue;
            }

            match self.connect_one(name, server).await {
                Ok(mut connected) => actions.append(&mut connected),
                Err(message) => warnings.push(McpConnectionWarning {
                    server: name.clone(),
                    message,
                }),
            }
        }

        (actions, warnings)
    }

    async fn connect_one(
        &mut self,
        name: &str,
        server: &McpServerConfig,
    ) -> Result<Vec<Arc<dyn Action>>, String> {
        let McpTransport::Stdio = server.transport;

        let transport = TokioChildProcess::new(Command::new(&server.command).configure(|cmd| {
            cmd.args(&server.args);
        }))
        .map_err(|e| format!("failed to spawn `{}`: {e}", server.command))?;

        let connection: ClientConnection = ()
            .serve(transport)
            .await
            .map_err(|e| format!("failed to initialize MCP session: {e}"))?;

        let tools = connection
            .list_tools(Default::default())
            .await
            .map_err(|e| format!("failed to list tools: {e}"))?;

        let connection = Arc::new(connection);
        self.connections
            .insert(name.to_string(), connection.clone());

        let actions = tools
            .tools
            .into_iter()
            .map(|tool| {
                Arc::new(McpToolAction {
                    server: name.to_string(),
                    tool_name: tool.name.to_string(),
                    description: tool.description.map(|d| d.to_string()).unwrap_or_else(|| {
                        format!("MCP tool `{}` from server `{name}`", tool.name)
                    }),
                    input_schema: Value::Object(tool.input_schema.as_ref().clone()),
                    connection: connection.clone(),
                }) as Arc<dyn Action>
            })
            .collect();

        Ok(actions)
    }

    /// Best-effort graceful shutdown of every connected server. Errors are
    /// swallowed: a server that already crashed can't be cancelled
    /// cleanly, and that's fine — the child process transport is dropped
    /// regardless.
    pub async fn shutdown(self) {
        for (name, connection) in self.connections {
            if let Ok(connection) = Arc::try_unwrap(connection)
                && let Err(e) = connection.cancel().await
            {
                tracing::debug!("error shutting down MCP server `{name}`: {e}");
            }
        }
    }
}

/// Adapts a single external MCP tool into an Orbit [`Action`].
struct McpToolAction {
    server: String,
    tool_name: String,
    description: String,
    input_schema: Value,
    connection: Arc<ClientConnection>,
}

#[async_trait]
impl Action for McpToolAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: namespaced_action_name(&self.server, &self.tool_name),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            // External tools are unknown code paths outside Orbit's own
            // review; default to requiring confirmation unless the project
            // explicitly allows this exact action.
            default_permission: Permission::Ask,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        if !input.is_object() {
            return Err(OrbitError::InvalidActionInput {
                name: namespaced_action_name(&self.server, &self.tool_name),
                reason: "arguments must be a JSON object".to_string(),
            });
        }
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let name = namespaced_action_name(&self.server, &self.tool_name);
        let arguments = input.0.as_object().cloned();

        let mut params = CallToolRequestParams::new(self.tool_name.clone());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }

        let result = self
            .connection
            .call_tool(params)
            .await
            .map_err(|e| OrbitError::Mcp(format!("`{name}` failed: {e}")))?;

        if result.is_error == Some(true) {
            let message = result
                .content
                .iter()
                .filter_map(content_block_text)
                .collect::<Vec<_>>()
                .join("\n");
            return Err(OrbitError::Mcp(format!(
                "`{name}` reported an error: {message}"
            )));
        }

        let text: Vec<Value> = result
            .content
            .iter()
            .filter_map(content_block_text)
            .map(Value::String)
            .collect();

        let data = result
            .structured_content
            .unwrap_or_else(|| serde_json::json!({ "content": text }));

        Ok(ActionOutput::new(data))
    }
}

fn content_block_text(block: &rmcp::model::ContentBlock) -> Option<String> {
    block.as_text().map(|t| t.text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_actions_under_the_server_name() {
        assert_eq!(
            namespaced_action_name("github", "create_issue"),
            "mcp.github.create_issue"
        );
    }

    #[tokio::test]
    async fn reserved_server_names_are_rejected() {
        let mut manager = McpClientManager::new();
        let mut servers = std::collections::BTreeMap::new();
        servers.insert(
            "project".to_string(),
            McpServerConfig {
                transport: McpTransport::Stdio,
                command: "does-not-matter".to_string(),
                args: vec![],
                enabled: true,
            },
        );
        let config = McpConfig {
            expose: vec![],
            servers,
        };
        let (actions, warnings) = manager.connect_all(&config).await;
        assert!(actions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("reserved"));
    }

    #[tokio::test]
    async fn unreachable_command_produces_a_warning_not_a_panic() {
        let mut manager = McpClientManager::new();
        let mut servers = std::collections::BTreeMap::new();
        servers.insert(
            "broken".to_string(),
            McpServerConfig {
                transport: McpTransport::Stdio,
                command: "orbit-definitely-not-a-real-binary".to_string(),
                args: vec![],
                enabled: true,
            },
        );
        let config = McpConfig {
            expose: vec![],
            servers,
        };
        let (actions, warnings) = manager.connect_all(&config).await;
        assert!(actions.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].server, "broken");
    }
}
