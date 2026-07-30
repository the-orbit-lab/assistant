use std::path::Path;
use std::sync::Arc;

use orbit_core::{OrbitError, ProjectPaths};
use orbit_project::ProjectConfig;
use orbit_workspace::{DiscoveredRoot, ProjectRegistry, WorkspaceConfig, WorkspacePaths};

use crate::args::GlobalArgs;

pub struct Loaded {
    pub paths: ProjectPaths,
    pub config: ProjectConfig,
}

/// A value explicitly beginning with `./`, `../`, `/`, or a Windows drive
/// prefix (`C:\`) is always a filesystem path, never a registered project
/// name or alias -- checked before any workspace lookup, so a project
/// literally named the same as a path-shaped string is never ambiguous.
fn looks_like_path(selector: &str) -> bool {
    if selector.starts_with("./") || selector.starts_with("../") || selector.starts_with('/') {
        return true;
    }
    let bytes = selector.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn apply_overrides(config: &mut ProjectConfig, global: &GlobalArgs) {
    if let Some(model) = &global.model {
        config.model.model = model.clone();
    }
    if let Some(endpoint) = &global.ollama_endpoint {
        config.model.endpoint = endpoint.clone();
    }
}

fn finish_loaded(global: &GlobalArgs, paths: ProjectPaths) -> Result<Loaded, OrbitError> {
    tracing::debug!(
        root = %paths.root.display(),
        config_path = %paths.config_path.display(),
        "resolved project"
    );
    let mut config = ProjectConfig::load(&paths.config_path)?;
    apply_overrides(&mut config, global);
    tracing::debug!(
        include = ?config.context.include,
        exclude = ?config.context.exclude,
        "loaded project configuration"
    );
    Ok(Loaded { paths, config })
}

fn load_workspace_registry(paths: WorkspacePaths) -> Result<Arc<ProjectRegistry>, OrbitError> {
    let config = WorkspaceConfig::load(&paths.config_path)?;
    tracing::debug!(
        root = %paths.root.display(),
        config_path = %paths.config_path.display(),
        project_count = config.projects.len(),
        "loaded workspace configuration"
    );
    let registry = ProjectRegistry::load(paths.root, paths.config_path, config)?;
    Ok(Arc::new(registry))
}

fn load_workspace_registry_at(dir: &Path) -> Result<Arc<ProjectRegistry>, OrbitError> {
    load_workspace_registry(orbit_workspace::workspace_paths_at(dir)?)
}

/// Resolve the active workspace: `--workspace` if given, otherwise search
/// upward from the current directory for `.orbit/workspace.yaml`
/// (ignoring any nearer project marker -- this is used only by commands
/// that are explicitly about the workspace itself).
pub fn resolve_workspace(global: &GlobalArgs) -> Result<Arc<ProjectRegistry>, OrbitError> {
    if let Some(dir) = &global.workspace {
        return load_workspace_registry_at(dir);
    }
    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    load_workspace_registry(orbit_workspace::discover_workspace_root(&cwd)?)
}

fn resolve_explicit_project_selector(
    global: &GlobalArgs,
    selector: &str,
) -> Result<Loaded, OrbitError> {
    if looks_like_path(selector) {
        return finish_loaded(
            global,
            orbit_project::project_paths_at(Path::new(selector))?,
        );
    }

    // A bare name/alias always needs an active workspace to resolve
    // against, whether given via --workspace or discovered from cwd.
    let registry = if let Some(dir) = &global.workspace {
        load_workspace_registry_at(dir)?
    } else {
        let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
        match orbit_workspace::discover(&cwd)? {
            DiscoveredRoot::Workspace(paths) => load_workspace_registry(paths)?,
            DiscoveredRoot::Project(_) => {
                // Already inside a plain, non-workspace project: there is
                // nothing to resolve `selector` as a name against.
                return Err(OrbitError::WorkspaceNotFound { searched_from: cwd });
            }
        }
    };

    let entry = registry.resolve_project(selector)?;
    if !entry.available {
        return Err(OrbitError::ProjectUnavailable {
            name: entry.name.clone(),
            reason: entry
                .error
                .clone()
                .unwrap_or_else(|| "project failed to load".to_string()),
        });
    }
    finish_loaded(
        global,
        ProjectPaths {
            root: entry.root.clone(),
            config_path: entry.config_path.clone(),
        },
    )
}

fn project_from_registry_default(
    global: &GlobalArgs,
    registry: &ProjectRegistry,
    strict: bool,
) -> Result<Loaded, OrbitError> {
    let available: Vec<String> = registry
        .list_projects()
        .into_iter()
        .filter(|p| p.available)
        .map(|p| p.name.clone())
        .collect();

    if strict {
        return Err(OrbitError::NoActiveProject { available });
    }
    let Some(default) = registry.default_project() else {
        return Err(OrbitError::NoActiveProject { available });
    };
    if !default.available {
        return Err(OrbitError::ProjectUnavailable {
            name: default.name.clone(),
            reason: default
                .error
                .clone()
                .unwrap_or_else(|| "project failed to load".to_string()),
        });
    }
    finish_loaded(
        global,
        ProjectPaths {
            root: default.root.clone(),
            config_path: default.config_path.clone(),
        },
    )
}

/// Resolve a single project for single-project-shaped commands
/// (`project`, `files`, `commands`, `search`, `ask` without `--projects`,
/// `chat`, `mcp serve`), honoring `--config` and `--project` overrides
/// before falling back to searching upward from the current directory.
///
/// Precedence: explicit `--project` (name/alias/path) > explicit
/// `--workspace` (using its `defaults.project`) > current directory
/// inside a registered project (unchanged single-project behavior) >
/// current directory at/inside a workspace (using its `defaults.project`)
/// > a clear "no project configuration found" error.
///
/// `strict` disables the workspace-default fallback entirely (used by
/// `run`, which must never execute a command in a project the caller
/// didn't select explicitly).
pub fn resolve_project_with_mode(global: &GlobalArgs, strict: bool) -> Result<Loaded, OrbitError> {
    if let Some(config_path) = &global.config {
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
        return finish_loaded(global, ProjectPaths { root, config_path });
    }

    if let Some(selector) = &global.project {
        return resolve_explicit_project_selector(global, selector);
    }

    if let Some(dir) = &global.workspace {
        let registry = load_workspace_registry_at(dir)?;
        return project_from_registry_default(global, &registry, strict);
    }

    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    match orbit_workspace::discover(&cwd)? {
        DiscoveredRoot::Project(paths) => finish_loaded(global, paths),
        DiscoveredRoot::Workspace(paths) => {
            let registry = load_workspace_registry(paths)?;
            project_from_registry_default(global, &registry, strict)
        }
    }
}

pub fn resolve_project(global: &GlobalArgs) -> Result<Loaded, OrbitError> {
    resolve_project_with_mode(global, false)
}
