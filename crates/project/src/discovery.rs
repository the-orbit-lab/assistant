use std::path::PathBuf;

use orbit_core::OrbitError;
use walkdir::WalkDir;

use crate::config::ProjectConfig;
use crate::security::{build_glob_set, is_path_allowed};

/// Hard ceiling on how many files a single discovery pass will enumerate,
/// independent of project configuration, so a misconfigured or huge
/// repository can't turn a search into an unbounded filesystem walk.
pub const MAX_DISCOVERED_FILES: usize = 20_000;

/// Files larger than this are still *listed*, but treated as non-text for
/// search/read purposes unless the caller asks for more.
pub const DEFAULT_MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;

const TEXT_EXTENSIONS: &[&str] = &[
    "md", "rs", "toml", "yaml", "yml", "txt", "json", "js", "ts", "tsx", "jsx", "py", "c", "h",
    "cpp", "hpp", "cc", "cs", "go", "java", "kt", "rb", "sh", "bash", "zsh", "sql", "proto",
    "html", "css", "ini", "cfg", "conf", "xml", "csv",
];

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Path relative to the project root, always forward-slash-free of `..`.
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub size: u64,
    pub is_text: bool,
}

/// Recursively discover every file the project configuration allows.
///
/// Applies include/exclude globs (exclude always wins, including the
/// mandatory non-overridable set), never follows symlinks during the walk,
/// and stops after [`MAX_DISCOVERED_FILES`] entries.
pub fn discover_files(
    root: &std::path::Path,
    config: &ProjectConfig,
) -> Result<Vec<DiscoveredFile>, OrbitError> {
    let includes = build_glob_set(&config.context.include)?;
    let excludes = build_glob_set(&config.effective_excludes())?;

    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prune mandatory-excluded directories early so we never even
            // descend into .git or target.
            if e.depth() == 0 {
                return true;
            }
            let Ok(relative) = e.path().strip_prefix(root) else {
                return true;
            };
            !(excludes.is_match(relative) && e.file_type().is_dir())
        })
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if files.len() >= MAX_DISCOVERED_FILES {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative_path) = entry.path().strip_prefix(root) else {
            continue;
        };
        if !is_path_allowed(relative_path, &includes, &excludes) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_text = relative_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| TEXT_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
            .unwrap_or(false)
            && metadata.len() <= DEFAULT_MAX_TEXT_FILE_BYTES;

        files.push(DiscoveredFile {
            relative_path: relative_path.to_path_buf(),
            absolute_path: entry.path().to_path_buf(),
            size: metadata.len(),
            is_text,
        });
    }

    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;
    use std::fs;

    fn config_with(include: &[&str], exclude: &[&str]) -> ProjectConfig {
        let yaml = format!(
            "version: 1\nproject:\n  name: demo\ncontext:\n  include:\n{}\n  exclude:\n{}\n",
            include
                .iter()
                .map(|p| format!("    - \"{p}\""))
                .collect::<Vec<_>>()
                .join("\n"),
            exclude
                .iter()
                .map(|p| format!("    - \"{p}\""))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        ProjectConfig::parse(&yaml).unwrap()
    }

    #[test]
    fn discovers_included_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("notes.txt"), "not included").unwrap();

        let config = config_with(&["README.md", "src/**"], &[]);
        let files = discover_files(&root, &config).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|f| f.relative_path.to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"README.md".to_string()));
        assert!(names.contains(&"src/main.rs".to_string()));
        assert!(!names.contains(&"notes.txt".to_string()));
    }

    #[test]
    fn default_include_covers_root_level_docs_beyond_readme() {
        // Regression test: a project with no explicit `context.include`
        // (the common case straight out of `orbit init`) must still see
        // every root-level markdown doc -- not just README.md -- so files
        // like CLAUDE.md are visible to search/read/ask instead of being
        // silently invisible to every grounded answer.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("README.md"), "# Demo").unwrap();
        fs::write(root.join("CLAUDE.md"), "# Instructions").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/PROJECT_SPEC.md"), "# Spec").unwrap();

        let config = ProjectConfig::parse("version: 1\nproject:\n  name: demo\n").unwrap();
        let files = discover_files(&root, &config).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|f| f.relative_path.to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"README.md".to_string()));
        assert!(names.contains(&"CLAUDE.md".to_string()));
        assert!(names.contains(&"docs/PROJECT_SPEC.md".to_string()));
    }

    #[test]
    fn exclude_precedence_prunes_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/out.rs"), "generated").unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();

        let config = config_with(&["**/*"], &[]);
        let files = discover_files(&root, &config).unwrap();
        assert!(files.iter().all(|f| !f.relative_path.starts_with("target")));
    }

    #[test]
    fn secret_files_are_always_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::write(root.join("id.pem"), "-----BEGIN-----").unwrap();

        let config = config_with(&["**/*"], &[]);
        let files = discover_files(&root, &config).unwrap();
        assert!(files.is_empty());
    }
}
