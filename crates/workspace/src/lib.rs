//! Multi-repository workspace support: an orchestration layer over
//! existing, unchanged project runtimes.
//!
//! A workspace never merges repositories into one filesystem root. Every
//! project keeps its own canonical root, its own `.orbit/project.yaml`
//! (include/exclude, commands, permissions, model, MCP exposure), and its
//! own security boundary; this crate only resolves *which* project(s) a
//! request targets and dispatches to their existing, per-project
//! `ActionContext`/`ActionRegistry`.
//!
//! ```text
//! Workspace Runtime (this crate)
//!     |
//! Project Registry (name/alias resolution, per-project availability)
//!     |
//! One or more selected Project Runtimes (each project's own ActionContext)
//!     |
//! Existing Action Runtime (orbit-actions, unmodified)
//! ```

pub mod budget;
pub mod config;
pub mod discovery;
pub mod native;
pub mod registry;
pub mod retrieval;
pub mod runtime;
pub mod source;

use std::sync::Arc;

use orbit_core::{ConfirmationProvider, OrbitError};

pub use config::{
    Relationship, WorkspaceConfig, WorkspaceDefaults, WorkspaceMeta, WorkspaceProjectEntry,
    normalize_identifier,
};
pub use discovery::{
    DiscoveredRoot, WorkspacePaths, discover, discover_workspace_root, workspace_paths_at,
};
pub use registry::{ProjectEntry, ProjectRegistry};
pub use runtime::WorkspaceRuntime;
pub use source::{WorkspaceSourceReference, dedupe_workspace_sources};

/// Build a ready-to-use `ActionRegistry` containing every `workspace.*`
/// action, bound to `project_registry`. This is the one call CLI/agent/MCP
/// code needs to go from a loaded workspace to something that can execute
/// workspace-scoped requests.
pub fn build_registry(
    project_registry: Arc<ProjectRegistry>,
    confirmation: Arc<dyn ConfirmationProvider>,
) -> Result<orbit_actions::ActionRegistry, OrbitError> {
    let mut registry = orbit_actions::ActionRegistry::new();
    let workspace_runtime = WorkspaceRuntime::new(project_registry, confirmation)?;
    native::register_all(&mut registry, workspace_runtime)?;
    Ok(registry)
}
