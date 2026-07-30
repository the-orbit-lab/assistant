use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use orbit_project::{SearchOptions, discover_files, search_files};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "project.search";

const MAX_ALLOWED_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct SearchAction;

#[async_trait::async_trait]
impl Action for SearchAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Deterministic local search over allowed project files: filenames, \
                Markdown headings, and file content. Returns ranked, sourced results."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_ALLOWED_LIMIT }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        let parsed: SearchInput =
            serde_json::from_value(input.clone()).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;
        if parsed.query.trim().is_empty() {
            return Err(OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: "query must not be empty".to_string(),
            });
        }
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: SearchInput =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        let files = discover_files(&ctx.root, &ctx.config)?;
        let mut options = SearchOptions::default();
        if let Some(limit) = parsed.limit {
            options.limit = limit.min(MAX_ALLOWED_LIMIT);
        }
        let results = search_files(&files, &parsed.query, &options);

        let sources = results.iter().map(|r| r.source.clone()).collect();
        let entries: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "path": r.source.path.to_string_lossy(),
                    "line_start": r.source.line_start,
                    "line_end": r.source.line_end,
                    "section": r.source.section,
                    "excerpt": r.excerpt,
                    "score": r.score,
                })
            })
            .collect();

        Ok(ActionOutput::new(json!({
            "query": parsed.query,
            "count": entries.len(),
            "results": entries,
        }))
        .with_sources(sources))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::default_context;

    #[tokio::test]
    async fn returns_ranked_sourced_results() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("watchdog.md"),
            "# Watchdog\nkeeps the system alive\n",
        )
        .unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = SearchAction
            .execute(&ctx, ActionInput(json!({"query": "watchdog"})))
            .await
            .unwrap();
        // The query matches both the filename and the Markdown heading.
        assert_eq!(out.data["count"], 2);
        assert_eq!(out.sources.len(), 2);
        assert_eq!(out.sources[0].path, std::path::PathBuf::from("watchdog.md"));
    }

    #[test]
    fn validate_rejects_empty_query() {
        let err = SearchAction.validate(&json!({"query": "  "})).unwrap_err();
        assert!(matches!(err, OrbitError::InvalidActionInput { .. }));
    }

    #[test]
    fn validate_rejects_missing_query() {
        let err = SearchAction.validate(&json!({})).unwrap_err();
        assert!(matches!(err, OrbitError::InvalidActionInput { .. }));
    }
}
