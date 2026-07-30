use orbit_core::{OrbitError, ProjectPaths};
use orbit_project::ProjectConfig;

use crate::args::GlobalArgs;

pub struct Loaded {
    pub paths: ProjectPaths,
    pub config: ProjectConfig,
}

/// Resolve the project root and load its configuration, honoring
/// `--config` and `--project` overrides before falling back to searching
/// upward from the current directory.
pub fn resolve_project(global: &GlobalArgs) -> Result<Loaded, OrbitError> {
    let paths = if let Some(config_path) = &global.config {
        let config_path = config_path
            .canonicalize()
            .map_err(|e| OrbitError::io(config_path, e))?;
        let root = config_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| OrbitError::ConfigInvalid {
                path: config_path.clone(),
                reason: "expected a `<root>/.orbit/project.yaml` layout".to_string(),
            })?
            .to_path_buf();
        ProjectPaths { root, config_path }
    } else if let Some(dir) = &global.project {
        orbit_project::project_paths_at(dir)?
    } else {
        let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
        orbit_project::discover_project_root(&cwd)?
    };

    let mut config = ProjectConfig::load(&paths.config_path)?;
    if let Some(model) = &global.model {
        config.model.model = model.clone();
    }
    if let Some(endpoint) = &global.ollama_endpoint {
        config.model.endpoint = endpoint.clone();
    }

    Ok(Loaded { paths, config })
}
