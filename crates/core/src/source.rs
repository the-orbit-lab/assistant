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
