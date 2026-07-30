use std::path::{Path, PathBuf};

use orbit_core::{OrbitError, ProjectPaths};

const CONFIG_DIR: &str = ".orbit";
const CONFIG_FILE: &str = "workspace.yaml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub config_path: PathBuf,
}

/// What an upward filesystem walk from a directory resolves to: a plain
/// project (today's behavior, unchanged) or a workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredRoot {
    Project(ProjectPaths),
    Workspace(WorkspacePaths),
}

fn absolute(path: &Path) -> Result<PathBuf, OrbitError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| OrbitError::io(path, e))?
            .join(path))
    }
}

/// Walk upward from `start`, checking each directory for
/// `.orbit/project.yaml` before `.orbit/workspace.yaml`. Nearest wins,
/// checking both marker files at the same level before moving up one
/// directory -- so a directory that is itself inside a registered project
/// (which has its own `.orbit/project.yaml`) always resolves as a plain
/// project, exactly as it would with no workspace involved at all. Only
/// once no project marker is found anywhere between `start` and an
/// enclosing `.orbit/workspace.yaml` does workspace mode apply.
pub fn discover(start: &Path) -> Result<DiscoveredRoot, OrbitError> {
    let start = absolute(start)?;
    let mut current: Option<&Path> = Some(start.as_path());

    while let Some(dir) = current {
        if dir.join(CONFIG_DIR).join("project.yaml").is_file() {
            return Ok(DiscoveredRoot::Project(
                orbit_project::discover_project_root(dir)?,
            ));
        }
        if dir.join(CONFIG_DIR).join(CONFIG_FILE).is_file() {
            let root = dir.canonicalize().map_err(|e| OrbitError::io(dir, e))?;
            return Ok(DiscoveredRoot::Workspace(WorkspacePaths {
                config_path: root.join(CONFIG_DIR).join(CONFIG_FILE),
                root,
            }));
        }
        current = dir.parent();
    }

    Err(OrbitError::ConfigNotFound {
        searched_from: start,
    })
}

/// Walk upward from `start` looking only for `.orbit/workspace.yaml`,
/// ignoring any project markers along the way. Used by commands that are
/// explicitly about the workspace itself (`orbit workspace`, `orbit
/// projects`) so they still find the right workspace even when run from
/// inside one of its member projects.
pub fn discover_workspace_root(start: &Path) -> Result<WorkspacePaths, OrbitError> {
    let start = absolute(start)?;
    let mut current: Option<&Path> = Some(start.as_path());

    while let Some(dir) = current {
        let config_path = dir.join(CONFIG_DIR).join(CONFIG_FILE);
        if config_path.is_file() {
            let root = dir.canonicalize().map_err(|e| OrbitError::io(dir, e))?;
            return Ok(WorkspacePaths {
                config_path: root.join(CONFIG_DIR).join(CONFIG_FILE),
                root,
            });
        }
        current = dir.parent();
    }

    Err(OrbitError::WorkspaceNotFound {
        searched_from: start,
    })
}

/// Load workspace paths from an explicit, user-supplied directory rather
/// than searching. Used by `--workspace <path>`.
pub fn workspace_paths_at(dir: &Path) -> Result<WorkspacePaths, OrbitError> {
    let root = dir.canonicalize().map_err(|e| OrbitError::io(dir, e))?;
    let config_path = root.join(CONFIG_DIR).join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(OrbitError::WorkspaceNotFound {
            searched_from: root,
        });
    }
    Ok(WorkspacePaths { root, config_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "version: 1\n").unwrap();
    }

    #[test]
    fn discovers_workspace_at_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join(".orbit/workspace.yaml"));
        let discovered = discover(tmp.path()).unwrap();
        assert!(matches!(discovered, DiscoveredRoot::Workspace(_)));
    }

    #[test]
    fn discovers_workspace_from_a_nested_non_project_directory() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join(".orbit/workspace.yaml"));
        let nested = tmp.path().join("outside/deep");
        fs::create_dir_all(&nested).unwrap();
        let discovered = discover(&nested).unwrap();
        assert!(matches!(discovered, DiscoveredRoot::Workspace(_)));
    }

    #[test]
    fn a_project_inside_a_workspace_still_resolves_as_a_plain_project() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join(".orbit/workspace.yaml"));
        let project_dir = tmp.path().join("obc");
        touch(&project_dir.join(".orbit/project.yaml"));
        let nested = project_dir.join("src");
        fs::create_dir_all(&nested).unwrap();

        let discovered = discover(&nested).unwrap();
        match discovered {
            DiscoveredRoot::Project(paths) => {
                assert_eq!(paths.root, project_dir.canonicalize().unwrap());
            }
            DiscoveredRoot::Workspace(_) => panic!("expected project mode, got workspace mode"),
        }
    }

    #[test]
    fn discover_workspace_root_ignores_a_nearer_project_marker() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join(".orbit/workspace.yaml"));
        let project_dir = tmp.path().join("obc");
        touch(&project_dir.join(".orbit/project.yaml"));

        let paths = discover_workspace_root(&project_dir).unwrap();
        assert_eq!(paths.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn errors_when_nothing_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            discover(tmp.path()),
            Err(OrbitError::ConfigNotFound { .. })
        ));
        assert!(matches!(
            discover_workspace_root(tmp.path()),
            Err(OrbitError::WorkspaceNotFound { .. })
        ));
    }
}
