use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A pointer back to the project content that grounded a search result or an
/// action output, so answers can always be traced to their origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReference {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl SourceReference {
    pub fn whole_file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            line_start: None,
            line_end: None,
            section: None,
        }
    }

    pub fn lines(path: impl Into<PathBuf>, start: usize, end: usize) -> Self {
        Self {
            path: path.into(),
            line_start: Some(start),
            line_end: Some(end),
            section: None,
        }
    }

    pub fn with_section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
        self
    }

    /// Split a workspace-scoped `<project>:<path>` path back into its
    /// project and its project-relative path.
    ///
    /// Workspace actions encode project identity into the path string
    /// (see `orbit_workspace::WorkspaceSourceReference::to_plain`) so
    /// multi-project sources flow through this project-agnostic type
    /// unchanged. This is the one place that encoding is decoded, so
    /// event consumers can receive project identity as its own field.
    ///
    /// Returns `(None, original_path)` for an ordinary single-project
    /// source. The prefix is only recognized when it cannot be confused
    /// with a real path: no separators, a conservative identifier
    /// charset, and at least two characters, so a Windows drive letter
    /// (`C:\...`) is never mistaken for a project named `C`.
    pub fn split_project_prefix(&self) -> (Option<String>, PathBuf) {
        let text = self.path.to_string_lossy();
        let Some((prefix, rest)) = text.split_once(':') else {
            return (None, self.path.clone());
        };
        let plausible_project = prefix.len() >= 2
            && prefix.len() <= 64
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !plausible_project || rest.is_empty() {
            return (None, self.path.clone());
        }
        (Some(prefix.to_string()), PathBuf::from(rest))
    }
}

impl std::fmt::Display for SourceReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())?;
        match (self.line_start, self.line_end) {
            (Some(start), Some(end)) if start == end => write!(f, ":{start}")?,
            (Some(start), Some(end)) => write!(f, ":{start}-{end}")?,
            (Some(start), None) => write!(f, ":{start}")?,
            _ => {}
        }
        if let Some(section) = &self.section {
            write!(f, " ({section})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_project_prefix_decodes_a_workspace_source() {
        let source = SourceReference::lines(PathBuf::from("docs:obc/architecture.md"), 3, 4);
        let (project, path) = source.split_project_prefix();
        assert_eq!(project.as_deref(), Some("docs"));
        assert_eq!(path, PathBuf::from("obc/architecture.md"));
    }

    #[test]
    fn split_project_prefix_leaves_a_plain_source_untouched() {
        let source = SourceReference::whole_file(PathBuf::from("docs/obc/architecture.md"));
        let (project, path) = source.split_project_prefix();
        assert_eq!(project, None);
        assert_eq!(path, PathBuf::from("docs/obc/architecture.md"));
    }

    /// A Windows drive letter must never be read as a project name, or a
    /// single-project source on Windows would be reported under a bogus
    /// project called `C`.
    #[test]
    fn split_project_prefix_ignores_a_windows_drive_letter() {
        let source = SourceReference::whole_file(PathBuf::from("C:\\projects\\obc\\main.rs"));
        assert_eq!(source.split_project_prefix().0, None);
    }

    #[test]
    fn split_project_prefix_ignores_prefixes_that_are_not_identifier_shaped() {
        for path in [
            "some/dir:file.md", // separator in the prefix
            "docs:",            // empty remainder
            "wei rd:file.md",   // space is not in the charset
        ] {
            let source = SourceReference::whole_file(PathBuf::from(path));
            assert_eq!(
                source.split_project_prefix().0,
                None,
                "{path} should not parse as a project-prefixed source"
            );
        }
    }
}
