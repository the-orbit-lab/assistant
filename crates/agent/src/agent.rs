use std::sync::Arc;
use std::time::Duration;

use orbit_actions::{ActionContext, ActionRegistry};
use orbit_core::{
    ActionInput, CancellationToken, ConfirmationProvider, EventEmitter, EventPayload,
    ExecutionRecord, FinishReason, Message, ModelRequest, ModelResponse, OrbitError,
    SourceReference, ToolDefinition,
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
    /// The turn stopped early because it was cancelled. `answer` then
    /// holds whatever text had already been produced (possibly empty),
    /// and `sources`/`records` hold the work that really completed before
    /// the stop — cancellation never claims that finished work was undone.
    pub cancelled: bool,
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
    builtin_overview_retrieval: bool,
    events: EventEmitter,
    cancel: CancellationToken,
    streaming: bool,
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
            builtin_overview_retrieval: true,
            events: EventEmitter::null(),
            cancel: CancellationToken::new(),
            streaming: false,
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

    /// Report this run's progress to `events`. Without this the agent
    /// behaves identically but silently — the emitter defaults to a null
    /// one, so nothing branches on whether anyone is observing.
    pub fn with_events(mut self, events: EventEmitter) -> Self {
        self.events = events;
        self
    }

    /// Allow this run to be cancelled. The token is checked before each
    /// model request, between tool calls, and (via the delta handler)
    /// between streamed chunks.
    pub fn with_cancellation(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Request incremental delivery of assistant text as
    /// `ResponseDelta` events. Falls back transparently to a single
    /// delta when the provider cannot stream, so enabling it is always
    /// safe.
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// The single-project `project.information` -> `project.read_file`
    /// deterministic retrieval (see `retrieval::run`) only makes sense
    /// when this agent's registry actually has those native actions
    /// registered. A caller driving a *workspace*-scoped registry (only
    /// `workspace.*` actions) does its own deterministic retrieval before
    /// the first turn and should disable this, since attempting it here
    /// would just be silently-failing `UnknownAction` calls against
    /// `project.information`.
    pub fn with_builtin_overview_retrieval(mut self, enabled: bool) -> Self {
        self.builtin_overview_retrieval = enabled;
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
        self.continue_from_history(history, question).await
    }

    /// Like [`Agent::run`], but assumes the caller has already appended
    /// the user's question (and, for a workspace-scoped caller, its own
    /// pre-model deterministic retrieval) to `history` in the right
    /// order. `question` is still needed to decide whether this agent's
    /// own built-in broad-overview retrieval applies -- irrelevant when
    /// [`Agent::with_builtin_overview_retrieval`] is `false`, which every
    /// workspace-scoped caller sets, since it does that retrieval itself.
    pub async fn continue_from_history(
        &self,
        history: &mut Vec<Message>,
        question: &str,
    ) -> Result<AgentOutcome, OrbitError> {
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
        // own to call project.information -> project.read_file in the
        // right order. Do it deterministically instead of hoping the
        // model gets there.
        if self.builtin_overview_retrieval && retrieval::is_broad_overview_question(question) {
            tracing::debug!(
                question,
                "broad overview question detected; running deterministic retrieval"
            );
            let (retrieved_sources, retrieved_records) = retrieval::run(
                &self.registry,
                &self.context,
                self.confirmation.as_ref(),
                history,
                &self.events,
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
            if self.cancel.is_cancelled() {
                return Ok(self.cancelled_outcome(String::new(), sources, records));
            }

            tracing::debug!(iteration, "requesting model completion");
            let request = ModelRequest {
                model: self.context.config.model.model.clone(),
                messages: history.clone(),
                tools: tools.clone(),
                timeout: self.request_timeout,
            };

            let response = match self.complete(&request).await {
                Ok(response) => response,
                Err(err) => {
                    self.events.emit(EventPayload::Failure {
                        message: err.to_string(),
                    });
                    return Err(err);
                }
            };

            // The provider stops early when the delta handler reports a
            // cancellation, so a partially streamed answer lands here.
            if self.cancel.is_cancelled() {
                return Ok(self.cancelled_outcome(response.message.content, sources, records));
            }

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
                    if self.cancel.is_cancelled() {
                        // Stop before running anything further. Whatever
                        // already ran stays in `records`/`sources`.
                        return Ok(self.cancelled_outcome(String::new(), sources, records));
                    }

                    let (record, result) = self
                        .registry
                        .execute_observed(
                            &self.context,
                            &call.name,
                            ActionInput(call.arguments),
                            self.confirmation.as_ref(),
                            &self.events,
                            &self.events.next_execution_id(),
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
                cancelled: false,
            });
        }

        let err = OrbitError::AgentIterationLimitReached {
            limit: self.max_iterations,
        };
        self.events.emit(EventPayload::Failure {
            message: err.to_string(),
        });
        Err(err)
    }

    /// One model request, reported as
    /// `ModelResponseStarted` → `ResponseDelta`* → `ModelResponseCompleted`.
    ///
    /// Every iteration of the loop produces one of these, including
    /// iterations whose response only requests tools; the *last* one in a
    /// turn is the one carrying the final answer.
    async fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
        let streaming = self.streaming && self.provider.supports_streaming();
        self.events.emit(EventPayload::ModelResponseStarted {
            model: request.model.clone(),
            streaming,
        });

        let response = if self.streaming {
            let events = self.events.clone();
            let cancel = self.cancel.clone();
            // Returning false stops the provider reading the stream, which
            // is how a cancelled turn interrupts generation mid-answer.
            let handler = move |delta: &str| {
                events.emit(EventPayload::ResponseDelta {
                    text: delta.to_string(),
                });
                !cancel.is_cancelled()
            };
            self.provider.chat_streaming(request, &handler).await?
        } else {
            self.provider.chat(request).await?
        };

        self.events.emit(EventPayload::ModelResponseCompleted {
            text: response.message.content.clone(),
        });
        Ok(response)
    }

    fn cancelled_outcome(
        &self,
        answer: String,
        sources: Vec<SourceReference>,
        records: Vec<ExecutionRecord>,
    ) -> AgentOutcome {
        tracing::debug!("agent run cancelled");
        self.events.emit(EventPayload::ExecutionCancelled {
            reason: "cancelled by user".to_string(),
        });
        AgentOutcome {
            answer,
            sources: dedupe_sources(sources),
            records,
            cancelled: true,
        }
    }
}

/// Normalize a run's collected sources for display, in application code
/// rather than trusting the model to have cited only what it was actually
/// given:
/// - an exact duplicate (same path, line range, section) is dropped;
/// - a path-only ("whole file") reference is dropped when a more precise
///   line-ranged reference to the *same* path also exists -- it adds no
///   information once a specific location is known;
/// - order is otherwise preserved exactly as sources were first
///   encountered (not re-sorted), so the earliest, most directly retrieved
///   evidence stays first.
///
/// This can only ever narrow `sources`, which itself only ever grows from
/// real `ActionOutput::sources` returned by executed actions (see `run`
/// above) -- the model's own answer text never contributes an entry, so it
/// cannot invent a source path that nothing was actually retrieved from.
fn dedupe_sources(sources: Vec<SourceReference>) -> Vec<SourceReference> {
    let has_line_range: std::collections::HashSet<&std::path::PathBuf> = sources
        .iter()
        .filter(|s| s.line_start.is_some())
        .map(|s| &s.path)
        .collect();

    let mut deduped: Vec<SourceReference> = Vec::new();
    for source in &sources {
        if source.line_start.is_none() && has_line_range.contains(&source.path) {
            continue;
        }
        if !deduped.contains(source) {
            deduped.push(source.clone());
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
    async fn builtin_overview_retrieval_can_be_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("README.md"),
            "# Orbit\n\nOrbit is a local-first AI engineering assistant.\n",
        )
        .unwrap();
        let provider = Arc::new(MockProvider::new(vec![orbit_core::ModelResponse {
            message: Message::assistant("no idea"),
            finish_reason: FinishReason::Stop,
        }]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_builtin_overview_retrieval(false);
        let mut history = Vec::new();
        let outcome = agent
            .run(&mut history, "What does this repository do?")
            .await
            .unwrap();
        assert!(
            outcome.sources.is_empty(),
            "retrieval was disabled, so no deterministic sources should appear"
        );
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

    fn line_source(path: &str, line: usize) -> SourceReference {
        SourceReference::lines(std::path::PathBuf::from(path), line, line)
    }

    fn whole_file_source(path: &str) -> SourceReference {
        SourceReference::whole_file(std::path::PathBuf::from(path))
    }

    #[test]
    fn dedupe_sources_drops_exact_duplicates() {
        let sources = vec![line_source("README.md", 3), line_source("README.md", 3)];
        assert_eq!(dedupe_sources(sources), vec![line_source("README.md", 3)]);
    }

    #[test]
    fn dedupe_sources_drops_whole_file_reference_when_a_line_range_exists() {
        let sources = vec![whole_file_source("README.md"), line_source("README.md", 3)];
        assert_eq!(dedupe_sources(sources), vec![line_source("README.md", 3)]);

        // Order of arrival should not matter.
        let sources = vec![line_source("CLAUDE.md", 1), whole_file_source("CLAUDE.md")];
        assert_eq!(dedupe_sources(sources), vec![line_source("CLAUDE.md", 1)]);
    }

    #[test]
    fn dedupe_sources_keeps_whole_file_reference_when_no_line_range_exists_for_it() {
        let sources = vec![whole_file_source("CLAUDE.md"), line_source("README.md", 3)];
        assert_eq!(
            dedupe_sources(sources),
            vec![whole_file_source("CLAUDE.md"), line_source("README.md", 3)]
        );
    }

    #[test]
    fn dedupe_sources_preserves_first_seen_order_across_distinct_paths() {
        let sources = vec![
            line_source("docs/PROJECT_SPEC.md", 5),
            line_source("README.md", 3),
            line_source("docs/OLLAMA.md", 38),
        ];
        assert_eq!(dedupe_sources(sources.clone()), sources);
    }

    /// Regression test: a prior version of the noisy-answer bug let a
    /// generic deterministic-retrieval search surface an incidental
    /// substring match (`Cargo.toml`'s `repository = ".../assistant"`
    /// line) as a "source" for an unrelated question. Sources must only
    /// ever come from what an executed action actually returned, and the
    /// model's own answer text must never add one, no matter what paths
    /// it mentions.
    #[tokio::test]
    async fn model_cannot_invent_a_source_by_mentioning_a_path_in_its_answer() {
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
                message: Message::assistant(
                    "See secret-notes.md and Cargo.toml:99 for more details on the watchdog.",
                ),
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

        assert!(
            outcome
                .sources
                .iter()
                .all(|s| s.path != std::path::Path::new("secret-notes.md")
                    && s.path != std::path::Path::new("Cargo.toml")),
            "the model's answer text must not be able to add sources: {:?}",
            outcome.sources
        );
        assert!(
            outcome
                .sources
                .iter()
                .any(|s| s.path.ends_with("watchdog.md"))
        );
    }
}

/// Event-stream and cancellation behavior of the agent loop itself.
#[cfg(test)]
mod event_tests {
    use super::*;
    use orbit_core::{AlwaysDeny, CollectingSink, EventEmitter, SessionId, ToolCall, TurnId};
    use orbit_project::ProjectConfig;
    use orbit_providers::MockProvider;
    use serde_json::json;

    fn context(root: &std::path::Path) -> Arc<ActionContext> {
        let root = root.canonicalize().unwrap();
        let config = ProjectConfig::parse(
            "version: 1\nproject:\n  name: obc\ncontext:\n  include:\n    - \"**/*\"\n",
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

    fn answer(text: &str) -> orbit_core::ModelResponse {
        orbit_core::ModelResponse {
            message: Message::assistant(text),
            finish_reason: FinishReason::Stop,
        }
    }

    fn search_call() -> orbit_core::ModelResponse {
        orbit_core::ModelResponse {
            message: Message::assistant_tool_calls(vec![ToolCall {
                id: "call_0".to_string(),
                name: "project.search".to_string(),
                arguments: json!({"query": "watchdog"}),
            }]),
            finish_reason: FinishReason::ToolCalls,
        }
    }

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("watchdog.md"),
            "# Watchdog\nresets the system on brownout\n",
        )
        .unwrap();
        tmp
    }

    fn emitter(sink: &Arc<CollectingSink>) -> EventEmitter {
        EventEmitter::new(sink.clone(), SessionId("sess-a".to_string())).with_turn(TurnId::new(1))
    }

    /// The documented per-turn ordering: a tool call is fully reported
    /// (requested → started → source → completed) before the model
    /// response that consumes it completes.
    #[tokio::test]
    async fn a_tool_calling_turn_emits_events_in_the_documented_order() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::new(vec![
            search_call(),
            answer("The watchdog resets the system."),
        ]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(emitter(&sink));

        let mut history = Vec::new();
        let outcome = agent
            .run(&mut history, "what does the watchdog do?")
            .await
            .unwrap();
        assert!(!outcome.cancelled);

        // Consecutive repeats are collapsed so the assertion pins the
        // *ordering*, not how many results the search engine happened to
        // rank (one query can legitimately match a filename and a line).
        let mut shape: Vec<&str> = Vec::new();
        for name in sink.type_names() {
            if shape.last() != Some(&name) {
                shape.push(name);
            }
        }
        assert_eq!(
            shape,
            vec![
                "model_response_started",
                "model_response_completed",
                "action_requested",
                "action_started",
                "source_found",
                "action_completed",
                "model_response_started",
                "model_response_completed",
            ]
        );

        // Every action event must carry the project it ran against.
        let events = sink.events();
        let action_events: Vec<_> = events
            .iter()
            .filter(|e| e.type_name().starts_with("action_") || e.type_name() == "source_found")
            .collect();
        assert!(
            action_events
                .iter()
                .all(|e| e.project.as_deref() == Some("obc"))
        );
        // ...and the turn id, so a UI can group them.
        assert!(events.iter().all(|e| e.turn_id == Some(TurnId::new(1))));
    }

    #[tokio::test]
    async fn streaming_deltas_are_emitted_and_reconstruct_the_answer() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::streaming(vec![answer(
            "STM32 was chosen for low power draw.",
        )]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(emitter(&sink))
        .with_streaming(true);

        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "why STM32?").await.unwrap();

        let deltas: String = sink
            .events()
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ResponseDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, outcome.answer);
        assert_eq!(deltas, "STM32 was chosen for low power draw.");

        // Streaming must be reported as such when the provider supports it.
        match &sink.events()[0].payload {
            EventPayload::ModelResponseStarted { streaming, .. } => assert!(*streaming),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A non-streaming provider must still satisfy the delta contract, so
    /// a UI can render identically regardless of provider capability.
    #[tokio::test]
    async fn a_non_streaming_provider_still_produces_one_delta() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::new(vec![answer("Buffered answer.")]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(emitter(&sink))
        .with_streaming(true);

        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "hello").await.unwrap();

        let deltas: Vec<String> = sink
            .events()
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ResponseDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Buffered answer.".to_string()]);
        assert_eq!(deltas.concat(), outcome.answer);
        match &sink.events()[0].payload {
            EventPayload::ModelResponseStarted { streaming, .. } => {
                assert!(!streaming, "must not claim to stream when it cannot")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancelling_before_the_first_model_request_stops_immediately() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::new(vec![answer("never reached")]));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let agent = Agent::new(
            provider.clone(),
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(emitter(&sink))
        .with_cancellation(cancel);

        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "why STM32?").await.unwrap();

        assert!(outcome.cancelled);
        assert!(outcome.answer.is_empty());
        assert!(
            provider.recorded_requests().is_empty(),
            "a cancelled turn must not reach the provider at all"
        );
        assert_eq!(sink.type_names(), vec!["execution_cancelled"]);
    }

    #[tokio::test]
    async fn cancelling_mid_stream_keeps_the_text_already_delivered() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(
            MockProvider::streaming(vec![answer("aaaabbbbccccdddd")]).with_delta_chunk_chars(4),
        );
        let cancel = CancellationToken::new();

        // Cancel as soon as the first delta is observed.
        struct CancelOnFirst {
            cancel: CancellationToken,
            inner: Arc<CollectingSink>,
        }
        impl orbit_core::EventSink for CancelOnFirst {
            fn emit(&self, event: orbit_core::AgentEvent) {
                if matches!(event.payload, EventPayload::ResponseDelta { .. }) {
                    self.cancel.cancel();
                }
                self.inner.emit(event);
            }
        }

        let events = EventEmitter::new(
            Arc::new(CancelOnFirst {
                cancel: cancel.clone(),
                inner: sink.clone(),
            }),
            SessionId("sess-a".to_string()),
        );
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(events)
        .with_streaming(true)
        .with_cancellation(cancel);

        let mut history = Vec::new();
        let outcome = agent.run(&mut history, "why STM32?").await.unwrap();

        assert!(outcome.cancelled);
        let deltas: String = sink
            .events()
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ResponseDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "aaaa", "generation must stop at the first delta");
        assert_eq!(
            outcome.answer, deltas,
            "a cancelled answer must equal exactly what was streamed"
        );
        assert!(sink.type_names().contains(&"execution_cancelled"));
    }

    /// Cancelling between the model's tool request and its execution must
    /// prevent the action from running at all.
    #[tokio::test]
    async fn cancelling_before_an_action_prevents_it_from_running() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::new(vec![search_call(), answer("unused")]));
        let cancel = CancellationToken::new();

        struct CancelAfterToolRequest {
            cancel: CancellationToken,
            inner: Arc<CollectingSink>,
        }
        impl orbit_core::EventSink for CancelAfterToolRequest {
            fn emit(&self, event: orbit_core::AgentEvent) {
                if matches!(event.payload, EventPayload::ModelResponseCompleted { .. }) {
                    self.cancel.cancel();
                }
                self.inner.emit(event);
            }
        }

        let events = EventEmitter::new(
            Arc::new(CancelAfterToolRequest {
                cancel: cancel.clone(),
                inner: sink.clone(),
            }),
            SessionId("sess-a".to_string()),
        );
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(events)
        .with_cancellation(cancel);

        let mut history = Vec::new();
        let outcome = agent
            .run(&mut history, "what does the watchdog do?")
            .await
            .unwrap();

        assert!(outcome.cancelled);
        let names = sink.type_names();
        assert!(
            !names.contains(&"action_started"),
            "the action must never have started: {names:?}"
        );
        assert!(outcome.records.is_empty());
        assert!(names.contains(&"execution_cancelled"));
    }

    /// Source events must come only from real action output -- a model
    /// naming a path in prose can never produce one.
    #[tokio::test]
    async fn model_prose_cannot_produce_a_source_event() {
        let tmp = fixture();
        let sink = Arc::new(CollectingSink::new());
        let provider = Arc::new(MockProvider::new(vec![
            search_call(),
            answer("Also see secret-notes.md and /etc/passwd for details."),
        ]));
        let agent = Agent::new(
            provider,
            registry(),
            context(tmp.path()),
            Arc::new(AlwaysDeny),
        )
        .with_events(emitter(&sink));

        let mut history = Vec::new();
        agent
            .run(&mut history, "what does the watchdog do?")
            .await
            .unwrap();

        let source_paths: Vec<String> = sink
            .events()
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::SourceFound { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert!(
            source_paths.iter().all(|p| p.contains("watchdog.md")),
            "only real action output may become a source: {source_paths:?}"
        );
    }
}
