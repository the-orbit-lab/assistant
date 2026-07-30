pub mod information;
pub mod list_project_files;
pub mod list_projects;
pub mod project_information;
pub mod read_file;
pub mod search;

use std::sync::Arc;

use orbit_actions::ActionRegistry;
use orbit_core::OrbitError;

use crate::runtime::WorkspaceRuntime;

/// Register all six `workspace.*` actions, each sharing the same
/// [`WorkspaceRuntime`] (and therefore the same `ProjectRegistry` and
/// native Action Registry underneath).
pub fn register_all(
    registry: &mut ActionRegistry,
    runtime: WorkspaceRuntime,
) -> Result<(), OrbitError> {
    registry.register(Arc::new(information::WorkspaceInformationAction {
        runtime: runtime.clone(),
    }))?;
    registry.register(Arc::new(list_projects::WorkspaceListProjectsAction {
        runtime: runtime.clone(),
    }))?;
    registry.register(Arc::new(
        project_information::WorkspaceProjectInformationAction {
            runtime: runtime.clone(),
        },
    ))?;
    registry.register(Arc::new(search::WorkspaceSearchAction {
        runtime: runtime.clone(),
    }))?;
    registry.register(Arc::new(read_file::WorkspaceReadFileAction {
        runtime: runtime.clone(),
    }))?;
    registry.register(Arc::new(
        list_project_files::WorkspaceListProjectFilesAction { runtime },
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::{ActionInput, AlwaysAllow, AlwaysDeny, ConfirmationProvider};
    use serde_json::json;
    use std::path::Path;

    fn write_project(root: &Path, name: &str, extra_yaml: &str, files: &[(&str, &str)]) {
        std::fs::create_dir_all(root.join(".orbit")).unwrap();
        std::fs::write(
            root.join(".orbit/project.yaml"),
            format!(
                "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n{extra_yaml}"
            ),
        )
        .unwrap();
        for (path, content) in files {
            let full = root.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, content).unwrap();
        }
    }

    async fn build(root: &Path, confirmation: Arc<dyn ConfirmationProvider>) -> ActionRegistry {
        let cfg = crate::WorkspaceConfig::parse(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  docs:\n    path: ./docs\n",
        )
        .unwrap();
        let project_registry = Arc::new(
            crate::ProjectRegistry::load(
                root.to_path_buf(),
                root.join(".orbit/workspace.yaml"),
                cfg,
            )
            .unwrap(),
        );
        let mut registry = ActionRegistry::new();
        let workspace_runtime = WorkspaceRuntime::new(project_registry, confirmation).unwrap();
        register_all(&mut registry, workspace_runtime).unwrap();
        registry
    }

    fn ctx(root: &Path) -> orbit_actions::ActionContext {
        orbit_actions::ActionContext {
            root: root.to_path_buf(),
            config_path: root.join(".orbit/workspace.yaml"),
            config: orbit_project::ProjectConfig::parse("version: 1\nproject:\n  name: ws\n")
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn search_returns_project_scoped_results_from_multiple_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(
            &root.join("obc"),
            "obc",
            "",
            &[("src/watchdog.rs", "// watchdog\nfn reset() {}\n")],
        );
        write_project(
            &root.join("docs"),
            "docs",
            "",
            &[("architecture.md", "# Watchdog\nresets on brownout\n")],
        );

        let registry = build(&root, Arc::new(AlwaysDeny)).await;
        let (_, result) = registry
            .execute(
                &ctx(&root),
                search::NAME,
                ActionInput(json!({"projects": ["obc", "docs"], "query": "watchdog"})),
                &AlwaysDeny,
            )
            .await;
        let output = result.unwrap();
        let results = output.data["results"].as_array().unwrap();
        let projects: std::collections::HashSet<_> = results
            .iter()
            .map(|r| r["project"].as_str().unwrap())
            .collect();
        assert!(projects.contains("obc"));
        assert!(projects.contains("docs"));

        // Sources must be project-qualified (the "project:path" format).
        assert!(
            output
                .sources
                .iter()
                .any(|s| s.path.to_string_lossy().starts_with("obc:"))
        );
        assert!(
            output
                .sources
                .iter()
                .any(|s| s.path.to_string_lossy().starts_with("docs:"))
        );
    }

    #[tokio::test]
    async fn search_rejects_an_unknown_project_name_rather_than_silently_skipping_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc", "", &[]);
        write_project(&root.join("docs"), "docs", "", &[]);

        let registry = build(&root, Arc::new(AlwaysDeny)).await;
        let (_, result) = registry
            .execute(
                &ctx(&root),
                search::NAME,
                ActionInput(json!({"projects": ["obc", "ghost"], "query": "anything"})),
                &AlwaysDeny,
            )
            .await;
        assert!(matches!(result, Err(OrbitError::UnknownProject { .. })));
    }

    #[tokio::test]
    async fn search_rejects_empty_projects_list() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc", "", &[]);
        write_project(&root.join("docs"), "docs", "", &[]);
        let registry = build(&root, Arc::new(AlwaysDeny)).await;
        let (_, result) = registry
            .execute(
                &ctx(&root),
                search::NAME,
                ActionInput(json!({"projects": [], "query": "anything"})),
                &AlwaysDeny,
            )
            .await;
        assert!(matches!(result, Err(OrbitError::InvalidActionInput { .. })));
    }

    #[tokio::test]
    async fn read_file_cannot_escape_into_a_sibling_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(
            &root.join("obc"),
            "obc",
            "",
            &[("src/watchdog.rs", "obc secret")],
        );
        write_project(&root.join("docs"), "docs", "", &[]);

        let registry = build(&root, Arc::new(AlwaysDeny)).await;
        // Ask docs to read a path that reaches into obc via `..`.
        let (_, result) = registry
            .execute(
                &ctx(&root),
                read_file::NAME,
                ActionInput(json!({"project": "docs", "path": "../obc/src/watchdog.rs"})),
                &AlwaysDeny,
            )
            .await;
        assert!(matches!(result, Err(OrbitError::PathOutsideProject { .. })));
    }

    #[tokio::test]
    async fn permissions_are_isolated_per_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(
            &root.join("obc"),
            "obc",
            "permissions:\n  project.read_file: deny\n",
            &[("README.md", "obc")],
        );
        write_project(&root.join("docs"), "docs", "", &[("README.md", "docs")]);

        let registry = build(&root, Arc::new(AlwaysAllow)).await;

        let (_, obc_result) = registry
            .execute(
                &ctx(&root),
                read_file::NAME,
                ActionInput(json!({"project": "obc", "path": "README.md"})),
                &AlwaysAllow,
            )
            .await;
        assert!(
            matches!(obc_result, Err(OrbitError::PermissionDenied { .. })),
            "obc denies project.read_file and that must be respected"
        );

        let (_, docs_result) = registry
            .execute(
                &ctx(&root),
                read_file::NAME,
                ActionInput(json!({"project": "docs", "path": "README.md"})),
                &AlwaysAllow,
            )
            .await;
        assert!(
            docs_result.is_ok(),
            "docs allows project.read_file independently of obc's permissions"
        );
    }

    #[tokio::test]
    async fn project_information_and_list_files_are_project_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write_project(&root.join("obc"), "obc", "", &[("README.md", "obc")]);
        write_project(
            &root.join("docs"),
            "docs",
            "",
            &[("README.md", "docs"), ("extra.md", "x")],
        );

        let registry = build(&root, Arc::new(AlwaysDeny)).await;

        let (_, info) = registry
            .execute(
                &ctx(&root),
                project_information::NAME,
                ActionInput(json!({"project": "obc"})),
                &AlwaysDeny,
            )
            .await;
        let info = info.unwrap();
        assert_eq!(info.data["information"]["name"], "obc");

        let (_, files) = registry
            .execute(
                &ctx(&root),
                list_project_files::NAME,
                ActionInput(json!({"project": "docs"})),
                &AlwaysDeny,
            )
            .await;
        let files = files.unwrap();
        assert_eq!(files.data["count"], 2);
    }

    #[tokio::test]
    async fn unavailable_project_is_reported_not_silently_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cfg = crate::WorkspaceConfig::parse(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  ghost:\n    path: ./ghost\n",
        )
        .unwrap();
        write_project(
            &root.join("obc"),
            "obc",
            "",
            &[("README.md", "watchdog obc")],
        );
        let project_registry = Arc::new(
            crate::ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg)
                .unwrap(),
        );
        let mut registry = ActionRegistry::new();
        let workspace_runtime =
            WorkspaceRuntime::new(project_registry, Arc::new(AlwaysDeny)).unwrap();
        register_all(&mut registry, workspace_runtime).unwrap();

        let (_, result) = registry
            .execute(
                &ctx(&root),
                search::NAME,
                ActionInput(json!({"projects": ["obc", "ghost"], "query": "watchdog"})),
                &AlwaysDeny,
            )
            .await;
        let output = result.unwrap();
        let unavailable = output.data["unavailable_projects"].as_array().unwrap();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0]["project"], "ghost");
        // obc's real result must still be present despite ghost failing.
        assert!(
            output.data["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["project"] == "obc")
        );
    }

    #[tokio::test]
    async fn workspace_information_reports_registered_and_unavailable_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cfg = crate::WorkspaceConfig::parse(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  ghost:\n    path: ./ghost\ndefaults:\n  project: obc\n",
        )
        .unwrap();
        write_project(&root.join("obc"), "obc", "", &[]);
        let project_registry = Arc::new(
            crate::ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg)
                .unwrap(),
        );
        let mut registry = ActionRegistry::new();
        let workspace_runtime =
            WorkspaceRuntime::new(project_registry, Arc::new(AlwaysDeny)).unwrap();
        register_all(&mut registry, workspace_runtime).unwrap();

        let (_, result) = registry
            .execute(
                &ctx(&root),
                information::NAME,
                ActionInput::empty(),
                &AlwaysDeny,
            )
            .await;
        let output = result.unwrap();
        assert_eq!(output.data["default_project"], "obc");
        assert_eq!(output.data["project_count"], 2);
        assert_eq!(output.data["available_project_count"], 1);
        assert_eq!(
            output.data["unavailable_projects"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
