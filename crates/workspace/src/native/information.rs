use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde_json::{Value, json};

use crate::runtime::WorkspaceRuntime;

pub const NAME: &str = "workspace.information";

pub struct WorkspaceInformationAction {
    pub runtime: WorkspaceRuntime,
}

#[async_trait::async_trait]
impl Action for WorkspaceInformationAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Report workspace identity, root, configuration location, default \
                project, registered project count, relationships, and unavailable projects."
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, _input: &Value) -> Result<(), OrbitError> {
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: &ActionContext,
        _input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let registry = &self.runtime.project_registry;
        let projects = registry.list_projects();
        let unavailable: Vec<Value> = projects
            .iter()
            .filter(|p| !p.available)
            .map(|p| json!({ "project": p.name, "error": p.error }))
            .collect();
        let relationships: Vec<Value> = registry
            .relationships()
            .iter()
            .map(|r| json!({ "source": r.source, "target": r.target, "type": r.relationship_type }))
            .collect();

        Ok(ActionOutput::new(json!({
            "name": registry.config.workspace.name,
            "description": registry.config.workspace.description,
            "root": registry.workspace_root.display().to_string(),
            "config_path": registry.workspace_config_path.display().to_string(),
            "default_project": registry.default_project().map(|p| p.name.clone()),
            "project_count": projects.len(),
            "available_project_count": projects.iter().filter(|p| p.available).count(),
            "unavailable_projects": unavailable,
            "relationships": relationships,
        })))
    }
}
