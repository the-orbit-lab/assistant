use orbit_actions::native::read_file;
use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::budget::MAX_READ_BYTES_PER_PROJECT;
use crate::runtime::WorkspaceRuntime;
use crate::source::WorkspaceSourceReference;

pub const NAME: &str = "workspace.read_file";

#[derive(Debug, Deserialize)]
struct Input {
    project: String,
    path: String,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    truncate: bool,
}

pub struct WorkspaceReadFileAction {
    pub runtime: WorkspaceRuntime,
}

#[async_trait::async_trait]
impl Action for WorkspaceReadFileAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Read a single file from one registered project, subject to that \
                project's own root/include/exclude/size security boundary -- delegates to that \
                project's own project.read_file. A project can never read another project's \
                files through this action."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Registered project name or alias" },
                    "path": { "type": "string", "description": "Path relative to that project's own root" },
                    "max_bytes": { "type": "integer", "minimum": 1 },
                    "truncate": {
                        "type": "boolean",
                        "description": "Return the first max_bytes instead of failing when the file is larger."
                    }
                },
                "required": ["project", "path"],
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
        let max_bytes = parsed
            .max_bytes
            .unwrap_or(MAX_READ_BYTES_PER_PROJECT)
            .min(MAX_READ_BYTES_PER_PROJECT);

        let output = self
            .runtime
            .call_project_action(
                entry,
                read_file::NAME,
                json!({
                    "path": parsed.path,
                    "max_bytes": max_bytes,
                    "truncate": parsed.truncate,
                }),
            )
            .await?;

        let workspace_sources: Vec<WorkspaceSourceReference> = output
            .sources
            .iter()
            .map(|s| WorkspaceSourceReference::new(entry.name.clone(), s.clone()))
            .collect();
        // Also carried as plain, project-prefixed SourceReferences (see
        // WorkspaceSourceReference::to_plain) so this flows through the
        // existing Agent source-aggregation/dedup/CLI-print pipeline with
        // no changes to orbit-core.
        let plain_sources = workspace_sources.iter().map(|s| s.to_plain()).collect();

        Ok(ActionOutput::new(json!({
            "project": entry.name,
            "path": output.data["path"],
            "size": output.data["size"],
            "truncated": output.data["truncated"],
            "content": output.data["content"],
            "sources": workspace_sources,
        }))
        .with_sources(plain_sources))
    }
}
