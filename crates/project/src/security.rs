use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSetBuilder};
use orbit_core::OrbitError;

/// Resolve a user- or model-supplied path against the project root,
/// rejecting anything that would escape it.
///
/// This is the single choke point every action must go through before
/// touching the filesystem. It rejects:
/// - absolute paths outside the root,
/// - `..` traversal that would leave the root,
/// - paths whose canonical (symlink-resolved) form falls outside the root.
///
/// The path is not required to exist: callers that only need to check
/// whether a path *would* be safe (e.g. before creating something) can use
/// this too. Existence is checked separately by the caller.
pub fn resolve_within_root(root: &Path, requested: &Path) -> Result<PathBuf, OrbitError> {
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(OrbitError::PathOutsideProject {
                        path: requested.to_path_buf(),
                    });
                }
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }

    if !normalized.starts_with(root) {
        return Err(OrbitError::PathOutsideProject {
            path: requested.to_path_buf(),
        });
    }

    if normalized.exists() {
        let canonical = normalized
            .canonicalize()
            .map_err(|e| OrbitError::io(&normalized, e))?;
        if !canonical.starts_with(root) {
            return Err(OrbitError::SymlinkEscape {
                path: requested.to_path_buf(),
            });
        }
        return Ok(canonical);
    }

    Ok(normalized)
}

/// Build a matcher for a set of glob patterns, relative to the project
/// root. An empty pattern list matches nothing.
pub fn build_glob_set(patterns: &[String]) -> Result<globset::GlobSet, OrbitError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| OrbitError::ConfigInvalid {
            path: PathBuf::new(),
            reason: format!("invalid glob pattern `{pattern}`: {e}"),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| OrbitError::ConfigInvalid {
        path: PathBuf::new(),
        reason: format!("failed to build glob matcher: {e}"),
    })
}

/// Exclude always wins over include: a path must match at least one include
/// pattern and must not match any exclude pattern (mandatory or
/// configured).
pub fn is_path_allowed(
    relative_path: &Path,
    includes: &globset::GlobSet,
    excludes: &globset::GlobSet,
) -> bool {
    if excludes.is_match(relative_path) {
        return false;
    }
    includes.is_match(relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_traversal_past_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let err = resolve_within_root(&root, Path::new("../../etc/passwd")).unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[test]
    fn rejects_absolute_path_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let err = resolve_within_root(&root, Path::new("/etc/passwd")).unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[test]
    fn allows_relative_path_inside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "hi").unwrap();
        let resolved = resolve_within_root(&root, Path::new("a.txt")).unwrap();
        assert_eq!(resolved, root.join("a.txt"));
    }

    #[test]
    fn rejects_traversal_within_a_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let err = resolve_within_root(&root, Path::new("src/../../outside")).unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("link.txt"))
            .unwrap();

        let err = resolve_within_root(&root, Path::new("link.txt")).unwrap_err();
        assert!(matches!(err, OrbitError::SymlinkEscape { .. }));
    }

    #[test]
    fn exclude_takes_precedence_over_include() {
        let includes = build_glob_set(&["**/*.rs".to_string()]).unwrap();
        let excludes = build_glob_set(&["target/**".to_string()]).unwrap();
        assert!(!is_path_allowed(
            Path::new("target/debug/build.rs"),
            &includes,
            &excludes
        ));
        assert!(is_path_allowed(
            Path::new("src/main.rs"),
            &includes,
            &excludes
        ));
    }
}
