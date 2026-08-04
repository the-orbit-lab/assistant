use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_core::{
    ActionDescriptor, ActionInput, ActionOutput, ConfirmationProvider, ConfirmationRequest,
    EventEmitter, EventPayload, ExecutionId, ExecutionRecord, OrbitError, Permission,
    PermissionDecision, PermissionOutcome, PermissionRequestId,
};
use orbit_project::ProjectConfig;
use serde_json::Value;

/// Everything a native action needs to run: the project's security
/// boundary and its validated configuration.
pub struct ActionContext {
    /// Must be absolute and canonicalized (symlinks resolved), matching
    /// [`orbit_core::ProjectPaths::root`]. Path-resolving actions compare a
    /// canonicalized candidate path against this field to catch symlink
    /// escapes; a non-canonical root (e.g. `/tmp/x` on macOS, where `/tmp`
    /// itself is a symlink to `/private/tmp`) makes that comparison fail
    /// for every request, not just malicious ones.
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: ProjectConfig,
}

impl ActionContext {
    /// The project this context represents, or `None` when it is the
    /// synthetic workspace-level context.
    ///
    /// A workspace-scoped action (`workspace.search`, ...) may touch
    /// several projects in one call, so there is no single project to
    /// attribute it to; per-project identity arrives on the `SourceFound`
    /// events instead. Reporting the workspace's own name here would look
    /// like a project that does not exist.
    pub fn project_identity(&self) -> Option<&str> {
        if self.config.project.project_type == orbit_project::config::WORKSPACE_PROJECT_TYPE {
            None
        } else {
            Some(&self.config.project.name)
        }
    }
}

/// A single Action Runtime capability: a stable name, a schema, a
/// permission requirement, and an implementation. Implemented by every
/// native Orbit action and, on the consuming side, by the adapter that
/// wraps an external MCP tool.
#[async_trait::async_trait]
pub trait Action: Send + Sync {
    fn descriptor(&self) -> ActionDescriptor;

    /// Typed validation of raw JSON input, run before any permission check
    /// so malformed requests never trigger a confirmation prompt.
    fn validate(&self, input: &Value) -> Result<(), OrbitError>;

    async fn execute(
        &self,
        ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError>;
}

/// Registers actions by name, resolves them, validates input, enforces
/// permissions, and executes them — the single path every caller (CLI,
/// agent, MCP server) goes through to run an action.
#[derive(Default)]
pub struct ActionRegistry {
    actions: HashMap<String, Arc<dyn Action>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, action: Arc<dyn Action>) -> Result<(), OrbitError> {
        let name = action.descriptor().name;
        if self.actions.contains_key(&name) {
            return Err(OrbitError::DuplicateAction { name });
        }
        self.actions.insert(name, action);
        Ok(())
    }

    pub fn descriptors(&self) -> Vec<ActionDescriptor> {
        let mut list: Vec<_> = self.actions.values().map(|a| a.descriptor()).collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Action>> {
        self.actions.get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.actions.contains_key(name)
    }

    /// Validate input, enforce the effective permission, and execute the
    /// named action. Always returns an [`ExecutionRecord`] alongside the
    /// result so callers can build session/audit history without
    /// re-deriving timing or permission outcome themselves.
    pub async fn execute(
        &self,
        ctx: &ActionContext,
        name: &str,
        input: ActionInput,
        confirmation: &dyn ConfirmationProvider,
    ) -> (ExecutionRecord, Result<ActionOutput, OrbitError>) {
        self.execute_observed(
            ctx,
            name,
            input,
            confirmation,
            &EventEmitter::null(),
            &ExecutionId::new(0),
        )
        .await
    }

    /// [`ActionRegistry::execute`], reporting progress to `events` as it
    /// goes.
    ///
    /// This is the *same* execution path — [`ActionRegistry::execute`] is a
    /// thin wrapper that passes a null emitter — so observing a session can
    /// never diverge from running one. Events live here rather than in the
    /// caller because this is the only place that knows *when* validation
    /// passed, *when* a permission check resolved, and therefore when the
    /// action genuinely started:
    ///
    /// ```text
    /// ActionRequested
    ///   → (validation)
    ///   → PermissionRequired + PermissionResolved, only for `ask`
    ///   → ActionStarted
    ///   → SourceFound per returned source
    ///   → ActionCompleted | ActionFailed
    /// ```
    ///
    /// A rejected input, a `deny`, or a refused confirmation short-circuits
    /// to `ActionFailed` without ever emitting `ActionStarted`, so an
    /// `ActionStarted` event always means the action really ran.
    pub async fn execute_observed(
        &self,
        ctx: &ActionContext,
        name: &str,
        input: ActionInput,
        confirmation: &dyn ConfirmationProvider,
        events: &EventEmitter,
        execution_id: &ExecutionId,
    ) -> (ExecutionRecord, Result<ActionOutput, OrbitError>) {
        let started_at = SystemTime::now();
        tracing::debug!(action = name, "executing action");

        let project = ctx.project_identity().map(str::to_string);
        // Built once and reused: it is the safe, redacted rendering shown
        // in both the request event and any permission prompt.
        let arguments_summary = if events.is_enabled() {
            orbit_core::summarize_arguments(&input.0)
        } else {
            String::new()
        };
        events.emit_execution_project(
            execution_id,
            project.as_deref(),
            EventPayload::ActionRequested {
                action: name.to_string(),
                arguments: arguments_summary.clone(),
            },
        );

        let fail = |outcome, err: OrbitError| {
            events.emit_execution_project(
                execution_id,
                project.as_deref(),
                EventPayload::ActionFailed {
                    action: name.to_string(),
                    error: err.to_string(),
                },
            );
            Self::finish(name, outcome, started_at, Err(err))
        };

        let Some(action) = self.get(name) else {
            return fail(
                PermissionOutcome::NotApplicable,
                OrbitError::UnknownAction {
                    name: name.to_string(),
                },
            );
        };

        if let Err(err) = action.validate(&input.0) {
            return fail(PermissionOutcome::NotApplicable, err);
        }

        let descriptor = action.descriptor();
        let effective = ctx
            .config
            .effective_permission(name, descriptor.default_permission);

        let permission_outcome = match effective {
            Permission::Deny => {
                return fail(
                    PermissionOutcome::DeniedByConfig,
                    OrbitError::PermissionDenied {
                        name: name.to_string(),
                        permission: effective.to_string(),
                    },
                );
            }
            Permission::Ask => {
                // The summary is for display/audit only (it redacts
                // secret-shaped values and shortens absolute paths); the
                // action still receives the original `input` untouched.
                let request = ConfirmationRequest {
                    request_id: PermissionRequestId::generate(),
                    action: name.to_string(),
                    description: descriptor.description.clone(),
                    project: project.clone(),
                    arguments_summary: arguments_summary.clone(),
                };
                events.emit_execution_project(
                    execution_id,
                    project.as_deref(),
                    EventPayload::PermissionRequired {
                        request_id: request.request_id.clone(),
                        action: name.to_string(),
                        description: descriptor.description.clone(),
                        arguments: arguments_summary.clone(),
                    },
                );

                let approved = confirmation.confirm(&request).await;

                events.emit_execution_project(
                    execution_id,
                    project.as_deref(),
                    EventPayload::PermissionResolved {
                        request_id: request.request_id.clone(),
                        decision: if approved {
                            PermissionDecision::AllowOnce
                        } else {
                            PermissionDecision::DenyOnce
                        },
                    },
                );

                if approved {
                    PermissionOutcome::ConfirmedByUser
                } else {
                    return fail(
                        PermissionOutcome::DeniedByUser,
                        OrbitError::ConfirmationDenied {
                            name: name.to_string(),
                        },
                    );
                }
            }
            Permission::Allow => PermissionOutcome::Allowed,
        };

        events.emit_execution_project(
            execution_id,
            project.as_deref(),
            EventPayload::ActionStarted {
                action: name.to_string(),
            },
        );

        let result = action.execute(ctx, input).await;
        let (record, result) = Self::finish(name, permission_outcome, started_at, result);

        match &result {
            Ok(output) => {
                // Sources come only from what the action actually
                // returned. Nothing downstream can add one.
                for source in &output.sources {
                    events.emit_source(execution_id, project.as_deref(), source);
                }
                events.emit_execution_project(
                    execution_id,
                    project.as_deref(),
                    EventPayload::ActionCompleted {
                        action: name.to_string(),
                        duration_ms: record.duration().as_millis() as u64,
                        source_count: output.sources.len(),
                    },
                );
            }
            Err(err) => events.emit_execution_project(
                execution_id,
                project.as_deref(),
                EventPayload::ActionFailed {
                    action: name.to_string(),
                    error: err.to_string(),
                },
            ),
        }

        (record, result)
    }

    fn finish(
        name: &str,
        permission_outcome: PermissionOutcome,
        started_at: SystemTime,
        result: Result<ActionOutput, OrbitError>,
    ) -> (ExecutionRecord, Result<ActionOutput, OrbitError>) {
        let finished_at = SystemTime::now();
        let success = result.is_ok();
        let record = ExecutionRecord {
            action: name.to_string(),
            permission_outcome,
            started_at,
            finished_at,
            success,
            error_summary: result.as_ref().err().map(|e| e.to_string()),
        };
        tracing::debug!(
            action = name,
            success,
            permission = ?permission_outcome,
            duration_ms = record.duration().as_millis() as u64,
            error = record.error_summary.as_deref(),
            "action finished"
        );
        (record, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::{AlwaysAllow, AlwaysDeny};
    use serde_json::json;

    struct EchoAction {
        permission: Permission,
        requires_field: bool,
    }

    #[async_trait::async_trait]
    impl Action for EchoAction {
        fn descriptor(&self) -> ActionDescriptor {
            ActionDescriptor {
                name: "test.echo".to_string(),
                description: "echoes input".to_string(),
                input_schema: json!({"type": "object"}),
                default_permission: self.permission,
            }
        }

        fn validate(&self, input: &Value) -> Result<(), OrbitError> {
            if self.requires_field && input.get("value").is_none() {
                return Err(OrbitError::InvalidActionInput {
                    name: "test.echo".to_string(),
                    reason: "missing `value`".to_string(),
                });
            }
            Ok(())
        }

        async fn execute(
            &self,
            _ctx: &ActionContext,
            input: ActionInput,
        ) -> Result<ActionOutput, OrbitError> {
            Ok(ActionOutput::new(input.0))
        }
    }

    fn ctx() -> ActionContext {
        ActionContext {
            root: std::path::PathBuf::from("/tmp"),
            config_path: std::path::PathBuf::from("/tmp/.orbit/project.yaml"),
            config: ProjectConfig::parse("version: 1\nproject:\n  name: demo\n").unwrap(),
        }
    }

    #[test]
    fn rejects_duplicate_registration() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Allow,
                requires_field: false,
            }))
            .unwrap();
        let err = registry
            .register(Arc::new(EchoAction {
                permission: Permission::Allow,
                requires_field: false,
            }))
            .unwrap_err();
        assert!(matches!(err, OrbitError::DuplicateAction { .. }));
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let registry = ActionRegistry::new();
        let (record, result) = registry
            .execute(&ctx(), "does.not.exist", ActionInput::empty(), &AlwaysDeny)
            .await;
        assert!(matches!(result, Err(OrbitError::UnknownAction { .. })));
        assert_eq!(record.permission_outcome, PermissionOutcome::NotApplicable);
        assert!(!record.success);
    }

    #[tokio::test]
    async fn malformed_input_is_rejected_before_permission_check() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Deny,
                requires_field: true,
            }))
            .unwrap();
        let (record, result) = registry
            .execute(&ctx(), "test.echo", ActionInput::empty(), &AlwaysDeny)
            .await;
        assert!(matches!(result, Err(OrbitError::InvalidActionInput { .. })));
        assert_eq!(record.permission_outcome, PermissionOutcome::NotApplicable);
    }

    #[tokio::test]
    async fn allow_permission_executes() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Allow,
                requires_field: false,
            }))
            .unwrap();
        let (record, result) = registry
            .execute(&ctx(), "test.echo", ActionInput::empty(), &AlwaysDeny)
            .await;
        assert!(result.is_ok());
        assert_eq!(record.permission_outcome, PermissionOutcome::Allowed);
    }

    #[tokio::test]
    async fn deny_permission_blocks_execution() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Deny,
                requires_field: false,
            }))
            .unwrap();
        let (record, result) = registry
            .execute(&ctx(), "test.echo", ActionInput::empty(), &AlwaysAllow)
            .await;
        assert!(matches!(result, Err(OrbitError::PermissionDenied { .. })));
        assert_eq!(record.permission_outcome, PermissionOutcome::DeniedByConfig);
    }

    #[tokio::test]
    async fn ask_permission_honors_confirmation_provider() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Ask,
                requires_field: false,
            }))
            .unwrap();

        let (record, result) = registry
            .execute(&ctx(), "test.echo", ActionInput::empty(), &AlwaysAllow)
            .await;
        assert!(result.is_ok());
        assert_eq!(
            record.permission_outcome,
            PermissionOutcome::ConfirmedByUser
        );

        let (record, result) = registry
            .execute(&ctx(), "test.echo", ActionInput::empty(), &AlwaysDeny)
            .await;
        assert!(matches!(result, Err(OrbitError::ConfirmationDenied { .. })));
        assert_eq!(record.permission_outcome, PermissionOutcome::DeniedByUser);
    }

    #[test]
    fn descriptors_are_sorted_by_name() {
        let mut registry = ActionRegistry::new();
        registry
            .register(Arc::new(EchoAction {
                permission: Permission::Allow,
                requires_field: false,
            }))
            .unwrap();
        let names: Vec<_> = registry.descriptors().into_iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use orbit_core::{
        AlwaysAllow, AlwaysDeny, CollectingSink, EventEmitter, SessionId, SourceReference,
    };
    use serde_json::json;

    struct SourceAction {
        permission: Permission,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl Action for SourceAction {
        fn descriptor(&self) -> ActionDescriptor {
            ActionDescriptor {
                name: "test.sourced".to_string(),
                description: "returns a source".to_string(),
                input_schema: json!({"type": "object"}),
                default_permission: self.permission,
            }
        }

        fn validate(&self, _input: &Value) -> Result<(), OrbitError> {
            Ok(())
        }

        async fn execute(
            &self,
            _ctx: &ActionContext,
            _input: ActionInput,
        ) -> Result<ActionOutput, OrbitError> {
            if self.fail {
                return Err(OrbitError::PathNotFound {
                    path: PathBuf::from("missing.md"),
                });
            }
            Ok(
                ActionOutput::new(json!({"ok": true})).with_sources(vec![SourceReference::lines(
                    PathBuf::from("docs/watchdog.md"),
                    3,
                    9,
                )]),
            )
        }
    }

    fn ctx_named(name: &str) -> ActionContext {
        ActionContext {
            root: PathBuf::from("/tmp"),
            config_path: PathBuf::from("/tmp/.orbit/project.yaml"),
            config: ProjectConfig::parse(&format!("version: 1\nproject:\n  name: {name}\n"))
                .unwrap(),
        }
    }

    fn registry_with(action: SourceAction) -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        registry.register(Arc::new(action)).unwrap();
        registry
    }

    async fn run(
        action: SourceAction,
        confirmation: &dyn ConfirmationProvider,
    ) -> (Vec<&'static str>, Arc<CollectingSink>) {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink.clone(), SessionId("sess-t".to_string()));
        let execution = emitter.next_execution_id();
        let registry = registry_with(action);
        let _ = registry
            .execute_observed(
                &ctx_named("obc"),
                "test.sourced",
                ActionInput::empty(),
                confirmation,
                &emitter,
                &execution,
            )
            .await;
        (sink.type_names(), sink)
    }

    #[tokio::test]
    async fn an_allowed_action_emits_requested_started_source_then_completed() {
        let (names, sink) = run(
            SourceAction {
                permission: Permission::Allow,
                fail: false,
            },
            &AlwaysDeny,
        )
        .await;

        assert_eq!(
            names,
            vec![
                "action_requested",
                "action_started",
                "source_found",
                "action_completed"
            ]
        );
        // Every event carries the project it ran against.
        assert!(
            sink.events()
                .iter()
                .all(|e| e.project.as_deref() == Some("obc"))
        );
        // And they all share one execution id.
        let ids: std::collections::HashSet<_> = sink
            .events()
            .iter()
            .map(|e| e.execution_id.clone())
            .collect();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn an_ask_action_emits_permission_events_around_the_check() {
        let (names, sink) = run(
            SourceAction {
                permission: Permission::Ask,
                fail: false,
            },
            &AlwaysAllow,
        )
        .await;

        assert_eq!(
            names,
            vec![
                "action_requested",
                "permission_required",
                "permission_resolved",
                "action_started",
                "source_found",
                "action_completed"
            ]
        );

        // The request id in PermissionRequired must be the one resolved.
        let events = sink.events();
        let required = match &events[1].payload {
            EventPayload::PermissionRequired { request_id, .. } => request_id.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        match &events[2].payload {
            EventPayload::PermissionResolved {
                request_id,
                decision,
            } => {
                assert_eq!(request_id, &required);
                assert_eq!(*decision, PermissionDecision::AllowOnce);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A denied confirmation must never produce ActionStarted -- that
    /// event is the signal that the action really ran.
    #[tokio::test]
    async fn a_denied_confirmation_never_emits_action_started() {
        let (names, sink) = run(
            SourceAction {
                permission: Permission::Ask,
                fail: false,
            },
            &AlwaysDeny,
        )
        .await;

        assert_eq!(
            names,
            vec![
                "action_requested",
                "permission_required",
                "permission_resolved",
                "action_failed"
            ]
        );
        assert!(!names.contains(&"action_started"));
        match &sink.events()[2].payload {
            EventPayload::PermissionResolved { decision, .. } => {
                assert_eq!(*decision, PermissionDecision::DenyOnce)
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_config_denied_action_fails_without_starting_or_prompting() {
        let (names, _) = run(
            SourceAction {
                permission: Permission::Deny,
                fail: false,
            },
            &AlwaysAllow,
        )
        .await;
        assert_eq!(names, vec!["action_requested", "action_failed"]);
    }

    #[tokio::test]
    async fn a_failing_action_emits_started_then_failed_and_no_sources() {
        let (names, _) = run(
            SourceAction {
                permission: Permission::Allow,
                fail: true,
            },
            &AlwaysDeny,
        )
        .await;
        assert_eq!(
            names,
            vec!["action_requested", "action_started", "action_failed"]
        );
    }

    #[tokio::test]
    async fn an_unknown_action_fails_before_any_permission_check() {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink.clone(), SessionId("sess-t".to_string()));
        let execution = emitter.next_execution_id();
        let _ = ActionRegistry::new()
            .execute_observed(
                &ctx_named("obc"),
                "does.not.exist",
                ActionInput::empty(),
                &AlwaysAllow,
                &emitter,
                &execution,
            )
            .await;
        assert_eq!(sink.type_names(), vec!["action_requested", "action_failed"]);
    }

    /// `execute` must behave identically to `execute_observed`; it is the
    /// same path with a null emitter, and nothing may depend on events.
    #[tokio::test]
    async fn execute_without_events_produces_the_same_outcome() {
        let registry = registry_with(SourceAction {
            permission: Permission::Allow,
            fail: false,
        });
        let (record, result) = registry
            .execute(
                &ctx_named("obc"),
                "test.sourced",
                ActionInput::empty(),
                &AlwaysDeny,
            )
            .await;
        assert!(record.success);
        assert_eq!(result.unwrap().sources.len(), 1);
    }
}
