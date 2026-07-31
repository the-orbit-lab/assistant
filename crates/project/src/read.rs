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
    let absolute = authorize(root, config, relative)?;

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

/// Like [`read_allowed_file`], but returns the first `max_bytes` of an
/// oversized file instead of refusing it, along with whether truncation
/// occurred.
///
/// This exists for retrieval. The most relevant document for a question is
/// often the longest one, and failing the read outright means the model
/// receives *nothing* about it — which is exactly how a well-documented
/// subject can end up answered from general knowledge. Showing the
/// beginning of the file, clearly marked as truncated, is strictly better
/// evidence than showing none.
///
/// The security boundary is identical and shared with
/// [`read_allowed_file`]: same root containment, same symlink rejection,
/// same include/exclude rules. Only the size behavior differs, and only
/// for callers that ask for it.
pub fn read_allowed_file_truncated(
    root: &Path,
    config: &ProjectConfig,
    relative: &Path,
    max_bytes: u64,
) -> Result<(String, bool), OrbitError> {
    let absolute = authorize(root, config, relative)?;

    let bytes = std::fs::read(&absolute).map_err(|e| OrbitError::io(&absolute, e))?;
    let text = String::from_utf8(bytes).map_err(|_| OrbitError::NotUtf8Text {
        path: relative.to_path_buf(),
    })?;

    let limit = max_bytes as usize;
    if text.len() <= limit {
        return Ok((text, false));
    }
    // Never split a multi-byte character.
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok((text[..end].to_string(), true))
}

/// Resolve `relative` inside `root` and confirm the project's
/// configuration allows reading it. The single place both read paths
/// enforce the security boundary.
fn authorize(
    root: &Path,
    config: &ProjectConfig,
    relative: &Path,
) -> Result<std::path::PathBuf, OrbitError> {
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
    Ok(absolute)
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

#[cfg(test)]
mod truncation_tests {
    use super::*;
    use std::fs;

    fn config() -> ProjectConfig {
        ProjectConfig::parse(
            "version: 1\nproject:\n  name: demo\ncontext:\n  include:\n    - \"**/*\"\n",
        )
        .unwrap()
    }

    #[test]
    fn returns_the_whole_file_when_it_fits() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("a.md"), "hello there").unwrap();
        let (content, truncated) =
            read_allowed_file_truncated(&root, &config(), Path::new("a.md"), 1_000).unwrap();
        assert_eq!(content, "hello there");
        assert!(!truncated);
    }

    /// The behavior this exists for: a long document yields its beginning
    /// rather than nothing, because the most relevant file for a question
    /// is often the longest one.
    #[test]
    fn truncates_instead_of_failing_on_an_oversized_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("big.md"), "x".repeat(5_000)).unwrap();

        assert!(
            read_allowed_file(&root, &config(), Path::new("big.md"), 100).is_err(),
            "the strict read must still refuse"
        );

        let (content, truncated) =
            read_allowed_file_truncated(&root, &config(), Path::new("big.md"), 100).unwrap();
        assert_eq!(content.len(), 100);
        assert!(truncated);
    }

    #[test]
    fn never_splits_a_multibyte_character() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        // Each `é` is two bytes, so a limit of 5 lands mid-character.
        fs::write(root.join("utf8.md"), "ééééé").unwrap();
        let (content, truncated) =
            read_allowed_file_truncated(&root, &config(), Path::new("utf8.md"), 5).unwrap();
        assert!(truncated);
        assert_eq!(content, "éé", "must stop on a character boundary");
    }

    /// Truncation must not become a way around the security boundary.
    #[test]
    fn enforces_the_same_security_boundary_as_the_strict_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join(".env"), "SECRET=do-not-leak").unwrap();

        assert!(
            read_allowed_file_truncated(&root, &config(), Path::new(".env"), 10_000).is_err(),
            "an excluded file must stay excluded"
        );
        assert!(
            read_allowed_file_truncated(&root, &config(), Path::new("../outside.md"), 10_000)
                .is_err(),
            "traversal must stay rejected"
        );
    }
}
