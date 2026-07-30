use orbit_actions::ActionRegistry;
use orbit_core::OrbitError;
use orbit_mcp_client::{McpClientManager, McpConnectionWarning};
use orbit_project::ProjectConfig;

/// Assemble the full Action Registry an agent session uses: every native
/// action, plus every tool exposed by the project's configured external
/// MCP servers, namespaced `mcp.<server>.<tool>`. A server that fails to
/// connect only produces a warning — the rest of the session still works.
pub async fn build_registry(
    config: &ProjectConfig,
) -> Result<(ActionRegistry, McpClientManager, Vec<McpConnectionWarning>), OrbitError> {
    let mut registry = orbit_actions::native_registry()?;
    let mut manager = McpClientManager::new();
    let (external_actions, mut warnings) = manager.connect_all(&config.mcp).await;

    for action in external_actions {
        let name = action.descriptor().name;
        if let Err(err) = registry.register(action) {
            warnings.push(McpConnectionWarning {
                server: name,
                message: err.to_string(),
            });
        }
    }

    Ok((registry, manager, warnings))
}
