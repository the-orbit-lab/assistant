//! Orbit as an MCP *server*: exposes a filtered view of the Action Runtime
//! over `stdio` to external MCP hosts (Claude Code, ChatGPT, ...).
//!
//! This crate adds no execution logic of its own. It reuses the same
//! [`ActionRegistry`], the same [`ActionContext`], and the same permission
//! enforcement the CLI and agent use — it only translates MCP's wire
//! protocol into `ActionRegistry::execute` calls and back, and it never
//! exposes an action the project configuration didn't explicitly list in
//! `mcp.expose`.

use std::collections::HashSet;
use std::sync::Arc;

use orbit_actions::{ActionContext, ActionRegistry};
use orbit_core::{ActionInput, AlwaysDeny, ConfirmationProvider, OrbitError};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::Value;

/// The MCP server never itself prompts interactively: an `ask`-permission
/// action is only reachable through MCP once a project explicitly sets it
/// to `allow` in `.orbit/project.yaml`.
const NON_INTERACTIVE: AlwaysDeny = AlwaysDeny;

pub struct OrbitMcpServer {
    registry: Arc<ActionRegistry>,
    context: Arc<ActionContext>,
    exposed: HashSet<String>,
    confirmation: Arc<dyn ConfirmationProvider>,
}

impl OrbitMcpServer {
    /// `exposed` is the project's `mcp.expose` list. Actions not in this
    /// set are invisible through MCP: not listed, and rejected if called
    /// by name.
    pub fn new(registry: ActionRegistry, context: ActionContext, exposed: Vec<String>) -> Self {
        Self {
            registry: Arc::new(registry),
            context: Arc::new(context),
            exposed: exposed.into_iter().collect(),
            confirmation: Arc::new(NON_INTERACTIVE),
        }
    }

    /// Override the confirmation provider (tests only; production MCP
    /// serving always stays non-interactive).
    #[doc(hidden)]
    pub fn with_confirmation(mut self, confirmation: Arc<dyn ConfirmationProvider>) -> Self {
        self.confirmation = confirmation;
        self
    }

    fn exposed_tool(&self, name: &str) -> Option<Tool> {
        if !self.exposed.contains(name) {
            return None;
        }
        let descriptor = self
            .registry
            .descriptors()
            .into_iter()
            .find(|d| d.name == name)?;
        let schema = descriptor
            .input_schema
            .as_object()
            .cloned()
            .unwrap_or_default();
        Some(Tool::new(descriptor.name, descriptor.description, schema))
    }

    pub async fn serve_stdio(self) -> Result<(), OrbitError> {
        let service = self
            .serve(stdio())
            .await
            .map_err(|e| OrbitError::Mcp(format!("failed to start MCP stdio server: {e}")))?;
        service
            .waiting()
            .await
            .map_err(|e| OrbitError::Mcp(format!("MCP server exited with an error: {e}")))?;
        Ok(())
    }
}

impl ServerHandler for OrbitMcpServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Orbit exposes a filtered set of project actions: local file discovery, \
                 deterministic search, file reads, and configured commands. Only actions listed \
                 in this project's `mcp.expose` configuration are available."
                    .to_string(),
            )
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.exposed_tool(name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools: Vec<Tool> = self
            .registry
            .descriptors()
            .into_iter()
            .filter(|d| self.exposed.contains(&d.name))
            .filter_map(|d| self.exposed_tool(&d.name))
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !self.exposed.contains(&name) {
            return Err(McpError::invalid_params(
                format!("tool `{name}` is not exposed by this Orbit project"),
                None,
            ));
        }

        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let (_, result) = self
            .registry
            .execute(
                &self.context,
                &name,
                ActionInput(arguments),
                self.confirmation.as_ref(),
            )
            .await;

        let result = match result {
            Ok(output) => CallToolResult::success(vec![ContentBlock::text(output.to_model_text())]),
            Err(err) => CallToolResult::error(vec![ContentBlock::text(err.to_string())]),
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::AlwaysAllow;
    use orbit_project::ProjectConfig;
    use rmcp::ServiceExt as ClientServiceExt;

    fn test_server(root: &std::path::Path, expose: Vec<&str>) -> OrbitMcpServer {
        let config = ProjectConfig::parse(
            "version: 1\nproject:\n  name: demo\ncontext:\n  include:\n    - \"**/*\"\n",
        )
        .unwrap();
        let mut registry = ActionRegistry::new();
        orbit_actions::native::register_all(&mut registry).unwrap();
        let context = ActionContext {
            root: root.to_path_buf(),
            config_path: root.join(".orbit/project.yaml"),
            config,
        };
        OrbitMcpServer::new(
            registry,
            context,
            expose.into_iter().map(String::from).collect(),
        )
        .with_confirmation(Arc::new(AlwaysAllow))
    }

    /// Spins up the real server against a real client over an in-process
    /// duplex pipe: a genuine MCP `initialize` + `tools/list`/`tools/call`
    /// round trip, not a hand-built request context.
    async fn connected_client(
        server: OrbitMcpServer,
    ) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let running = server
                .serve(server_io)
                .await
                .expect("server failed to start");
            running.waiting().await.ok();
        });
        ().serve(client_io).await.expect("client failed to connect")
    }

    #[tokio::test]
    async fn only_exposed_actions_are_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let server = test_server(&root, vec!["project.search"]);
        let client = connected_client(server).await;

        let tools = client.list_tools(Default::default()).await.unwrap();
        assert_eq!(tools.tools.len(), 1);
        assert_eq!(tools.tools[0].name, "project.search");
        client.cancel().await.ok();
    }

    #[tokio::test]
    async fn unexposed_action_cannot_be_called() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let server = test_server(&root, vec!["project.search"]);
        let client = connected_client(server).await;

        let err = client
            .call_tool(CallToolRequestParams::new("project.read_file"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not exposed"));
        client.cancel().await.ok();
    }

    #[tokio::test]
    async fn exposed_action_executes_and_returns_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("README.md"), "hello from orbit").unwrap();
        let server = test_server(&root, vec!["project.read_file"]);
        let client = connected_client(server).await;

        let result = client
            .call_tool(
                CallToolRequestParams::new("project.read_file").with_arguments(
                    serde_json::json!({"path": "README.md"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let text = match &result.content[0] {
            ContentBlock::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("hello from orbit"));
        client.cancel().await.ok();
    }
}
