use std::path::PathBuf;

use orbit_core::SourceReference;
use serde::{Deserialize, Serialize};

/// A source reference scoped to the project it came from. Never merged
/// with another project's reference to a path of the same name -- project
/// identity is part of the source's identity, not decoration on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSourceReference {
    pub project: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl WorkspaceSourceReference {
    pub fn new(project: impl Into<String>, source: SourceReference) -> Self {
        Self {
            project: project.into(),
            path: source.path,
            line_start: source.line_start,
            line_end: source.line_end,
            section: source.section,
        }
    }

    /// Encode project identity into the path itself
    /// (`"<project>:<path>"`) so this reference can flow through the
    /// existing, project-agnostic `orbit_core::SourceReference` pipeline
    /// (`Agent`'s source aggregation, `dedupe_sources`, CLI
    /// `print_sources`) with zero changes to `orbit-core`, while still
    /// rendering in exactly the `docs:obc/adr/....md:18-41` format
    /// multi-project sources use. Dedup keys naturally stay
    /// project-scoped this way too: `docs:README.md` and `obc:README.md`
    /// are different path strings and can never be collapsed together.
    pub fn to_plain(&self) -> SourceReference {
        SourceReference {
            path: PathBuf::from(format!("{}:{}", self.project, self.path.display())),
            line_start: self.line_start,
            line_end: self.line_end,
            section: self.section.clone(),
        }
    }
}

impl std::fmt::Display for WorkspaceSourceReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.project, self.path.display())?;
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

/// Normalize a multi-project run's collected sources: drop exact
/// duplicates, drop a project-scoped path-only reference in favor of a
/// more precise line-ranged reference to the same (project, path) pair,
/// and otherwise preserve first-seen order. Project identity is always
/// part of the dedup key, so `docs:README.md` and `obc:README.md` are
/// never collapsed into each other even though the path matches.
pub fn dedupe_workspace_sources(
    sources: Vec<WorkspaceSourceReference>,
) -> Vec<WorkspaceSourceReference> {
    let has_line_range: std::collections::HashSet<(&str, &std::path::Path)> = sources
        .iter()
        .filter(|s| s.line_start.is_some())
        .map(|s| (s.project.as_str(), s.path.as_path()))
        .collect();

    let mut deduped: Vec<WorkspaceSourceReference> = Vec::new();
    for source in &sources {
        let key = (source.project.as_str(), source.path.as_path());
        if source.line_start.is_none() && has_line_range.contains(&key) {
            continue;
        }
        if !deduped.contains(source) {
            deduped.push(source.clone());
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(project: &str, path: &str, line: usize) -> WorkspaceSourceReference {
        WorkspaceSourceReference {
            project: project.to_string(),
            path: PathBuf::from(path),
            line_start: Some(line),
            line_end: Some(line),
            section: None,
        }
    }

    fn whole_file(project: &str, path: &str) -> WorkspaceSourceReference {
        WorkspaceSourceReference {
            project: project.to_string(),
            path: PathBuf::from(path),
            line_start: None,
            line_end: None,
            section: None,
        }
    }

    #[test]
    fn never_merges_the_same_path_across_different_projects() {
        let sources = vec![line("docs", "README.md", 3), line("obc", "README.md", 3)];
        let deduped = dedupe_workspace_sources(sources.clone());
        assert_eq!(
            deduped, sources,
            "distinct projects must both survive dedup"
        );
    }

    #[test]
    fn drops_whole_file_reference_when_a_line_range_exists_for_the_same_project() {
        let sources = vec![
            whole_file("docs", "README.md"),
            line("docs", "README.md", 3),
        ];
        assert_eq!(
            dedupe_workspace_sources(sources),
            vec![line("docs", "README.md", 3)]
        );
    }

    #[test]
    fn keeps_whole_file_reference_for_a_different_project_even_if_another_has_a_line_range() {
        let sources = vec![whole_file("obc", "README.md"), line("docs", "README.md", 3)];
        let deduped = dedupe_workspace_sources(sources.clone());
        assert_eq!(deduped, sources);
    }

    #[test]
    fn display_formats_as_project_colon_path_colon_lines() {
        let source = WorkspaceSourceReference {
            project: "obc".to_string(),
            path: PathBuf::from("src/platform/stm32.rs"),
            line_start: Some(12),
            line_end: Some(68),
            section: None,
        };
        assert_eq!(source.to_string(), "obc:src/platform/stm32.rs:12-68");
    }
}
