use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde_json::{Value, json};

use crate::runtime::WorkspaceRuntime;

pub const NAME: &str = "workspace.list_projects";

pub struct WorkspaceListProjectsAction {
    pub runtime: WorkspaceRuntime,
}

#[async_trait::async_trait]
impl Action for WorkspaceListProjectsAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "List every project registered in this workspace: name, aliases, \
                relative path, description, and availability."
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
        let entries: Vec<Value> = self
            .runtime
            .project_registry
            .list_projects()
            .into_iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "aliases": p.aliases,
                    "path": p.configured_path,
                    "description": p.description,
                    "available": p.available,
                    "error": p.error,
                })
            })
            .collect();

        Ok(ActionOutput::new(json!({
            "count": entries.len(),
            "projects": entries,
        })))
    }
}
