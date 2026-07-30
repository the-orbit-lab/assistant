use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use orbit_actions::ActionContext;
use orbit_core::OrbitError;
use orbit_project::ProjectConfig;

use crate::config::{Relationship, WorkspaceConfig, normalize_identifier};

/// Everything the registry knows about one configured project, whether or
/// not it actually loaded successfully.
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    /// The path exactly as configured (workspace-relative, typically).
    pub configured_path: String,
    /// Canonical project root. Only meaningful when `available` -- an
    /// unavailable project may not even have a real directory.
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: Option<ProjectConfig>,
    pub available: bool,
    pub error: Option<String>,
    /// Relationships configured with this project as the source, e.g.
    /// `obc -> docs (documented-by)`.
    pub relationships: Vec<Relationship>,
}

impl ProjectEntry {
    /// Build the `ActionContext` used to run this project's own native
    /// actions, failing clearly if the project never loaded.
    pub fn action_context(&self) -> Result<ActionContext, OrbitError> {
        let config = self
            .config
            .clone()
            .ok_or_else(|| OrbitError::ProjectUnavailable {
                name: self.name.clone(),
                reason: self
                    .error
                    .clone()
                    .unwrap_or_else(|| "project failed to load".to_string()),
            })?;
        Ok(ActionContext {
            root: self.root.clone(),
            config_path: self.config_path.clone(),
            config,
        })
    }
}

/// Resolves registered project names and aliases into loaded project
/// state. Every project keeps its own canonical root, its own loaded
/// `.orbit/project.yaml`, and its own availability -- this is a directory
/// of independent project runtimes, not a merged filesystem.
#[derive(Debug)]
pub struct ProjectRegistry {
    pub workspace_root: PathBuf,
    pub workspace_config_path: PathBuf,
    pub config: WorkspaceConfig,
    entries: BTreeMap<String, ProjectEntry>,
    /// normalized alias -> registered name
    alias_index: HashMap<String, String>,
    /// normalized name -> registered name
    name_index: HashMap<String, String>,
}

impl ProjectRegistry {
    /// Resolve and load every configured project against `workspace_root`.
    /// A project directory that's missing, or missing its own
    /// `.orbit/project.yaml`, or whose own config is invalid, is marked
    /// unavailable rather than failing the whole workspace -- but a path
    /// that resolves *outside* the workspace root (by `..` traversal, an
    /// absolute path elsewhere, or a symlink) is a configuration error
    /// that fails the whole load: that's a security boundary, not a
    /// "service is down" situation.
    pub fn load(
        workspace_root: PathBuf,
        workspace_config_path: PathBuf,
        config: WorkspaceConfig,
    ) -> Result<Self, OrbitError> {
        let mut entries = BTreeMap::new();
        let mut alias_index = HashMap::new();
        let mut name_index = HashMap::new();
        let mut seen_canonical: HashMap<PathBuf, String> = HashMap::new();

        for (name, project) in &config.projects {
            let relationships: Vec<Relationship> = config
                .relationships
                .iter()
                .filter(|r| &r.source == name)
                .cloned()
                .collect();

            let resolved = orbit_project::security::resolve_within_root(
                &workspace_root,
                Path::new(&project.path),
            );
            let candidate_root = match resolved {
                Ok(path) => path,
                Err(OrbitError::PathOutsideProject { .. } | OrbitError::SymlinkEscape { .. }) => {
                    return Err(OrbitError::WorkspaceProjectEscapesRoot {
                        name: name.clone(),
                        path: PathBuf::from(&project.path),
                    });
                }
                Err(other) => return Err(other),
            };

            let mut entry = ProjectEntry {
                name: name.clone(),
                aliases: project.aliases.clone(),
                description: project.description.clone(),
                configured_path: project.path.clone(),
                root: candidate_root.clone(),
                config_path: candidate_root.join(".orbit").join("project.yaml"),
                config: None,
                available: false,
                error: None,
                relationships,
            };

            if !candidate_root.is_dir() {
                entry.error = Some(format!(
                    "project directory `{}` does not exist",
                    candidate_root.display()
                ));
            } else {
                // resolve_within_root already canonicalized `candidate_root`
                // when it exists, so this is the real, symlink-resolved path.
                if let Some(owner) = seen_canonical.get(&candidate_root) {
                    entry.error = Some(format!(
                        "resolves to the same directory as project `{owner}`; \
                         two names can't share one project root"
                    ));
                } else {
                    seen_canonical.insert(candidate_root.clone(), name.clone());
                    if !entry.config_path.is_file() {
                        entry.error = Some(
                            "`.orbit/project.yaml` was not found in this project directory"
                                .to_string(),
                        );
                    } else {
                        match ProjectConfig::load(&entry.config_path) {
                            Ok(loaded) => {
                                entry.available = true;
                                entry.config = Some(loaded);
                            }
                            Err(err) => entry.error = Some(err.to_string()),
                        }
                    }
                }
            }

            let normalized_name = normalize_identifier(name);
            name_index.insert(normalized_name, name.clone());
            for alias in &project.aliases {
                alias_index.insert(normalize_identifier(alias), name.clone());
            }
            entries.insert(name.clone(), entry);
        }

        Ok(Self {
            workspace_root,
            workspace_config_path,
            config,
            entries,
            alias_index,
            name_index,
        })
    }

    pub fn list_projects(&self) -> Vec<&ProjectEntry> {
        self.entries.values().collect()
    }

    /// Exact registered name only -- no alias, no normalization.
    pub fn get_project(&self, name: &str) -> Option<&ProjectEntry> {
        self.entries.get(name)
    }

    pub fn find_by_alias(&self, alias: &str) -> Option<&ProjectEntry> {
        self.alias_index
            .get(&normalize_identifier(alias))
            .and_then(|name| self.entries.get(name))
    }

    pub fn default_project(&self) -> Option<&ProjectEntry> {
        self.config
            .defaults
            .project
            .as_deref()
            .and_then(|name| self.entries.get(name))
    }

    pub fn relationships(&self) -> &[Relationship] {
        &self.config.relationships
    }

    /// A synthetic `ActionContext` representing the *workspace itself*,
    /// used for the outer `ActionRegistry::execute` dispatch of
    /// `workspace.*` actions (their own permission default lookup, e.g.
    /// `workspace.search: allow`). It carries the workspace's own
    /// `permissions` map but an otherwise-empty project shape --
    /// `workspace.*` actions never read its `context`/`commands`, since
    /// each one resolves its own per-project `ActionContext` internally
    /// before touching any project's files.
    pub fn workspace_action_context(&self) -> ActionContext {
        ActionContext {
            root: self.workspace_root.clone(),
            config_path: self.workspace_config_path.clone(),
            config: ProjectConfig {
                version: 1,
                project: orbit_project::ProjectMeta {
                    name: self.config.workspace.name.clone(),
                    project_type: "workspace".to_string(),
                    description: self.config.workspace.description.clone(),
                },
                model: orbit_project::ModelConfig::default(),
                context: orbit_project::ContextConfig {
                    include: Vec::new(),
                    exclude: Vec::new(),
                },
                commands: BTreeMap::new(),
                permissions: self.config.permissions.clone(),
                mcp: orbit_project::McpConfig::default(),
            },
        }
    }

    /// Tiered, deterministic resolution: exact name, then exact alias,
    /// then normalized name, then normalized alias. Never fuzzy -- a
    /// selector that doesn't land exactly (at some tier) is rejected, not
    /// guessed at.
    pub fn resolve_project(&self, selector: &str) -> Result<&ProjectEntry, OrbitError> {
        if let Some(entry) = self.entries.get(selector) {
            return Ok(entry);
        }
        if let Some(name) = self
            .entries
            .values()
            .find(|e| e.aliases.iter().any(|a| a == selector))
            .map(|e| e.name.clone())
        {
            return Ok(&self.entries[&name]);
        }
        let normalized = normalize_identifier(selector);
        if let Some(name) = self.name_index.get(&normalized) {
            return Ok(&self.entries[name]);
        }
        if let Some(name) = self.alias_index.get(&normalized) {
            return Ok(&self.entries[name]);
        }

        Err(OrbitError::UnknownProject {
            name: selector.to_string(),
            available: self.entries.keys().cloned().collect(),
        })
    }

    /// Resolve every selector, in order, deduplicating repeats while
    /// preserving first-seen order. Any single unresolved selector fails
    /// the whole call -- a multi-project request never silently drops one
    /// of the projects it named.
    pub fn resolve_projects(&self, selectors: &[String]) -> Result<Vec<&ProjectEntry>, OrbitError> {
        let mut resolved = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for selector in selectors {
            let entry = self.resolve_project(selector)?;
            if seen.insert(entry.name.clone()) {
                resolved.push(entry);
            }
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_project(root: &Path, name: &str) {
        fs::create_dir_all(root.join(".orbit")).unwrap();
        fs::write(
            root.join(".orbit/project.yaml"),
            format!("version: 1\nproject:\n  name: {name}\n"),
        )
        .unwrap();
        fs::write(root.join("README.md"), format!("# {name}")).unwrap();
    }

    fn config(yaml: &str) -> WorkspaceConfig {
        WorkspaceConfig::parse(yaml).unwrap()
    }

    #[test]
    fn loads_available_projects_and_resolves_by_name_and_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");

        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n    aliases: [flight-computer]\n",
        );
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        assert!(registry.get_project("obc").unwrap().available);
        assert_eq!(registry.resolve_project("obc").unwrap().name, "obc");
        assert_eq!(
            registry.resolve_project("flight-computer").unwrap().name,
            "obc"
        );
        assert_eq!(
            registry.resolve_project("Flight-Computer").unwrap().name,
            "obc",
            "normalized alias match must still work"
        );
        assert_eq!(registry.resolve_project("OBC").unwrap().name, "obc");
    }

    #[test]
    fn missing_project_directory_is_unavailable_not_a_load_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cfg =
            config("version: 1\nworkspace:\n  name: Lab\nprojects:\n  ghost:\n    path: ./ghost\n");
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let entry = registry.get_project("ghost").unwrap();
        assert!(!entry.available);
        assert!(entry.error.as_ref().unwrap().contains("does not exist"));
    }

    #[test]
    fn missing_project_yaml_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        let cfg =
            config("version: 1\nworkspace:\n  name: Lab\nprojects:\n  empty:\n    path: ./empty\n");
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let entry = registry.get_project("empty").unwrap();
        assert!(!entry.available);
        assert!(
            entry
                .error
                .as_ref()
                .unwrap()
                .contains("project.yaml` was not found")
        );
    }

    #[test]
    fn rejects_project_path_escaping_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("member")).unwrap();
        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  outsider:\n    path: ../outside-the-workspace\n",
        );
        let err = ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg)
            .unwrap_err();
        assert!(matches!(
            err,
            OrbitError::WorkspaceProjectEscapesRoot { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_project_path_that_is_a_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&outside.path().canonicalize().unwrap(), "outside");
        std::os::unix::fs::symlink(outside.path(), root.join("linked")).unwrap();

        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  linked:\n    path: ./linked\n",
        );
        let err = ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg)
            .unwrap_err();
        assert!(matches!(
            err,
            OrbitError::WorkspaceProjectEscapesRoot { .. }
        ));
    }

    #[test]
    fn two_names_resolving_to_the_same_directory_marks_the_second_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");
        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  obc-again:\n    path: ./obc\n",
        );
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        // BTreeMap iteration order is alphabetical: "obc" before "obc-again".
        assert!(registry.get_project("obc").unwrap().available);
        let dup = registry.get_project("obc-again").unwrap();
        assert!(!dup.available);
        assert!(dup.error.as_ref().unwrap().contains("same directory"));
    }

    #[test]
    fn resolve_projects_deduplicates_while_preserving_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");
        write_project(&root.join("docs"), "docs");
        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  docs:\n    path: ./docs\n",
        );
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let resolved = registry
            .resolve_projects(&["docs".to_string(), "obc".to_string(), "docs".to_string()])
            .unwrap();
        let names: Vec<_> = resolved.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "obc"]);
    }

    #[test]
    fn resolve_projects_fails_if_any_selector_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");
        let cfg =
            config("version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n");
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let err = registry
            .resolve_projects(&["obc".to_string(), "ghost".to_string()])
            .unwrap_err();
        assert!(matches!(err, OrbitError::UnknownProject { .. }));
    }

    #[test]
    fn unknown_selector_lists_available_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");
        let cfg =
            config("version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n");
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let err = registry.resolve_project("ghost").unwrap_err();
        match err {
            OrbitError::UnknownProject { available, .. } => {
                assert_eq!(available, vec!["obc".to_string()]);
            }
            other => panic!("expected UnknownProject, got {other:?}"),
        }
    }

    #[test]
    fn default_project_resolves_the_configured_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("assistant"), "assistant");
        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  assistant:\n    path: ./assistant\ndefaults:\n  project: assistant\n",
        );
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();
        assert_eq!(registry.default_project().unwrap().name, "assistant");
    }

    #[test]
    fn relationships_are_attached_to_the_source_project_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc");
        write_project(&root.join("docs"), "docs");
        let cfg = config(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  docs:\n    path: ./docs\nrelationships:\n  - source: obc\n    target: docs\n    type: documented-by\n",
        );
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();

        let obc = registry.get_project("obc").unwrap();
        assert_eq!(obc.relationships.len(), 1);
        assert_eq!(obc.relationships[0].target, "docs");
        assert!(
            registry
                .get_project("docs")
                .unwrap()
                .relationships
                .is_empty()
        );
    }
}
