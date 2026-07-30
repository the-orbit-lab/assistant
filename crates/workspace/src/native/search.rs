use orbit_actions::native::search;
use orbit_actions::{Action, ActionContext};
use orbit_core::{ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::budget::{
    MAX_EXCERPT_BYTES_PER_RESULT, MAX_PROJECTS_PER_REQUEST, MAX_RESULTS_PER_PROJECT,
    MAX_TOTAL_CONTEXT_BYTES, truncate_bytes,
};
use crate::runtime::WorkspaceRuntime;
use crate::source::WorkspaceSourceReference;

pub const NAME: &str = "workspace.search";

#[derive(Debug, Deserialize)]
struct Input {
    /// Required and non-empty on purpose: this action never implicitly
    /// searches "every project" for an ambiguous request -- the caller
    /// (deterministic retrieval, an explicit `--projects` flag, or a
    /// model that was only given specific project tools) always names
    /// what it wants searched.
    projects: Vec<String>,
    query: String,
    #[serde(default)]
    limit_per_project: Option<usize>,
}

pub struct WorkspaceSearchAction {
    pub runtime: WorkspaceRuntime,
}

#[async_trait::async_trait]
impl Action for WorkspaceSearchAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Deterministic local search across one or more named registered \
                projects. Never scans every project implicitly -- always name which projects \
                to search. Returns ranked, project-scoped, sourced results."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "projects": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Registered project names or aliases to search"
                    },
                    "query": { "type": "string" },
                    "limit_per_project": { "type": "integer", "minimum": 1, "maximum": MAX_RESULTS_PER_PROJECT }
                },
                "required": ["projects", "query"],
                "additionalProperties": false
            }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        let parsed: Input =
            serde_json::from_value(input.clone()).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;
        if parsed.projects.is_empty() {
            return Err(OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: "projects must name at least one registered project".to_string(),
            });
        }
        if parsed.projects.len() > MAX_PROJECTS_PER_REQUEST {
            return Err(OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: format!(
                    "at most {MAX_PROJECTS_PER_REQUEST} projects can be searched in one request"
                ),
            });
        }
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
        _ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: Input =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        // An unknown project name is a caller mistake -- fail loudly and
        // immediately rather than silently searching a subset.
        let entries = self
            .runtime
            .project_registry
            .resolve_projects(&parsed.projects)?;
        let limit = parsed
            .limit_per_project
            .unwrap_or(MAX_RESULTS_PER_PROJECT)
            .min(MAX_RESULTS_PER_PROJECT);

        let mut results: Vec<Value> = Vec::new();
        let mut unavailable: Vec<Value> = Vec::new();
        let mut workspace_sources: Vec<WorkspaceSourceReference> = Vec::new();
        let mut total_bytes = 0usize;

        for entry in entries {
            if !entry.available {
                unavailable.push(json!({
                    "project": entry.name,
                    "error": entry.error,
                }));
                continue;
            }

            let output = self
                .runtime
                .call_project_action(
                    entry,
                    search::NAME,
                    json!({ "query": parsed.query, "limit": limit }),
                )
                .await?;

            let project_results = output.data["results"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for (result, source) in project_results.into_iter().zip(output.sources.iter()) {
                if total_bytes >= MAX_TOTAL_CONTEXT_BYTES {
                    break;
                }
                let excerpt = result["excerpt"].as_str().unwrap_or_default();
                let excerpt = truncate_bytes(excerpt, MAX_EXCERPT_BYTES_PER_RESULT);
                total_bytes += excerpt.len();

                results.push(json!({
                    "project": entry.name,
                    "path": result["path"],
                    "line_start": result["line_start"],
                    "line_end": result["line_end"],
                    "section": result["section"],
                    "excerpt": excerpt,
                    "score": result["score"],
                }));
                workspace_sources.push(WorkspaceSourceReference::new(
                    entry.name.clone(),
                    source.clone(),
                ));
            }
        }

        let plain_sources = workspace_sources.iter().map(|s| s.to_plain()).collect();

        Ok(ActionOutput::new(json!({
            "query": parsed.query,
            "count": results.len(),
            "results": results,
            "unavailable_projects": unavailable,
            "sources": workspace_sources,
        }))
        .with_sources(plain_sources))
    }
}
