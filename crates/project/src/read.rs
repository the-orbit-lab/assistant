use std::path::Path;

use orbit_core::OrbitError;

use crate::config::ProjectConfig;
use crate::security::{build_glob_set, is_path_allowed, resolve_within_root};

/// Read a single project file as UTF-8 text, enforcing the same security
/// boundary as discovery: inside the root, not excluded, within a size
/// limit. This is the only sanctioned way for an action to read file
/// contents.
pub fn read_allowed_file(
    root: &Path,
    config: &ProjectConfig,
    relative: &Path,
    max_bytes: u64,
) -> Result<String, OrbitError> {
    let absolute = resolve_within_root(root, relative)?;

    if !absolute.is_file() {
        return Err(OrbitError::PathNotFound {
            path: relative.to_path_buf(),
        });
    }

    let relative_from_root = absolute
        .strip_prefix(root)
        .unwrap_or(relative)
        .to_path_buf();

    let includes = build_glob_set(&config.context.include)?;
    let excludes = build_glob_set(&config.effective_excludes())?;
    if !is_path_allowed(&relative_from_root, &includes, &excludes) {
        return Err(OrbitError::PathExcluded {
            path: relative.to_path_buf(),
        });
    }

    let metadata = std::fs::metadata(&absolute).map_err(|e| OrbitError::io(&absolute, e))?;
    if metadata.len() > max_bytes {
        return Err(OrbitError::FileTooLarge {
            path: relative.to_path_buf(),
            size: metadata.len(),
            limit: max_bytes,
        });
    }

    let bytes = std::fs::read(&absolute).map_err(|e| OrbitError::io(&absolute, e))?;
    String::from_utf8(bytes).map_err(|_| OrbitError::NotUtf8Text {
        path: relative.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config() -> ProjectConfig {
        ProjectConfig::parse(
            "version: 1\nproject:\n  name: demo\ncontext:\n  include:\n    - \"**/*\"\n",
        )
        .unwrap()
    }

    #[test]
    fn reads_allowed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("README.md"), "hello world").unwrap();
        let content = read_allowed_file(&root, &config(), Path::new("README.md"), 1024).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn rejects_excluded_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        let err = read_allowed_file(&root, &config(), Path::new(".env"), 1024).unwrap_err();
        assert!(matches!(err, OrbitError::PathExcluded { .. }));
    }

    #[test]
    fn rejects_oversized_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("big.txt"), "x".repeat(100)).unwrap();
        let err = read_allowed_file(&root, &config(), Path::new("big.txt"), 10).unwrap_err();
        assert!(matches!(err, OrbitError::FileTooLarge { .. }));
    }

    #[test]
    fn rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let err =
            read_allowed_file(&root, &config(), Path::new("../outside.txt"), 1024).unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }
}
