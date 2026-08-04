//! The Session Runtime: stateful, multi-turn conversations observed
//! through the Agent Event Stream.
//!
//! This is the layer the CLI renderer, the JSONL bridge, and a future
//! SwiftUI client all sit on top of. It owns conversation state and turn
//! orchestration; it does **not** own agent logic (that is
//! [`orbit_agent::Agent`]) or action logic (that is
//! [`orbit_actions::ActionRegistry`]). Everything a front end needs to
//! display arrives as events, so no front end ever re-derives what
//! happened.
//!
//! State lives in process memory for the life of the runtime and is never
//! written to disk — a conversation is not silently persisted.

use std::sync::Arc;

use orbit_actions::{ActionContext, ActionRegistry};
use orbit_agent::Agent;
use orbit_core::{
    CancellationToken, EVENT_PROTOCOL_VERSION, EventEmitter, EventPayload, EventSink,
    ExecutionRecord, Message, OrbitError, PermissionDecision, PermissionRequestId, SessionId,
    SessionMode, SourceReference, TurnId,
};
use orbit_providers::ModelProvider;
use orbit_workspace::ProjectRegistry;

use crate::permission::{ConfirmationMode, SessionConfirmation};
use crate::topic::TopicState;

/// What a session is scoped to. Chosen once, at construction.
enum Scope {
    SingleProject {
        registry: Arc<ActionRegistry>,
        context: Arc<ActionContext>,
        project: String,
    },
    Workspace {
        /// Registry of the six `workspace.*` actions.
        registry: Arc<ActionRegistry>,
        /// Synthetic workspace-level context for dispatching them.
        context: Arc<ActionContext>,
        projects: Arc<ProjectRegistry>,
    },
}

/// A configured command that actually ran during this session, kept so a
/// front end can show what was executed without re-reading the transcript.
#[derive(Debug, Clone)]
pub struct CommandRun {
    pub project: String,
    pub action: String,
    pub success: bool,
}

/// Whether a turn is currently in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    Idle,
    Running(TurnId),
}

/// Everything the session remembers, in memory only.
pub struct SessionState {
    pub history: Vec<Message>,
    /// What the conversation is currently about, so a follow-up question
    /// can be resolved against it instead of retrieved on its own thin
    /// wording. See [`crate::topic`].
    pub topic: TopicState,
    pub records: Vec<ExecutionRecord>,
    pub sources: Vec<SourceReference>,
    pub command_runs: Vec<CommandRun>,
    pub active_projects: Vec<String>,
    pub turns: u64,
    pub execution: ExecutionState,
}

/// A read-only snapshot for `/status` and for protocol clients.
#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub session_id: SessionId,
    pub mode: SessionMode,
    pub workspace: Option<String>,
    pub active_projects: Vec<String>,
    pub turns: u64,
    pub message_count: usize,
    pub source_count: usize,
    pub action_count: usize,
    pub command_run_count: usize,
    pub execution: ExecutionState,
    pub pending_permissions: Vec<PermissionRequestId>,
    pub streaming: bool,
}

/// The result of one completed turn.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub turn_id: TurnId,
    pub answer: String,
    /// Sources for *this turn only*; the session keeps the cumulative set.
    pub sources: Vec<SourceReference>,
    pub cancelled: bool,
    pub active_projects: Vec<String>,
    /// The scope came from the workspace's `defaults.project` rather than
    /// from an explicit selection or a named project, so a front end can
    /// say so.
    pub used_default_project: bool,
}

/// A stateful conversation. Cheap to share (`Arc`): a front end can hold
/// one reference for running turns and another for cancelling or
/// resolving permissions concurrently.
pub struct SessionRuntime {
    id: SessionId,
    mode: SessionMode,
    workspace_name: Option<String>,
    scope: Scope,
    provider: Arc<dyn ModelProvider>,
    emitter: EventEmitter,
    confirmation: Arc<SessionConfirmation>,
    streaming: bool,
    /// Held for the duration of a turn. Turns are serial by design.
    state: tokio::sync::Mutex<SessionState>,
    /// Separate from `state` on purpose: cancelling and resolving
    /// permissions must work *while* a turn holds the state lock.
    current_cancel: std::sync::Mutex<Option<CancellationToken>>,
    mcp_manager: tokio::sync::Mutex<Option<orbit_agent::McpClientManager>>,
}

impl SessionRuntime {
    /// Start a single-project session, connecting the project's configured
    /// external MCP servers exactly as `orbit ask`/`orbit chat` always
    /// have. Returned warnings are also emitted as `Warning` events.
    pub async fn single_project(
        context: ActionContext,
        provider: Arc<dyn ModelProvider>,
        sink: Arc<dyn EventSink>,
        confirmation_mode: ConfirmationMode,
        streaming: bool,
    ) -> Result<Self, OrbitError> {
        let project = context.config.project.name.clone();
        let (registry, manager, warnings) = orbit_agent::build_registry(&context.config).await?;

        let runtime = Self::build(
            SessionMode::SingleProject,
            None,
            Scope::SingleProject {
                registry: Arc::new(registry),
                context: Arc::new(context),
                project: project.clone(),
            },
            vec![project],
            provider,
            sink,
            confirmation_mode,
            streaming,
            Some(manager),
        );

        for warning in warnings {
            runtime.emitter.emit(EventPayload::Warning {
                message: format!("MCP server `{}`: {}", warning.server, warning.message),
            });
        }
        Ok(runtime)
    }

    /// Start a workspace session. No project is active until one is
    /// selected explicitly or named in a question — a workspace session
    /// never silently starts pointed at a repository.
    pub fn workspace(
        projects: Arc<ProjectRegistry>,
        provider: Arc<dyn ModelProvider>,
        sink: Arc<dyn EventSink>,
        confirmation_mode: ConfirmationMode,
        streaming: bool,
    ) -> Result<Self, OrbitError> {
        let confirmation = Arc::new(SessionConfirmation::new(confirmation_mode));
        let registry = orbit_workspace::build_registry(projects.clone(), confirmation.clone())?;
        let context = projects.workspace_action_context();
        let workspace_name = projects.config.workspace.name.clone();

        Ok(Self::build_with_confirmation(
            SessionMode::Workspace,
            Some(workspace_name),
            Scope::Workspace {
                registry: Arc::new(registry),
                context: Arc::new(context),
                projects,
            },
            Vec::new(),
            provider,
            sink,
            confirmation,
            streaming,
            None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        mode: SessionMode,
        workspace_name: Option<String>,
        scope: Scope,
        active_projects: Vec<String>,
        provider: Arc<dyn ModelProvider>,
        sink: Arc<dyn EventSink>,
        confirmation_mode: ConfirmationMode,
        streaming: bool,
        manager: Option<orbit_agent::McpClientManager>,
    ) -> Self {
        Self::build_with_confirmation(
            mode,
            workspace_name,
            scope,
            active_projects,
            provider,
            sink,
            Arc::new(SessionConfirmation::new(confirmation_mode)),
            streaming,
            manager,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_confirmation(
        mode: SessionMode,
        workspace_name: Option<String>,
        scope: Scope,
        active_projects: Vec<String>,
        provider: Arc<dyn ModelProvider>,
        sink: Arc<dyn EventSink>,
        confirmation: Arc<SessionConfirmation>,
        streaming: bool,
        manager: Option<orbit_agent::McpClientManager>,
    ) -> Self {
        let id = SessionId::generate();
        let emitter = EventEmitter::new(sink, id.clone());

        emitter.emit(EventPayload::SessionStarted {
            protocol_version: EVENT_PROTOCOL_VERSION,
            mode,
            workspace: workspace_name.clone(),
            projects: active_projects.clone(),
        });

        Self {
            id,
            mode,
            workspace_name,
            scope,
            provider,
            emitter,
            confirmation,
            streaming,
            state: tokio::sync::Mutex::new(SessionState {
                history: Vec::new(),
                topic: TopicState::new(),
                records: Vec::new(),
                sources: Vec::new(),
                command_runs: Vec::new(),
                active_projects,
                turns: 0,
                execution: ExecutionState::Idle,
            }),
            current_cancel: std::sync::Mutex::new(None),
            mcp_manager: tokio::sync::Mutex::new(manager),
        }
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    pub fn workspace_name(&self) -> Option<&str> {
        self.workspace_name.as_deref()
    }

    /// The registered projects of the active workspace, or the single
    /// project's own name.
    pub fn known_projects(&self) -> Vec<String> {
        match &self.scope {
            Scope::SingleProject { project, .. } => vec![project.clone()],
            Scope::Workspace { projects, .. } => projects
                .list_projects()
                .into_iter()
                .map(|p| p.name.clone())
                .collect(),
        }
    }

    /// Name, availability, aliases, and load error for every registered
    /// project — what a client needs to render a project picker.
    pub fn project_summaries(&self) -> Vec<(String, bool, Vec<String>, Option<String>)> {
        match &self.scope {
            Scope::SingleProject { project, .. } => {
                vec![(project.clone(), true, Vec::new(), None)]
            }
            Scope::Workspace { projects, .. } => projects
                .list_projects()
                .into_iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        p.available,
                        p.aliases.clone(),
                        p.error.clone(),
                    )
                })
                .collect(),
        }
    }

    pub async fn status(&self) -> SessionStatus {
        let state = self.state.lock().await;
        SessionStatus {
            session_id: self.id.clone(),
            mode: self.mode,
            workspace: self.workspace_name.clone(),
            active_projects: state.active_projects.clone(),
            turns: state.turns,
            message_count: state.history.len(),
            source_count: state.sources.len(),
            action_count: state.records.len(),
            command_run_count: state.command_runs.len(),
            execution: state.execution.clone(),
            pending_permissions: self.confirmation.pending_request_ids(),
            streaming: self.streaming,
        }
    }

    /// Every source this session has collected, in first-seen order.
    /// Every source this session collected, deduplicated.
    ///
    /// A turn contributes sources twice over by design -- deterministic
    /// retrieval records what it read, and the agent records what its own
    /// tool calls returned -- and a multi-turn session revisits the same
    /// files. Without this, `/sources` repeated identical references and
    /// listed a whole file next to the precise lines of it that were
    /// actually quoted.
    ///
    /// Deduplication is by project, path, line range, and section (the
    /// project is part of the encoded path for workspace sources), and a
    /// path-only reference is dropped when a line-ranged reference to the
    /// same file exists -- the precise one is what the answer rests on.
    pub async fn sources(&self) -> Vec<SourceReference> {
        orbit_agent::dedupe_sources(self.state.lock().await.sources.clone())
    }

    /// Replace the active project set.
    ///
    /// Resolution goes through the workspace's deterministic
    /// [`ProjectRegistry`]: an unknown or unavailable name is an error and
    /// leaves the previous selection untouched. Nothing here consults the
    /// model, and a selection is never inferred.
    pub async fn set_active_projects(
        &self,
        selectors: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let Scope::Workspace { projects, .. } = &self.scope else {
            return Err(OrbitError::UnknownProject {
                name: selectors.join(", "),
                available: self.known_projects(),
            });
        };

        let entries = projects.resolve_projects(selectors)?;
        // Resolve first, mutate second: a partially valid list must not
        // leave the session half-switched.
        for entry in &entries {
            if !entry.available {
                return Err(OrbitError::ProjectUnavailable {
                    name: entry.name.clone(),
                    reason: entry
                        .error
                        .clone()
                        .unwrap_or_else(|| "project failed to load".to_string()),
                });
            }
        }
        let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();

        let mut state = self.state.lock().await;
        if state.active_projects != names {
            state.active_projects = names.clone();
            state.topic.set_projects(&names);
            self.emitter.emit(EventPayload::ActiveProjectsChanged {
                projects: names.clone(),
            });
        }
        Ok(names)
    }

    pub async fn active_projects(&self) -> Vec<String> {
        self.state.lock().await.active_projects.clone()
    }

    /// Cancel the turn currently running. Returns `false` when nothing is
    /// running, so a client can distinguish "cancelled" from "nothing to
    /// cancel" rather than being told a no-op succeeded.
    pub fn cancel_current_turn(&self) -> bool {
        let token = self
            .current_cancel
            .lock()
            .expect("cancellation mutex poisoned")
            .clone();
        match token {
            Some(token) => {
                token.cancel();
                // Anything blocked on a permission decision must be
                // released, or the turn would never observe the cancel.
                self.confirmation.cancel_all_pending();
                true
            }
            None => false,
        }
    }

    /// Answer a pending `ask` permission.
    pub fn resolve_permission(
        &self,
        request_id: &PermissionRequestId,
        decision: PermissionDecision,
    ) -> bool {
        self.confirmation.resolve(request_id, decision)
    }

    /// Forget the conversation while keeping the session and its project
    /// selection.
    pub async fn clear(&self) {
        let mut state = self.state.lock().await;
        state.history.clear();
        state.sources.clear();
        state.records.clear();
        state.command_runs.clear();
        state.topic.reset();
    }

    /// End the session, shutting down any external MCP servers it started.
    pub async fn end(&self, reason: &str) {
        if let Some(manager) = self.mcp_manager.lock().await.take() {
            manager.shutdown().await;
        }
        self.emitter.emit(EventPayload::SessionEnded {
            reason: reason.to_string(),
        });
    }

    /// Run one turn: the user's message in, a grounded answer out, with
    /// everything in between reported as events.
    ///
    /// Ordering within a turn is always:
    /// `UserMessageReceived` → retrieval/action/model events →
    /// `ModelResponseCompleted` → `TurnCompleted`. A cancelled turn ends
    /// with `ExecutionCancelled` and no `TurnCompleted`.
    pub async fn send_message(&self, text: &str) -> Result<TurnOutcome, OrbitError> {
        let mut state = self.state.lock().await;

        state.turns += 1;
        let turn_id = TurnId::new(state.turns);
        state.execution = ExecutionState::Running(turn_id.clone());

        let events = self.emitter.with_turn(turn_id.clone());
        events.emit(EventPayload::UserMessageReceived {
            text: text.to_string(),
        });

        let cancel = CancellationToken::new();
        *self
            .current_cancel
            .lock()
            .expect("cancellation mutex poisoned") = Some(cancel.clone());
        self.confirmation.set_cancellation(Some(cancel.clone()));

        let result = self
            .run_turn(&mut state, &turn_id, text, &events, cancel)
            .await;

        state.execution = ExecutionState::Idle;
        *self
            .current_cancel
            .lock()
            .expect("cancellation mutex poisoned") = None;
        self.confirmation.set_cancellation(None);

        match &result {
            Ok(outcome) if !outcome.cancelled => {
                events.emit(EventPayload::TurnCompleted {
                    source_count: outcome.sources.len(),
                    action_count: state.records.len(),
                });
            }
            // A cancelled turn already emitted ExecutionCancelled; a
            // failed one already emitted Failure. Neither completes.
            _ => {}
        }
        result
    }

    async fn run_turn(
        &self,
        state: &mut SessionState,
        _turn_id: &TurnId,
        text: &str,
        events: &EventEmitter,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, OrbitError> {
        let records_before = state.records.len();

        let (outcome, active_projects, used_default) = match &self.scope {
            Scope::SingleProject {
                registry,
                context,
                project,
            } => {
                if state.history.is_empty() {
                    state
                        .history
                        .push(Message::system(orbit_agent::prompt::system_prompt(
                            &context.config.project.name,
                            &context.config.project.description,
                        )));
                }
                state.history.push(Message::user(text));

                let analysis =
                    orbit_project::analyze_with_context(text, &state.topic.context_terms());
                state
                    .topic
                    .observe_question(&analysis.terms, analysis.needs_context);

                let agent = Agent::new(
                    self.provider.clone(),
                    registry.clone(),
                    context.clone(),
                    self.confirmation.clone(),
                )
                .with_events(events.clone().with_project(Some(project.clone())))
                .with_cancellation(cancel)
                .with_streaming(self.streaming)
                .with_context_terms(analysis.context_terms.clone());

                let outcome = agent
                    .continue_from_history(&mut state.history, text)
                    .await?;
                state.sources.extend(outcome.sources.iter().cloned());
                state.topic.observe_sources(&outcome.sources);
                (outcome, vec![project.clone()], false)
            }

            Scope::Workspace {
                registry,
                context,
                projects,
            } => {
                if state.history.is_empty() {
                    state
                        .history
                        .push(Message::system(orbit_agent::prompt::system_prompt(
                            &projects.config.workspace.name,
                            &projects.config.workspace.description,
                        )));
                }
                state.history.push(Message::user(text));

                // A project named in this message joins the active set,
                // so a follow-up like "compare that with docs" broadens
                // the conversation instead of being ignored because a
                // selection was already in effect. This is the same
                // deterministic exact-name/alias scan used everywhere
                // else — never a model decision.
                let mentioned = orbit_workspace::retrieval::find_project_mentions(text, projects);
                let mut changed = false;
                for project in mentioned {
                    if !state.active_projects.contains(&project) {
                        state.active_projects.push(project);
                        changed = true;
                    }
                }
                if changed {
                    events.emit(EventPayload::ActiveProjectsChanged {
                        projects: state.active_projects.clone(),
                    });
                }

                // With a selection in effect, retrieval uses it verbatim.
                // With none, retrieval decides: a workspace-level listing
                // question, or the workspace's `defaults.project`.
                let explicit = if state.active_projects.is_empty() {
                    None
                } else {
                    Some(state.active_projects.clone())
                };

                let analysis =
                    orbit_project::analyze_with_context(text, &state.topic.context_terms());
                state
                    .topic
                    .observe_question(&analysis.terms, analysis.needs_context);
                let context_terms = analysis.context_terms.clone();

                let retrieved = orbit_workspace::retrieval::run(
                    registry,
                    context,
                    projects,
                    self.confirmation.as_ref(),
                    text,
                    explicit.as_deref(),
                    &mut state.history,
                    events,
                    &context_terms,
                )
                .await;
                let confidence = retrieved.confidence();
                let scope = retrieved.scope.clone();
                let declared_symbols = retrieved.declared_symbols;
                let retrieved_sources = retrieved.sources;
                let retrieved_records = retrieved.records;

                // Weak evidence is stated as a trusted instruction rather
                // than left for the model to paper over with general
                // knowledge.
                if confidence.needs_grounding_warning() {
                    state
                        .history
                        .push(Message::system(orbit_agent::prompt::grounding_notice(
                            confidence,
                        )));
                }

                // Retrieval genuinely ran, so its sources belong to the
                // session even if the model call later fails. They are
                // recorded here and deliberately *not* again below, where
                // only the agent's own sources are added -- adding both
                // would list every retrieved source twice (in the session
                // and in `/sources`).
                state.sources.extend(retrieved_sources.iter().cloned());
                state.topic.observe_sources(&retrieved_sources);
                state.records.extend(retrieved_records);

                // A fallback to `defaults.project` deliberately does *not*
                // join the active set: an overview question must never
                // silently pin the session to a repository.

                if cancel.is_cancelled() {
                    events.emit(EventPayload::ExecutionCancelled {
                        reason: "cancelled by user".to_string(),
                    });
                    return Ok(TurnOutcome {
                        turn_id: _turn_id.clone(),
                        answer: String::new(),
                        sources: orbit_agent::dedupe_sources(retrieved_sources),
                        cancelled: true,
                        active_projects: state.active_projects.clone(),
                        used_default_project: scope.used_default,
                    });
                }

                let agent = Agent::new(
                    self.provider.clone(),
                    registry.clone(),
                    context.clone(),
                    self.confirmation.clone(),
                )
                .with_events(events.clone())
                .with_cancellation(cancel)
                .with_streaming(self.streaming)
                // Workspace retrieval already ran above; the agent's own
                // single-project retrieval would only fail here.
                .with_builtin_retrieval(false)
                .with_declared_symbols(declared_symbols);

                let mut outcome = agent
                    .continue_from_history(&mut state.history, text)
                    .await?;
                state.sources.extend(outcome.sources.iter().cloned());
                state.topic.observe_sources(&outcome.sources);
                let mut all = retrieved_sources;
                all.append(&mut outcome.sources);
                outcome.sources = all;

                let active = state.active_projects.clone();
                (outcome, active, scope.used_default)
            }
        };

        // Sources were recorded by each branch above, exactly once each.
        state.records.extend(outcome.records.iter().cloned());

        // Record configured-command executions specifically: a front end
        // shows "what did this session run" without parsing the transcript.
        for record in &state.records[records_before.min(state.records.len())..] {
            if record.action == "command.run_configured" {
                state.command_runs.push(CommandRun {
                    project: active_projects.first().cloned().unwrap_or_default(),
                    action: record.action.clone(),
                    success: record.success,
                });
            }
        }

        Ok(TurnOutcome {
            turn_id: _turn_id.clone(),
            answer: outcome.answer,
            sources: orbit_agent::dedupe_sources(outcome.sources),
            cancelled: outcome.cancelled,
            active_projects,
            used_default_project: used_default,
        })
    }
}
