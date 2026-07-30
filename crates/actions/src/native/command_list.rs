use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "command.list";

pub struct CommandListAction;

#[async_trait::async_trait]
impl Action for CommandListAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "List commands configured for this project and the permission each \
                requires."
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
        let run_permission = ctx
            .config
            .effective_permission(crate::native::command_run::NAME, Permission::Ask);
        let commands: Vec<Value> = ctx
            .config
            .commands
            .iter()
            .map(|(name, def)| {
                json!({
                    "name": name,
                    "program": def.program,
                    "args": def.args,
                    "permission": run_permission.to_string(),
                })
            })
            .collect();

        Ok(ActionOutput::new(json!({
            "count": commands.len(),
            "commands": commands,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::context;

    #[tokio::test]
    async fn lists_configured_commands_with_permission() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let ctx = context(
            &root,
            "version: 1\nproject:\n  name: demo\ncommands:\n  test:\n    program: cargo\n    args: [test]\npermissions:\n  command.run_configured: allow\n",
        );
        let out = CommandListAction
            .execute(&ctx, ActionInput::empty())
            .await
            .unwrap();
        let commands = out.data["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["permission"], "allow");
    }
}
