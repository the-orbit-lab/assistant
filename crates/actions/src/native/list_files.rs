use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use orbit_project::discover_files;
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "project.list_files";

pub struct ListFilesAction;

#[async_trait::async_trait]
impl Action for ListFilesAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "List every project file allowed by the current include/exclude \
                configuration."
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
        let files = discover_files(&ctx.root, &ctx.config)?;
        tracing::debug!(root = %ctx.root.display(), discovered_file_count = files.len(), "discovered project files");
        let entries: Vec<Value> = files
            .iter()
            .map(|f| {
                json!({
                    "path": f.relative_path.to_string_lossy(),
                    "size": f.size,
                    "is_text": f.is_text,
                })
            })
            .collect();

        Ok(ActionOutput::new(json!({
            "count": entries.len(),
            "files": entries,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::default_context;

    #[tokio::test]
    async fn lists_allowed_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "hi").unwrap();
        std::fs::write(tmp.path().join(".env"), "SECRET=1").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ListFilesAction
            .execute(&ctx, ActionInput::empty())
            .await
            .unwrap();
        let files = out.data["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "README.md"));
        assert!(!files.iter().any(|f| f["path"] == ".env"));
    }
}
