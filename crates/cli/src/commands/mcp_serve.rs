use std::sync::Arc;

use orbit_core::{AlwaysDeny, OrbitError};
use orbit_mcp_server::OrbitMcpServer;
use orbit_workspace::DiscoveredRoot;

use crate::args::GlobalArgs;
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let use_workspace = global.workspace.is_some()
        || (global.project.is_none() && matches!(discover_root()?, DiscoveredRoot::Workspace(_)));

    if use_workspace {
        serve_workspace(global).await
    } else {
        serve_single_project(global).await
    }
}

fn discover_root() -> Result<DiscoveredRoot, OrbitError> {
    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    orbit_workspace::discover(&cwd)
}

async fn serve_single_project(global: &GlobalArgs) -> Result<(), OrbitError> {
    let loaded = resolve_project(global)?;
    let expose = loaded.config.mcp.expose.clone();
    let config_path = loaded.paths.config_path.clone();
    if expose.is_empty() {
        eprintln!(
            "Warning: mcp.expose is empty in {}; no actions will be available over MCP.",
            config_path.display()
        );
    }

    let registry = orbit_actions::native_registry()?;
    let ctx = build_context(loaded);
    let server = OrbitMcpServer::new(registry, ctx, expose);

    // Every diagnostic here goes to stderr -- stdout is reserved for MCP
    // protocol frames from this point on.
    for warning in server.exposure_warnings() {
        eprintln!("Warning: {}", warning.message);
    }
    eprintln!(
        "Orbit MCP server starting on stdio (single-project mode, config: {})...",
        config_path.display()
    );
    server.serve_stdio().await
}

/// Workspace mode always exposes exactly the six `workspace.*` actions --
/// never one dynamically generated tool per registered repository (see
/// docs/WORKSPACES.md). `OrbitMcpServer` itself needs no workspace-specific
/// code: it's the same exposure/permission/protocol machinery operating on
/// a workspace-scoped `ActionRegistry` and a synthetic workspace-level
/// `ActionContext` instead of a single project's.
async fn serve_workspace(global: &GlobalArgs) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    let registry = orbit_workspace::build_registry(project_registry.clone(), Arc::new(AlwaysDeny))?;
    let expose: Vec<String> = registry.descriptors().into_iter().map(|d| d.name).collect();
    let ctx = project_registry.workspace_action_context();
    let config_path = project_registry.workspace_config_path.clone();

    let server = OrbitMcpServer::new(registry, ctx, expose);
    for warning in server.exposure_warnings() {
        eprintln!("Warning: {}", warning.message);
    }
    eprintln!(
        "Orbit MCP server starting on stdio (workspace mode, config: {})...",
        config_path.display()
    );
    server.serve_stdio().await
}
