pub mod ask;
pub mod chat;
pub mod doctor;
pub mod files;
pub mod init;
pub mod list_commands;
pub mod mcp_serve;
pub mod project;
pub mod projects;
pub mod run;
pub mod search;
pub mod workspace;
pub mod workspace_init;

use orbit_core::OrbitError;
use orbit_workspace::DiscoveredRoot;

/// What the current directory resolves to, using the ordinary discovery
/// precedence (nearest `.orbit/project.yaml` wins over an enclosing
/// `.orbit/workspace.yaml`). Shared by every command that has to choose
/// between single-project and workspace behavior.
pub fn discover_root() -> Result<DiscoveredRoot, OrbitError> {
    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    orbit_workspace::discover(&cwd)
}
