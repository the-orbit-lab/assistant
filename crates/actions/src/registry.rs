use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use orbit_core::{
    ActionDescriptor, ActionInput, ActionOutput, ConfirmationProvider, ConfirmationRequest,
    ExecutionRecord, OrbitError, Permission, PermissionOutcome,
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
        let started_at = SystemTime::now();
        tracing::debug!(action = name, "executing action");

        let Some(action) = self.get(name) else {
            let err = OrbitError::UnknownAction {
                name: name.to_string(),
            };
            return Self::finish(name, PermissionOutcome::NotApplicable, started_at, Err(err));
        };

        if let Err(err) = action.validate(&input.0) {
            return Self::finish(name, PermissionOutcome::NotApplicable, started_at, Err(err));
        }

        let descriptor = action.descriptor();
        let effective = ctx
            .config
            .effective_permission(name, descriptor.default_permission);

        let permission_outcome = match effective {
            Permission::Deny => {
                let err = OrbitError::PermissionDenied {
                    name: name.to_string(),
                    permission: effective.to_string(),
                };
                return Self::finish(
                    name,
                    PermissionOutcome::DeniedByConfig,
                    started_at,
                    Err(err),
                );
            }
            Permission::Ask => {
                let request = ConfirmationRequest {
                    action: name.to_string(),
                    description: descriptor.description.clone(),
                };
                if confirmation.confirm(&request) {
                    PermissionOutcome::ConfirmedByUser
                } else {
                    let err = OrbitError::ConfirmationDenied {
                        name: name.to_string(),
                    };
                    return Self::finish(
                        name,
                        PermissionOutcome::DeniedByUser,
                        started_at,
                        Err(err),
                    );
                }
            }
            Permission::Allow => PermissionOutcome::Allowed,
        };

        let result = action.execute(ctx, input).await;
        Self::finish(name, permission_outcome, started_at, result)
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
