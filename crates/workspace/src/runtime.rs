use std::sync::Arc;

use orbit_actions::ActionRegistry;
use orbit_core::{ActionInput, ActionOutput, ConfirmationProvider, OrbitError};
use serde_json::Value;

use crate::registry::{ProjectEntry, ProjectRegistry};

/// Shared dependencies every `workspace.*` action needs: the resolved
/// project directory, the (stateless, shared) native Action Registry used
/// to actually run `project.*` actions, and a confirmation provider for
/// whatever inner per-project permission checks come up. Constructed once
/// per session and cloned cheaply (it's all `Arc`s) into each action.
#[derive(Clone)]
pub struct WorkspaceRuntime {
    pub project_registry: Arc<ProjectRegistry>,
    pub native_registry: Arc<ActionRegistry>,
    pub confirmation: Arc<dyn ConfirmationProvider>,
}

impl WorkspaceRuntime {
    pub fn new(
        project_registry: Arc<ProjectRegistry>,
        confirmation: Arc<dyn ConfirmationProvider>,
    ) -> Result<Self, OrbitError> {
        Ok(Self {
            project_registry,
            native_registry: Arc::new(orbit_actions::native_registry()?),
            confirmation,
        })
    }

    /// Resolve `selector` and require the resulting project to actually be
    /// available -- the one place every workspace action goes through
    /// before touching a project, so "unknown project" and "unavailable
    /// project" are always reported the same way.
    pub fn require_available(&self, selector: &str) -> Result<&ProjectEntry, OrbitError> {
        let entry = self.project_registry.resolve_project(selector)?;
        if !entry.available {
            return Err(OrbitError::ProjectUnavailable {
                name: entry.name.clone(),
                reason: entry
                    .error
                    .clone()
                    .unwrap_or_else(|| "project failed to load".to_string()),
            });
        }
        Ok(entry)
    }

    /// Run one native project action (`project.search`, `project.read_file`,
    /// ...) against a single, already-available project entry -- the exact
    /// same `ActionRegistry::execute` path a single-project CLI command or
    /// the agent would use, with that project's own `ActionContext` and
    /// therefore its own permissions, include/exclude rules, and security
    /// boundary. Nothing about this call can reach any other project.
    pub async fn call_project_action(
        &self,
        entry: &ProjectEntry,
        action_name: &str,
        input: Value,
    ) -> Result<ActionOutput, OrbitError> {
        let ctx = entry.action_context()?;
        let (_, result) = self
            .native_registry
            .execute(
                &ctx,
                action_name,
                ActionInput(input),
                self.confirmation.as_ref(),
            )
            .await;
        result
    }
}
