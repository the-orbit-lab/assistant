use std::collections::BTreeMap;
use std::path::Path;

use orbit_core::Permission;
use serde::{Deserialize, Serialize};

pub const SUPPORTED_WORKSPACE_VERSION: u32 = 1;

/// `.orbit/workspace.yaml`, structurally validated (no filesystem access —
/// see [`crate::registry::ProjectRegistry`] for the I/O-dependent checks:
/// project directories existing, canonical containment, per-project
/// `.orbit/project.yaml` loading).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub version: u32,
    pub workspace: WorkspaceMeta,
    #[serde(default)]
    pub projects: BTreeMap<String, WorkspaceProjectEntry>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub defaults: WorkspaceDefaults,
    /// Permissions for the `workspace.*` actions themselves (distinct from
    /// each project's own `permissions`, which always still governs the
    /// per-project native action a workspace action delegates to). Absent
    /// from the example in the workspace spec but added for symmetry with
    /// `.orbit/project.yaml`; every `workspace.*` action has a safe
    /// `allow`-by-default anyway, since they're read-only orchestration
    /// and the real enforcement happens per project underneath.
    #[serde(default)]
    pub permissions: BTreeMap<String, Permission>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProjectEntry {
    /// Workspace-relative (or absolute) filesystem path to the project.
    /// Resolved and validated against the workspace root by
    /// `ProjectRegistry::load`, never trusted as-is.
    pub path: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub relationship_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDefaults {
    #[serde(default)]
    pub project: Option<String>,
}

/// Case/separator-insensitive comparison used for alias and name matching.
/// Never used for path or command resolution -- see
/// [`crate::registry::ProjectRegistry::resolve`].
pub fn normalize_identifier(s: &str) -> String {
    s.trim().to_lowercase().replace(['_', ' '], "-")
}

impl WorkspaceConfig {
    pub fn load(path: &Path) -> Result<Self, orbit_core::OrbitError> {
        let raw = std::fs::read_to_string(path).map_err(|e| orbit_core::OrbitError::io(path, e))?;
        Self::parse(&raw).map_err(|reason| orbit_core::OrbitError::ConfigInvalid {
            path: path.to_path_buf(),
            reason,
        })
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let config: WorkspaceConfig =
            serde_norway::from_str(raw).map_err(|e| format!("YAML parse error: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    /// Purely structural validation: nothing here touches the filesystem.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SUPPORTED_WORKSPACE_VERSION {
            return Err(format!(
                "unsupported workspace configuration version {} (expected {})",
                self.version, SUPPORTED_WORKSPACE_VERSION
            ));
        }
        if self.workspace.name.trim().is_empty() {
            return Err("workspace.name must not be empty".to_string());
        }

        // name -> normalized name, to catch "obc" vs "OBC" collisions too.
        let mut normalized_names: BTreeMap<String, &str> = BTreeMap::new();
        // alias -> owning project name, to catch cross-project alias reuse.
        let mut alias_owners: BTreeMap<String, &str> = BTreeMap::new();
        let all_names: std::collections::BTreeSet<&str> =
            self.projects.keys().map(String::as_str).collect();

        for (name, entry) in &self.projects {
            if name.trim().is_empty() {
                return Err("project names must not be empty".to_string());
            }
            if entry.path.trim().is_empty() {
                return Err(format!("project `{name}` has an empty `path`"));
            }
            let normalized = normalize_identifier(name);
            if let Some(existing) = normalized_names.insert(normalized.clone(), name) {
                return Err(format!(
                    "project names `{existing}` and `{name}` are ambiguous once normalized \
                     (both become `{normalized}`)"
                ));
            }

            let mut seen_in_project = std::collections::BTreeSet::new();
            for alias in &entry.aliases {
                if alias.trim().is_empty() {
                    return Err(format!("project `{name}` has an empty alias"));
                }
                if all_names.contains(alias.as_str()) && alias != name {
                    return Err(format!(
                        "alias `{alias}` on project `{name}` collides with another project's name"
                    ));
                }
                let normalized_alias = normalize_identifier(alias);
                if !seen_in_project.insert(normalized_alias.clone()) {
                    return Err(format!(
                        "project `{name}` lists alias `{alias}` more than once"
                    ));
                }
                if let Some(owner) = alias_owners.insert(normalized_alias.clone(), name)
                    && owner != name
                {
                    return Err(format!(
                        "alias `{alias}` is claimed by both `{owner}` and `{name}`"
                    ));
                }
            }
        }

        if let Some(default_project) = &self.defaults.project
            && !all_names.contains(default_project.as_str())
        {
            return Err(format!(
                "defaults.project `{default_project}` is not a registered project"
            ));
        }

        let mut seen_relationships = std::collections::BTreeSet::new();
        for relationship in &self.relationships {
            if !all_names.contains(relationship.source.as_str()) {
                return Err(format!(
                    "relationship references unknown project `{}`",
                    relationship.source
                ));
            }
            if !all_names.contains(relationship.target.as_str()) {
                return Err(format!(
                    "relationship references unknown project `{}`",
                    relationship.target
                ));
            }
            if relationship.relationship_type.trim().is_empty() {
                return Err("relationship `type` must not be empty".to_string());
            }
            let key = (
                relationship.source.clone(),
                relationship.target.clone(),
                relationship.relationship_type.clone(),
            );
            if !seen_relationships.insert(key) {
                return Err(format!(
                    "duplicate relationship {} -> {} ({})",
                    relationship.source, relationship.target, relationship.relationship_type
                ));
            }
        }

        Ok(())
    }
}

/// Render `s` as a double-quoted YAML scalar -- same rationale as
/// `orbit_project::config`'s `yaml_quote`: arbitrary user-supplied text
/// (a workspace/project name or description) can never corrupt the
/// generated document, and an empty string is written unambiguously
/// rather than as a bare, null-parsing value.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A documented starter configuration written by `orbit workspace init`.
/// `detected_projects` are directory names found to already contain
/// `.orbit/project.yaml`, registered with no aliases and an empty
/// description -- the user fills those in.
pub fn starter_yaml(name: &str, description: &str, detected_projects: &[String]) -> String {
    let quoted_name = yaml_quote(name);
    let quoted_description = yaml_quote(description);

    let projects_block = if detected_projects.is_empty() {
        "projects: {}\n".to_string()
    } else {
        let mut block = String::from("projects:\n");
        for project in detected_projects {
            block.push_str(&format!(
                "  {project}:\n    path: ./{project}\n    aliases: []\n    description: \"\"\n"
            ));
        }
        block
    };

    format!(
        r#"version: 1

workspace:
  name: {quoted_name}
  description: {quoted_description}

{projects_block}
# Directional relationships between projects, e.g. "obc is documented-by
# docs". Purely descriptive today -- shown by `orbit workspace` and
# workspace.information, not used to change access.
relationships: []

defaults:
  # A project used for safe, read-only overview questions and
  # single-project commands when no project is explicitly selected.
  # Commands that change anything (e.g. `orbit run`) never use this.
  project: {default_project}
"#,
        default_project = detected_projects
            .first()
            .map(|p| yaml_quote(p))
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> String {
        "version: 1\nworkspace:\n  name: Orbit Lab\nprojects:\n  obc:\n    path: ./obc\n"
            .to_string()
    }

    #[test]
    fn parses_a_minimal_workspace() {
        let config = WorkspaceConfig::parse(&minimal()).unwrap();
        assert_eq!(config.workspace.name, "Orbit Lab");
        assert_eq!(config.projects.len(), 1);
    }

    #[test]
    fn rejects_unsupported_version() {
        let yaml = "version: 2\nworkspace:\n  name: x\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("unsupported workspace configuration version"));
    }

    #[test]
    fn rejects_empty_project_name() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  \"\":\n    path: ./x\n";
        assert!(WorkspaceConfig::parse(yaml).is_err());
    }

    #[test]
    fn rejects_alias_colliding_with_another_project_name() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  obc:\n    path: ./obc\n    aliases: [docs]\n  docs:\n    path: ./docs\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("collides with another project's name"));
    }

    #[test]
    fn rejects_alias_claimed_by_two_projects() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  obc:\n    path: ./obc\n    aliases: [flight]\n  assistant:\n    path: ./assistant\n    aliases: [flight]\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("is claimed by both"));
    }

    #[test]
    fn rejects_duplicate_alias_within_one_project() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  obc:\n    path: ./obc\n    aliases: [flight, Flight]\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("more than once"));
    }

    #[test]
    fn rejects_invalid_default_project() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  obc:\n    path: ./obc\ndefaults:\n  project: nope\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("is not a registered project"));
    }

    #[test]
    fn rejects_relationship_to_unknown_project() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  obc:\n    path: ./obc\nrelationships:\n  - source: obc\n    target: ghost\n    type: uses\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("unknown project"));
    }

    #[test]
    fn rejects_duplicate_relationship() {
        let yaml = "version: 1\nworkspace:\n  name: x\nprojects:\n  a:\n    path: ./a\n  b:\n    path: ./b\nrelationships:\n  - source: a\n    target: b\n    type: uses\n  - source: a\n    target: b\n    type: uses\n";
        let err = WorkspaceConfig::parse(yaml).unwrap_err();
        assert!(err.contains("duplicate relationship"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = "version: 1\nworkspace:\n  name: x\nbogus: true\n";
        assert!(WorkspaceConfig::parse(yaml).is_err());
    }

    #[test]
    fn starter_yaml_round_trips_with_detected_projects() {
        let yaml = starter_yaml(
            "Orbit Lab",
            "desc",
            &["obc".to_string(), "docs".to_string()],
        );
        let config = WorkspaceConfig::parse(&yaml).expect("starter workspace config must be valid");
        assert_eq!(config.workspace.name, "Orbit Lab");
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.defaults.project.as_deref(), Some("obc"));
    }

    #[test]
    fn starter_yaml_round_trips_with_no_detected_projects() {
        let yaml = starter_yaml("Orbit Lab", "", &[]);
        let config = WorkspaceConfig::parse(&yaml).expect("starter workspace config must be valid");
        assert!(config.projects.is_empty());
        assert_eq!(config.defaults.project, None);
    }

    #[test]
    fn normalize_identifier_folds_case_and_separators() {
        assert_eq!(normalize_identifier("Mission Tools"), "mission-tools");
        assert_eq!(normalize_identifier("mission_tools"), "mission-tools");
        assert_eq!(normalize_identifier("  OBC "), "obc");
    }

    /// Keeps `examples/workspace.yaml` honest as the config format
    /// evolves -- a structural drift here would otherwise only surface
    /// when a user copies the example and hits a confusing parse error.
    #[test]
    fn example_workspace_yaml_is_structurally_valid() {
        let yaml = include_str!("../../../examples/workspace.yaml");
        let config = WorkspaceConfig::parse(yaml).expect("examples/workspace.yaml must parse");
        assert_eq!(config.workspace.name, "Orbit Lab");
        assert_eq!(config.defaults.project.as_deref(), Some("assistant"));
    }
}
