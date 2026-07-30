use std::path::PathBuf;

/// The configured project's identity (its declared name), independent of
/// where it lives on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId(pub String);

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Resolved filesystem locations for a project. `root` is always an
/// absolute, canonicalized path and is the security boundary every action
/// enforces against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub config_path: PathBuf,
}

impl ProjectPaths {
    pub fn orbit_dir(&self) -> PathBuf {
        self.root.join(".orbit")
    }
}
