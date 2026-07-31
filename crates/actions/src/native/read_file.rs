use std::path::PathBuf;

use orbit_core::{
    ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission, SourceReference,
};
use orbit_project::{read_allowed_file, read_allowed_file_truncated};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "project.read_file";

/// Hard ceiling on a single read, independent of any caller-supplied limit,
/// so a huge `max_bytes` value can't be used to blow the model context or
/// exhaust memory.
const MAX_ALLOWED_READ_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_READ_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    #[serde(default)]
    max_bytes: Option<u64>,
    /// Return the first `max_bytes` of an oversized file instead of
    /// failing. Off by default, so existing callers keep the strict
    /// behavior they rely on.
    #[serde(default)]
    truncate: bool,
}

pub struct ReadFileAction;

#[async_trait::async_trait]
impl Action for ReadFileAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Read a single allowed project file as UTF-8 text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the project root" },
                    "max_bytes": { "type": "integer", "minimum": 1 },
                    "truncate": {
                        "type": "boolean",
                        "description": "Return the first max_bytes instead of failing when the file is larger."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        serde_json::from_value::<ReadFileInput>(input.clone()).map_err(|e| {
            OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            }
        })?;
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: ReadFileInput =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        let limit = parsed
            .max_bytes
            .unwrap_or(DEFAULT_READ_BYTES)
            .min(MAX_ALLOWED_READ_BYTES);

        let relative = PathBuf::from(&parsed.path);
        let (content, truncated) = if parsed.truncate {
            read_allowed_file_truncated(&ctx.root, &ctx.config, &relative, limit)?
        } else {
            (
                read_allowed_file(&ctx.root, &ctx.config, &relative, limit)?,
                false,
            )
        };
        // Never log file content -- only that a read happened and its size.
        tracing::debug!(
            path = %parsed.path,
            bytes = content.len(),
            truncated,
            "project.read_file executed"
        );

        Ok(ActionOutput::new(json!({
            "path": parsed.path,
            "size": content.len(),
            // Stated explicitly so the model knows it is seeing part of a
            // file rather than all of it.
            "truncated": truncated,
            "content": content,
        }))
        .with_sources(vec![SourceReference::whole_file(relative)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::default_context;

    #[tokio::test]
    async fn reads_an_allowed_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "hello there").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ReadFileAction
            .execute(&ctx, ActionInput(json!({"path": "README.md"})))
            .await
            .unwrap();
        assert_eq!(out.data["content"], "hello there");
        assert_eq!(out.sources.len(), 1);
    }

    #[tokio::test]
    async fn rejects_traversal_via_the_action() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let err = ReadFileAction
            .execute(&ctx, ActionInput(json!({"path": "../../etc/passwd"})))
            .await
            .unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[test]
    fn validate_rejects_missing_path() {
        let err = ReadFileAction.validate(&json!({})).unwrap_err();
        assert!(matches!(err, OrbitError::InvalidActionInput { .. }));
    }
}
