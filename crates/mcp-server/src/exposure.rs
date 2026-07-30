//! Decides, once at startup, which of a project's `mcp.expose` entries can
//! actually be listed and called through MCP -- reusing the same
//! [`ActionRegistry`] and the same [`ProjectConfig::effective_permission`]
//! the CLI and agent use, so this is a filter over existing behavior, not a
//! second permission system.

use std::collections::{HashMap, HashSet};

use orbit_actions::ActionRegistry;
use orbit_core::Permission;
use orbit_project::ProjectConfig;

/// Why a configured `mcp.expose` entry did not make it into the listable
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposureIssue {
    /// The name does not match any registered action.
    UnknownAction,
    /// The action's effective permission is `deny`.
    DeniedPermission,
    /// The action's effective permission is `ask`. This transport has no
    /// interactive confirmation, and this version does not implement an
    /// approval mechanism for it -- see [`OrbitMcpServer`](crate::OrbitMcpServer).
    RequiresConfirmation,
}

/// A single actionable warning about one `mcp.expose` entry.
#[derive(Debug, Clone)]
pub struct ExposureWarning {
    pub action: String,
    pub issue: ExposureIssue,
    /// Ready to print as-is (to `orbit doctor` output or an `orbit mcp
    /// serve` startup line).
    pub message: String,
}

/// The result of resolving a project's `mcp.expose` list against its
/// registry and permissions.
#[derive(Debug, Clone, Default)]
pub struct ExposureReport {
    /// Action names that can actually be listed and called through MCP.
    pub listable: HashSet<String>,
    pub warnings: Vec<ExposureWarning>,
}

/// Resolve `expose` (a project's raw `mcp.expose` list, which may contain
/// duplicates, names of actions that don't exist, or names whose effective
/// permission isn't `allow`) into what MCP can actually serve.
///
/// Behavior, matching [`docs/MCP.md`](../../../docs/MCP.md):
/// - `allow` → listable and callable;
/// - `ask` → never listed. This transport has no safe way to obtain
///   explicit per-call approval, so rather than inventing an automatic
///   approval mechanism, the action is excluded and a specific warning
///   names the fix (set it to `allow`);
/// - `deny` → never listed, same as if it were absent from `mcp.expose`;
/// - a name with no matching registered action → never listed, flagged so
///   a typo in configuration is visible instead of silently doing nothing;
/// - duplicate entries in `mcp.expose` collapse to one (a `HashSet` result
///   can't represent a duplicate either way).
pub fn compute_exposure(
    registry: &ActionRegistry,
    expose: &[String],
    config: &ProjectConfig,
) -> ExposureReport {
    let mut listable = HashSet::new();
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

    for name in expose {
        if !seen.insert(name.clone()) {
            continue;
        }

        let Some(descriptor) = registry.descriptors().into_iter().find(|d| &d.name == name) else {
            warnings.push(ExposureWarning {
                action: name.clone(),
                issue: ExposureIssue::UnknownAction,
                message: format!(
                    "mcp.expose lists `{name}`, which is not a registered Orbit action. \
                     Check for a typo in `.orbit/project.yaml`."
                ),
            });
            continue;
        };

        let effective = config.effective_permission(name, descriptor.default_permission);
        match effective {
            Permission::Allow => {
                listable.insert(name.clone());
            }
            Permission::Deny => warnings.push(ExposureWarning {
                action: name.clone(),
                issue: ExposureIssue::DeniedPermission,
                message: format!(
                    "MCP exposure `{name}` has permission `deny` and cannot be used through MCP."
                ),
            }),
            Permission::Ask => warnings.push(ExposureWarning {
                action: name.clone(),
                issue: ExposureIssue::RequiresConfirmation,
                message: format!(
                    "MCP exposure `{name}` requires confirmation and cannot be used through \
                     the current non-interactive MCP transport. Set its permission to `allow` \
                     in `.orbit/project.yaml` to expose it."
                ),
            }),
        }
    }

    ExposureReport { listable, warnings }
}

/// Fast lookup from action name to its warning, for the exact-name error
/// path in `call_tool`.
pub(crate) fn warnings_by_action(report: &ExposureReport) -> HashMap<&str, &ExposureWarning> {
    report
        .warnings
        .iter()
        .map(|w| (w.action.as_str(), w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ActionRegistry {
        orbit_actions::native_registry().unwrap()
    }

    fn config(yaml: &str) -> ProjectConfig {
        ProjectConfig::parse(yaml).unwrap()
    }

    #[test]
    fn allow_permission_is_listable() {
        let report = compute_exposure(
            &registry(),
            &["project.search".to_string()],
            &config("version: 1\nproject:\n  name: demo\n"),
        );
        assert!(report.listable.contains("project.search"));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn deny_permission_is_excluded_with_a_warning() {
        let report = compute_exposure(
            &registry(),
            &["command.run_configured".to_string()],
            &config(
                "version: 1\nproject:\n  name: demo\npermissions:\n  command.run_configured: deny\n",
            ),
        );
        assert!(!report.listable.contains("command.run_configured"));
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].issue, ExposureIssue::DeniedPermission);
    }

    #[test]
    fn ask_permission_is_excluded_with_a_specific_warning() {
        // command.run_configured defaults to `ask`.
        let report = compute_exposure(
            &registry(),
            &["command.run_configured".to_string()],
            &config("version: 1\nproject:\n  name: demo\n"),
        );
        assert!(!report.listable.contains("command.run_configured"));
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report.warnings[0].issue,
            ExposureIssue::RequiresConfirmation
        );
        assert!(report.warnings[0].message.contains("non-interactive"));
    }

    #[test]
    fn unknown_action_name_is_flagged() {
        let report = compute_exposure(
            &registry(),
            &["project.write_file".to_string()],
            &config("version: 1\nproject:\n  name: demo\n"),
        );
        assert!(report.listable.is_empty());
        assert_eq!(report.warnings[0].issue, ExposureIssue::UnknownAction);
    }

    #[test]
    fn duplicate_entries_collapse_to_one() {
        let report = compute_exposure(
            &registry(),
            &["project.search".to_string(), "project.search".to_string()],
            &config("version: 1\nproject:\n  name: demo\n"),
        );
        assert_eq!(report.listable.len(), 1);
        assert!(report.warnings.is_empty());
    }
}
