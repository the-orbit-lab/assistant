use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use orbit_project::discover_files;
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "project.information";

/// Reports project identity, configured commands, effective permissions,
/// and discovered-file count. `known_actions` is a snapshot of every other
/// registered action's descriptor, taken once at startup after they are
/// all registered, so this action can report the registry's own state
/// without holding a reference back to the registry.
pub struct InformationAction {
    known_actions: Vec<ActionDescriptor>,
}

impl InformationAction {
    pub fn new(known_actions: Vec<ActionDescriptor>) -> Self {
        Self { known_actions }
    }
}

#[async_trait::async_trait]
impl Action for InformationAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Report project metadata, root, configuration location, active model \
                provider, configured commands, effective permissions, and discovered-file count."
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
        ctx: &ActionContext,
        _input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let discovered = discover_files(&ctx.root, &ctx.config)?;

        let commands: Value = ctx
            .config
            .commands
            .iter()
            .map(|(name, def)| {
                json!({
                    "name": name,
                    "program": def.program,
                    "args": def.args,
                })
            })
            .collect();

        let permissions: Value = self
            .known_actions
            .iter()
            .map(|descriptor| {
                let effective = ctx
                    .config
                    .effective_permission(&descriptor.name, descriptor.default_permission);
                json!({ "action": descriptor.name, "permission": effective.to_string() })
            })
            .collect();

        let data = json!({
            "name": ctx.config.project.name,
            "type": ctx.config.project.project_type,
            "description": ctx.config.project.description,
            "root": ctx.root.display().to_string(),
            "config_path": ctx.config_path.display().to_string(),
            "provider": ctx.config.model.provider,
            "model": ctx.config.model.model,
            "endpoint": ctx.config.model.endpoint,
            "commands": commands,
            "permissions": permissions,
            "discovered_file_count": discovered.len(),
            "mcp_expose": ctx.config.mcp.expose,
            "mcp_servers": ctx.config.mcp.servers.keys().cloned().collect::<Vec<_>>(),
        });

        Ok(ActionOutput::new(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native;
    use crate::registry::ActionRegistry;
    use crate::test_support::default_context;

    #[tokio::test]
    async fn reports_project_metadata_and_file_count() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("README.md"), "hi").unwrap();
        let ctx = default_context(&root);

        let mut registry = ActionRegistry::new();
        native::register_all(&mut registry).unwrap();
        let action = registry.get(NAME).unwrap();
        let out = action.execute(&ctx, ActionInput::empty()).await.unwrap();

        assert_eq!(out.data["name"], "demo");
        assert_eq!(out.data["discovered_file_count"], 1);
        assert!(out.data["permissions"].as_array().unwrap().len() >= 5);
    }
}
