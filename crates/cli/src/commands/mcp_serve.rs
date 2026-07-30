use orbit_core::OrbitError;
use orbit_mcp_server::OrbitMcpServer;

use crate::args::GlobalArgs;
use crate::resolve::resolve_project;
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
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
        "Orbit MCP server starting on stdio (project config: {})...",
        config_path.display()
    );
    server.serve_stdio().await
}
