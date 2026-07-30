use std::time::Duration;

use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;

use tokio::process::Command;

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "command.run_configured";

/// Configured commands (`cargo test`, `cargo clippy`, ...) can legitimately
/// run for a while; this bounds how long Orbit will wait before treating
/// the command as hung rather than blocking forever.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
/// Captured stdout/stderr is bounded so a runaway command can't blow up
/// memory or flood the model's context.
const MAX_CAPTURED_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct RunCommandInput {
    name: String,
}

pub struct RunConfiguredCommandAction;

fn truncate_output(mut output: Vec<u8>) -> (String, bool) {
    let truncated = output.len() > MAX_CAPTURED_OUTPUT_BYTES;
    output.truncate(MAX_CAPTURED_OUTPUT_BYTES);
    (String::from_utf8_lossy(&output).into_owned(), truncated)
}

#[async_trait::async_trait]
impl Action for RunConfiguredCommandAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Run a single named command from the project's `commands` \
                configuration. Only pre-configured program+argument pairs can run — arbitrary \
                shell strings are never accepted."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Configured command name, e.g. `test`" }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            default_permission: Permission::Ask,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        let parsed: RunCommandInput =
            serde_json::from_value(input.clone()).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;
        if parsed.name.trim().is_empty() {
            return Err(OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: "name must not be empty".to_string(),
            });
        }
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: RunCommandInput =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        let def = ctx.config.commands.get(&parsed.name).ok_or_else(|| {
            OrbitError::CommandNotConfigured {
                name: parsed.name.clone(),
            }
        })?;

        let mut command = Command::new(&def.program);
        command
            .args(&def.args)
            .current_dir(&ctx.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let started = std::time::Instant::now();
        let child = command
            .spawn()
            .map_err(|e| OrbitError::CommandExecutionFailed {
                name: parsed.name.clone(),
                reason: e.to_string(),
            })?;

        let output = tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| OrbitError::CommandExecutionFailed {
                name: parsed.name.clone(),
                reason: format!(
                    "command did not finish within {}s",
                    COMMAND_TIMEOUT.as_secs()
                ),
            })?
            .map_err(|e| OrbitError::CommandExecutionFailed {
                name: parsed.name.clone(),
                reason: e.to_string(),
            })?;

        let duration_ms = started.elapsed().as_millis();
        let (stdout, stdout_truncated) = truncate_output(output.stdout);
        let (stderr, stderr_truncated) = truncate_output(output.stderr);

        Ok(ActionOutput::new(json!({
            "name": parsed.name,
            "program": def.program,
            "args": def.args,
            "exit_code": output.status.code(),
            "success": output.status.success(),
            "duration_ms": duration_ms,
            "stdout": stdout,
            "stdout_truncated": stdout_truncated,
            "stderr": stderr,
            "stderr_truncated": stderr_truncated,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::context;

    #[tokio::test]
    async fn runs_a_configured_command() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let ctx = context(
            &root,
            "version: 1\nproject:\n  name: demo\ncommands:\n  greet:\n    program: echo\n    args: [hello]\n",
        );
        let out = RunConfiguredCommandAction
            .execute(&ctx, ActionInput(json!({"name": "greet"})))
            .await
            .unwrap();
        assert_eq!(out.data["success"], true);
        assert!(out.data["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn rejects_unconfigured_command_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let ctx = context(&root, "version: 1\nproject:\n  name: demo\n");
        let err = RunConfiguredCommandAction
            .execute(&ctx, ActionInput(json!({"name": "does-not-exist"})))
            .await
            .unwrap_err();
        assert!(matches!(err, OrbitError::CommandNotConfigured { .. }));
    }

    #[tokio::test]
    async fn ignores_injected_program_and_args_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let ctx = context(
            &root,
            "version: 1\nproject:\n  name: demo\ncommands:\n  greet:\n    program: echo\n    args: [safe]\n",
        );
        let out = RunConfiguredCommandAction
            .execute(
                &ctx,
                ActionInput(json!({"name": "greet", "program": "rm", "args": ["-rf", "/"]})),
            )
            .await
            .unwrap();
        assert_eq!(out.data["program"], "echo");
        assert_eq!(out.data["stdout"].as_str().unwrap().trim(), "safe");
    }
}
