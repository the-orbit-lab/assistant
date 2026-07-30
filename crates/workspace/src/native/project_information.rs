use orbit_actions::native::information;
use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::runtime::WorkspaceRuntime;

pub const NAME: &str = "workspace.project_information";

#[derive(Debug, Deserialize)]
struct Input {
    project: String,
}

pub struct WorkspaceProjectInformationAction {
    pub runtime: WorkspaceRuntime,
}

#[async_trait::async_trait]
impl Action for WorkspaceProjectInformationAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Report one registered project's own metadata, commands, and \
                effective permissions -- delegates to that project's own project.information."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered project name or alias" }
                },
                "required": ["project"],
                "additionalProperties": false
            }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        serde_json::from_value::<Input>(input.clone()).map_err(|e| {
            OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            }
        })?;
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: Input =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        let entry = self.runtime.require_available(&parsed.project)?;
        let output = self
            .runtime
            .call_project_action(entry, information::NAME, json!({}))
            .await?;

        Ok(ActionOutput::new(json!({
            "project": entry.name,
            "information": output.data,
        })))
    }
}
