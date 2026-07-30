use std::path::{Path, PathBuf};

use orbit_core::{OrbitError, ProjectPaths};

const CONFIG_DIR: &str = ".orbit";
const CONFIG_FILE: &str = "project.yaml";

/// Walk upward from `start` looking for `.orbit/project.yaml`.
///
/// If a `.git` directory boundary is crossed before a configuration is
/// found, and the configuration then turns up outside that boundary, the
/// match is treated as ambiguous and rejected rather than silently used:
/// the caller is very likely inside an unrelated nested repository and a
/// distant ancestor's project configuration would apply the wrong
/// include/exclude/permission rules to it.
pub fn discover_project_root(start: &Path) -> Result<ProjectPaths, OrbitError> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| OrbitError::io(start, e))?
            .join(start)
    };

    let mut git_boundary: Option<PathBuf> = None;
    let mut current: Option<&Path> = Some(start.as_path());

    while let Some(dir) = current {
        let config_path = dir.join(CONFIG_DIR).join(CONFIG_FILE);
        if config_path.is_file() {
            if let Some(boundary) = &git_boundary
                && *boundary != dir
            {
                return Err(OrbitError::AmbiguousProjectRoot {
                    parent: dir.to_path_buf(),
                    child: boundary.clone(),
                });
            }
            let root = dir.canonicalize().map_err(|e| OrbitError::io(dir, e))?;
            return Ok(ProjectPaths {
                config_path: root.join(CONFIG_DIR).join(CONFIG_FILE),
                root,
            });
        }
        if git_boundary.is_none() && dir.join(".git").exists() {
            git_boundary = Some(dir.to_path_buf());
        }
        current = dir.parent();
    }

    Err(OrbitError::ConfigNotFound {
        searched_from: start,
    })
}

/// Load project paths from an explicit, user-supplied project directory
/// rather than searching. Used by `--project <path>`.
pub fn project_paths_at(dir: &Path) -> Result<ProjectPaths, OrbitError> {
    let root = dir.canonicalize().map_err(|e| OrbitError::io(dir, e))?;
    let config_path = root.join(CONFIG_DIR).join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(OrbitError::ConfigNotFound {
            searched_from: root,
        });
    }
    Ok(ProjectPaths { root, config_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_config_in_current_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".orbit")).unwrap();
        fs::write(tmp.path().join(".orbit/project.yaml"), "version: 1\n").unwrap();

        let paths = discover_project_root(tmp.path()).unwrap();
        assert_eq!(paths.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn finds_config_in_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".orbit")).unwrap();
        fs::write(tmp.path().join(".orbit/project.yaml"), "version: 1\n").unwrap();
        let nested = tmp.path().join("src/deep/nested");
        fs::create_dir_all(&nested).unwrap();

        let paths = discover_project_root(&nested).unwrap();
        assert_eq!(paths.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn errors_when_no_config_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = discover_project_root(tmp.path()).unwrap_err();
        assert!(matches!(err, OrbitError::ConfigNotFound { .. }));
    }

    #[test]
    fn rejects_project_outside_git_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".orbit")).unwrap();
        fs::write(tmp.path().join(".orbit/project.yaml"), "version: 1\n").unwrap();

        let nested_repo = tmp.path().join("vendor/other-repo");
        fs::create_dir_all(nested_repo.join(".git")).unwrap();
        let deep = nested_repo.join("src");
        fs::create_dir_all(&deep).unwrap();

        let err = discover_project_root(&deep).unwrap_err();
        assert!(matches!(err, OrbitError::AmbiguousProjectRoot { .. }));
    }
}
