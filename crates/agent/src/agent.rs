use std::sync::Arc;
use std::time::Duration;

use orbit_actions::{ActionContext, ActionRegistry};
use orbit_core::{
    ActionInput, ConfirmationProvider, ExecutionRecord, FinishReason, Message, ModelRequest,
    OrbitError, SourceReference, ToolDefinition,
};
use orbit_providers::ModelProvider;

use crate::prompt::system_prompt;
use crate::retrieval;

pub const DEFAULT_MAX_ITERATIONS: u32 = 8;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// The result of a single `Agent::run` call: the final answer, every
/// source the actions it called returned, and the execution records for
/// session/audit history.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub answer: String,
    pub sources: Vec<SourceReference>,
    pub records: Vec<ExecutionRecord>,
}

/// Orbit's own agent loop: calls the Action Runtime directly. It never
/// goes through Orbit's MCP server to run a native action — that indirection
/// exists only for external hosts consuming Orbit, not for Orbit itself.
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ActionRegistry>,
    context: Arc<ActionContext>,
    confirmation: Arc<dyn ConfirmationProvider>,
    max_iterations: u32,
    request_timeout: Duration,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        registry: Arc<ActionRegistry>,
        context: Arc<ActionContext>,
        confirmation: Arc<dyn ConfirmationProvider>,
    ) -> Self {
        Self {
            provider,
            registry,
            context,
            confirmation,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub fn with_max_iterations(mut self, max_iterations: u32) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Answer `question`, appending to and mutating `history` in place so
    /// callers (e.g. `orbit chat`) can keep a running conversation across
    /// calls. Returns once the model produces a final answer, the
    /// iteration limit is hit, or the provider fails.
    pub async fn run(
        &self,
        history: &mut Vec<Message>,
        question: &str,
    ) -> Result<AgentOutcome, OrbitError> {
        if history.is_empty() {
            history.push(Message::system(system_prompt(
                &self.context.config.project.name,
                &self.context.config.project.description,
            )));
        }
        history.push(Message::user(question));

        let tools: Vec<ToolDefinition> = self
            .registry
            .descriptors()
            .into_iter()
            .map(|d| ToolDefinition {
                name: d.name,
                description: d.description,
                input_schema: d.input_schema,
            })
            .collect();
        tracing::debug!(
            tool_count = tools.len(),
            tools = ?tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            "tools offered to model"
        );

        let mut sources = Vec::new();
        let mut records = Vec::new();

        // Broad "what does this do" questions have no keyword a search
        // could key off, and a small local model unreliably decides on its
        // own to call project.information -> project.search ->
        // project.read_file in the right order. Do it deterministically
        // instead of hoping the model gets there.
        if retrieval::is_broad_overview_question(question) {
            tracing::debug!(
                question,
                "broad overview question detected; running deterministic retrieval"
            );
            let (retrieved_sources, retrieved_records) = retrieval::run(
                &self.registry,
                &self.context,
                self.confirmation.as_ref(),
                history,
            )
            .await;
            tracing::debug!(
                sources = retrieved_sources.len(),
                "deterministic retrieval complete"
            );
            sources.extend(retrieved_sources);
            records.extend(retrieved_records);
        }

        for iteration in 0..self.max_iterations {
            tracing::debug!(iteration, "requesting model completion");
            let request = ModelRequest {
                model: self.context.config.model.model.clone(),
                messages: history.clone(),
                tools: tools.clone(),
                timeout: self.request_timeout,
            };

            let response = self.provider.chat(&request).await?;

            if response.finish_reason == FinishReason::ToolCalls
                && !response.message.tool_calls.is_empty()
            {
                let calls = response.message.tool_calls.clone();
                tracing::debug!(
                    calls = ?calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                    "model requested tool calls"
                );
                history.push(response.message);

                for call in calls {
                    let (record, result) = self
                        .registry
                        .execute(
                            &self.context,
                            &call.name,
                            ActionInput(call.arguments),
                            self.confirmation.as_ref(),
                        )
                        .await;
                    tracing::debug!(
                        action = %call.name,
                        success = record.success,
                        permission = ?record.permission_outcome,
                        duration_ms = record.duration().as_millis() as u64,
                        "action execution finished"
                    );
                    records.push(record);

                    match result {
                        Ok(output) => {
                            sources.extend(output.sources.iter().cloned());
                            history.push(Message::tool_result(&call.id, output.to_model_text()));
                        }
                        Err(err) => {
                            history.push(Message::tool_result(&call.id, format!("Error: {err}")));
                        }
                    }
                }
                tracing::debug!(total_sources = sources.len(), "sources collected so far");
                continue;
            }

            history.push(response.message.clone());
            let sources = dedupe_sources(sources);
            tracing::debug!(
                answer_len = response.message.content.len(),
                source_count = sources.len(),
                "agent run finished"
            );
            return Ok(AgentOutcome {
                answer: response.message.content,
                sources,
                records,
            });
        }

        Err(OrbitError::AgentIterationLimitReached {
            limit: self.max_iterations,
        })
    }
}

fn dedupe_sources(sources: Vec<SourceReference>) -> Vec<SourceReference> {
    let mut deduped: Vec<SourceReference> = Vec::new();
    for source in sources {
        if !deduped.contains(&source) {
            deduped.push(source);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::{AlwaysDeny, ToolCall};
    use orbit_project::ProjectConfig;
    use orbit_providers::MockProvider;
    use serde_json::json;

    fn context(root: &std::path::Path) -> Arc<ActionContext> {
        // ActionContext::root must be canonical (see its doc comment) --
        // tempfile's paths go through a symlink on macOS (/tmp ->
        // /private/tmp), so path-resolving actions like project.read_file
        // would otherwise see every request as a symlink escape.
        let root = root.canonicalize().unwrap();
        let config = ProjectConfig::parse(
            "version: 1\nproject:\n  name: demo\n  description: a demo project\ncontext:\n  include:\n    - \"**/*\"\n",
        )
        .unwrap();
        Arc::new(ActionContext {
            config_path: root.join(".orbit/project.yaml"),
            root,
            config,
        })
    }

    fn registry() -> Arc<ActionRegistry> {
        Arc::new(orbit_actions::native_registry().unwrap())
    }

    #[tokio::test]
    async fn answers_directly_when_no_tool_call_is_made() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(MockProvider::new(vec![orbit_core::ModelResponse {
            message: Message::assistant("Orbit is a local-first engineering assistant."),
            finish_reason: FinishReason::Stop,
        }]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        );
        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "What is Orbit?").await.unwrap();
        assert_eq!(
            outcome.answer,
            "Orbit is a local-first engineering assistant."
        );
        assert!(outcome.sources.is_empty());
    }

    #[tokio::test]
    async fn executes_a_tool_call_and_aggregates_sources() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("watchdog.md"),
            "# Watchdog\nkeeps things alive\n",
        )
        .unwrap();

        let provider = Arc::new(MockProvider::new(vec![
            orbit_core::ModelResponse {
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_0".to_string(),
                    name: "project.search".to_string(),
                    arguments: json!({"query": "watchdog"}),
                }]),
                finish_reason: FinishReason::ToolCalls,
            },
            orbit_core::ModelResponse {
                message: Message::assistant("The watchdog keeps things alive."),
                finish_reason: FinishReason::Stop,
            },
        ]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        );
        let mut history = Vec::new();
        let outcome = agent
            .run(&mut history, "What does the watchdog do?")
            .await
            .unwrap();

        assert_eq!(outcome.answer, "The watchdog keeps things alive.");
        assert!(!outcome.sources.is_empty());
        assert_eq!(outcome.records.len(), 1);
        assert!(outcome.records[0].success);
    }

    #[tokio::test]
    async fn unknown_tool_call_is_reported_back_to_the_model_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = Arc::new(MockProvider::new(vec![
            orbit_core::ModelResponse {
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_0".to_string(),
                    name: "does.not.exist".to_string(),
                    arguments: json!({}),
                }]),
                finish_reason: FinishReason::ToolCalls,
            },
            orbit_core::ModelResponse {
                message: Message::assistant("I could not find that tool."),
                finish_reason: FinishReason::Stop,
            },
        ]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        );
        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "do something").await.unwrap();
        assert_eq!(outcome.answer, "I could not find that tool.");
        assert!(!outcome.records[0].success);
    }

    #[tokio::test]
    async fn stops_after_max_iterations_without_a_final_answer() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "alpha\n").unwrap();
        let looping_response = || orbit_core::ModelResponse {
            message: Message::assistant_tool_calls(vec![ToolCall {
                id: "call_0".to_string(),
                name: "project.list_files".to_string(),
                arguments: json!({}),
            }]),
            finish_reason: FinishReason::ToolCalls,
        };
        let provider = Arc::new(MockProvider::new(
            std::iter::repeat_with(looping_response).take(10).collect(),
        ));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_max_iterations(3);
        let mut history = Vec::new();
        let err = agent.run(&mut history, "loop forever").await.unwrap_err();
        assert!(matches!(
            err,
            OrbitError::AgentIterationLimitReached { limit: 3 }
        ));
    }

    /// Regression test for a real failure: `orbit ask "What does this
    /// repository do?"` answered "no explicit descriptions ... found" even
    /// though the temp repo has a README and a docs/PROJECT_SPEC.md that
    /// plainly describe it. Root cause was the local model deciding not to
    /// call any tool for a vague question. This reproduces that exact
    /// model behavior with a mock (a Stop response with no tool call, on
    /// the very first turn) and asserts that grounding happened anyway:
    /// the README/spec content must have reached the provider, and the
    /// final sources must reference them.
    #[tokio::test]
    async fn broad_overview_question_is_grounded_even_if_the_model_never_calls_a_tool() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("README.md"),
            "# Orbit\n\nOrbit is a local-first AI engineering assistant that inspects a \
             project's files, runs deterministic actions, and answers grounded in real \
             sources.\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        std::fs::write(
            tmp.path().join("docs/PROJECT_SPEC.md"),
            "# Project Specification\n\nOrbit must inspect allowed project files, call \
             actions, enforce permissions, and explain its answers with sources.\n",
        )
        .unwrap();

        // Exactly the reported bug: the model never requests a tool call
        // and answers immediately from nothing.
        let provider = Arc::new(MockProvider::new(vec![orbit_core::ModelResponse {
            message: Message::assistant("I could not find a clear description."),
            finish_reason: FinishReason::Stop,
        }]));

        let agent = Agent::new(
            provider.clone(),
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        );
        let mut history = Vec::new();
        let outcome = agent
            .run(&mut history, "What does this repository do?")
            .await
            .unwrap();

        assert!(
            !outcome.sources.is_empty(),
            "expected sources from deterministic retrieval, got none"
        );
        assert!(
            outcome
                .sources
                .iter()
                .any(|s| s.path.ends_with("README.md")),
            "expected README.md among sources, got {:?}",
            outcome.sources
        );

        // The requirement is stronger than "sources were recorded": the
        // actual grounding text must have been sent to the provider before
        // it ever answered.
        let sent_requests = provider.recorded_requests();
        assert!(!sent_requests.is_empty());
        let all_sent_text: String = sent_requests
            .iter()
            .flat_map(|r| r.messages.iter())
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all_sent_text.contains("local-first AI engineering assistant"),
            "README content did not reach the model:\n{all_sent_text}"
        );
        assert!(
            all_sent_text.contains("enforce permissions"),
            "docs/PROJECT_SPEC.md content did not reach the model:\n{all_sent_text}"
        );
    }

    #[tokio::test]
    async fn denied_permission_is_relayed_as_a_tool_error_not_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ProjectConfig::parse(
            "version: 1\nproject:\n  name: demo\ncommands:\n  test:\n    program: echo\n    args: [hi]\npermissions:\n  command.run_configured: deny\n",
        )
        .unwrap();
        let ctx = Arc::new(ActionContext {
            root: tmp.path().to_path_buf(),
            config_path: tmp.path().join(".orbit/project.yaml"),
            config,
        });
        let provider = Arc::new(MockProvider::new(vec![
            orbit_core::ModelResponse {
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_0".to_string(),
                    name: "command.run_configured".to_string(),
                    arguments: json!({"name": "test"}),
                }]),
                finish_reason: FinishReason::ToolCalls,
            },
            orbit_core::ModelResponse {
                message: Message::assistant("That command is not allowed."),
                finish_reason: FinishReason::Stop,
            },
        ]));
        let agent = Agent::new(provider, registry(), ctx, Arc::new(AlwaysDeny));
        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "run the tests").await.unwrap();
        assert_eq!(outcome.answer, "That command is not allowed.");
        assert!(!outcome.records[0].success);
        let tool_message = history
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("call_0"))
            .unwrap();
        assert!(tool_message.content.contains("denied"));
    }
}
