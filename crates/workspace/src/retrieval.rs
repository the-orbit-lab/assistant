//! Deterministic, pre-model retrieval for workspace-scoped questions --
//! the multi-project analogue of `orbit-agent`'s single-project
//! `retrieval` module, built on the same principle: don't depend on a
//! small local model to decide, on its own, which project(s) a question
//! is about and to call the right tools for them.
//!
//! Everything here is deterministic text matching (exact registered
//! names/aliases, a fixed stopword list) -- there is no fuzzy or semantic
//! routing. A question that doesn't resolve to a specific project falls
//! back to the workspace's own `defaults.project` (visibly, see
//! `ResolvedScope::used_default`) or, failing that, to workspace-level
//! information only. The model is never given a path it didn't ask a
//! resolvable project for, and this step never touches more than
//! [`crate::budget::MAX_PROJECTS_PER_REQUEST`] projects.

use std::collections::HashSet;

use orbit_actions::{ActionContext, ActionRegistry};
use orbit_core::{
    ConfirmationProvider, EventEmitter, EventPayload, ExecutionRecord, Message,
    RetrievalConfidence, SourceReference, ToolCall,
};
use orbit_retrieval::agenda::EvidenceAgenda;
use orbit_retrieval::pipeline::{Pipeline, PipelineInput};
use orbit_retrieval::select::SelectionLimits;
use serde_json::{Value, json};

use crate::budget::MAX_PROJECTS_PER_REQUEST;
use crate::config::normalize_identifier;
use crate::registry::ProjectRegistry;

/// Which project(s) a question resolved to, and whether that came from an
/// explicit selector, an exact name/alias mention in the text, or the
/// workspace's configured default -- the caller uses this to say plainly
/// which project(s) were used, per "the selected project must be visible
/// in the response."
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedScope {
    /// Empty means workspace-level only: no specific project.
    pub projects: Vec<String>,
    pub used_default: bool,
}

/// Deliberately narrow: matches "what/which projects...", "list
/// projects", "available projects" -- but not "what does *the OBC
/// project* do", which names a specific (singular) project and must
/// resolve to it, not fall through to a workspace-wide listing just
/// because the word "project" appears.
fn is_workspace_listing_question(question: &str) -> bool {
    let q = question.to_lowercase();
    q.contains("what projects")
        || q.contains("which projects")
        || q.contains("list projects")
        || q.contains("list the projects")
        || q.contains("available projects")
        || q.contains("projects are available")
        || q.contains("projects do you have")
        || q.contains("projects exist")
}

/// Scan `question` for exact registered project names or aliases (up to
/// three-word phrases, to catch aliases like "mission analysis"), longest
/// match first at each position, in left-to-right order of first mention.
pub fn find_project_mentions(question: &str, registry: &ProjectRegistry) -> Vec<String> {
    let words: Vec<&str> = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    let mut candidates: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for entry in registry.list_projects() {
        candidates.insert(normalize_identifier(&entry.name), entry.name.clone());
        for alias in &entry.aliases {
            candidates.insert(normalize_identifier(alias), entry.name.clone());
        }
    }

    struct Hit {
        start: usize,
        project: String,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for start in 0..words.len() {
        let max_len = 3.min(words.len() - start);
        for len in (1..=max_len).rev() {
            let phrase = normalize_identifier(&words[start..start + len].join("-"));
            if let Some(project) = candidates.get(&phrase) {
                hits.push(Hit {
                    start,
                    project: project.clone(),
                });
                break;
            }
        }
    }
    hits.sort_by_key(|h| h.start);

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for hit in hits {
        if seen.insert(hit.project.clone()) {
            found.push(hit.project);
        }
    }
    found
}

/// Which project(s) a question is about. Deterministic throughout: an
/// explicit selection, else an exact name/alias mentioned in the text,
/// else the workspace default.
fn resolve_scope(
    question: &str,
    explicit: Option<&[String]>,
    registry: &ProjectRegistry,
) -> ResolvedScope {
    if let Some(explicit) = explicit {
        return ResolvedScope {
            projects: explicit.to_vec(),
            used_default: false,
        };
    }
    if is_workspace_listing_question(question) {
        return ResolvedScope {
            projects: Vec::new(),
            used_default: false,
        };
    }
    let mentions = find_project_mentions(question, registry);
    if !mentions.is_empty() {
        return ResolvedScope {
            projects: mentions,
            used_default: false,
        };
    }
    if let Some(default) = registry.default_project() {
        return ResolvedScope {
            projects: vec![default.name.clone()],
            used_default: true,
        };
    }
    ResolvedScope {
        projects: Vec::new(),
        used_default: false,
    }
}

/// Overview-doc ranking, mirroring `orbit-agent::retrieval`'s heuristic:
/// README-like files first, then instructions files, then anything
/// spec/overview/architecture-shaped, at most one directory deep.
fn score_overview_candidate(path: &str) -> Option<u32> {
    if path.matches('/').count() > 1 {
        return None;
    }
    let basename = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    match basename.as_str() {
        "readme.md" | "readme" => Some(100),
        "claude.md" => Some(90),
        _ if basename.contains("overview") => Some(80),
        _ if basename.contains("spec") => Some(70),
        _ if basename.contains("architecture") => Some(60),
        _ => None,
    }
}

const MAX_OVERVIEW_READS_PER_PROJECT: usize = 2;
const OVERVIEW_READ_BYTES: u64 = 8_000;

/// Build the evidence agenda for one project in the scope.
///
/// Each registered project keeps its own root, its own configuration,
/// and its own security boundary, so the corpus is discovered per
/// project rather than merged. A project whose configuration failed to
/// load contributes nothing instead of failing the turn.
fn plan_project_evidence(
    project_registry: &ProjectRegistry,
    project: &str,
    question: &str,
    context_terms: &[String],
    scope: &[String],
) -> Option<EvidenceAgenda> {
    let entry = project_registry.get_project(project)?;
    let context = entry.action_context().ok()?;
    let files = orbit_project::discover_files(&context.root, &context.config).ok()?;

    let pipeline = Pipeline::new(files);
    Some(orbit_retrieval::agenda::build(
        &pipeline,
        &PipelineInput {
            question: question.to_string(),
            context_terms: context_terms.to_vec(),
            recent_paths: Vec::new(),
            target_projects: scope.to_vec(),
        },
        &SelectionLimits::default(),
    ))
}

/// What one workspace-scoped retrieval step produced.
#[derive(Debug, Clone, Default)]
pub struct RetrievalOutcome {
    pub scope: ResolvedScope,
    pub sources: Vec<SourceReference>,
    pub records: Vec<ExecutionRecord>,
    /// Terms actually searched for, for `--verbose` diagnostics.
    pub terms: Vec<String>,
}

impl RetrievalOutcome {
    /// Judged on distinct `(project, path)` pairs, so several matches
    /// inside one document do not look like corroboration.
    pub fn confidence(&self) -> RetrievalConfidence {
        let files: HashSet<_> = self.sources.iter().map(|s| s.path.clone()).collect();
        RetrievalConfidence::from_distinct_files(files.len())
    }
}

/// Run the deterministic retrieval step for one workspace-scoped question,
/// appending synthetic `workspace.*` tool-call/tool-result message pairs
/// to `history` exactly as a model-initiated call would produce -- through
/// the same `ActionRegistry::execute`, so permission enforcement and
/// execution records are identical either way.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    registry: &ActionRegistry,
    action_ctx: &ActionContext,
    project_registry: &ProjectRegistry,
    confirmation: &dyn ConfirmationProvider,
    question: &str,
    explicit_projects: Option<&[String]>,
    history: &mut Vec<Message>,
    events: &EventEmitter,
    context_terms: &[String],
) -> RetrievalOutcome {
    let scope = resolve_scope(question, explicit_projects, project_registry);
    // Which retrieval implementation this run used. Named explicitly
    // because the reported failure was not a ranking bug -- it was this
    // path silently running a *different* retrieval from the one every
    // test exercised, with nothing in the output saying so.
    tracing::debug!(
        retrieval_implementation = "orbit-retrieval pipeline (workspace)",
        scope = ?scope.projects,
        "deterministic retrieval starting"
    );
    let mut sources = Vec::new();
    let mut records = Vec::new();
    let mut next_call_id = 0u32;

    events.emit(EventPayload::RetrievalStarted {
        scope: scope.projects.clone(),
    });

    if scope.projects.is_empty() {
        call(
            registry,
            action_ctx,
            confirmation,
            history,
            "workspace.information",
            json!({}),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
        call(
            registry,
            action_ctx,
            confirmation,
            history,
            "workspace.list_projects",
            json!({}),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
        events.emit(EventPayload::RetrievalCompleted {
            scope: scope.projects.clone(),
            action_count: records.len(),
            source_count: sources.len(),
        });
        return RetrievalOutcome {
            scope,
            sources,
            records,
            terms: Vec::new(),
        };
    }

    let bounded_scope: Vec<String> = scope
        .projects
        .iter()
        .take(MAX_PROJECTS_PER_REQUEST)
        .cloned()
        .collect();

    for project in &bounded_scope {
        call(
            registry,
            action_ctx,
            confirmation,
            history,
            "workspace.project_information",
            json!({ "project": project }),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
    }

    // Project names are dropped from the query: they match nearly every
    // file inside their own project and would drown out the real subject.
    let analysis = analyze_question(question, &bounded_scope, project_registry, context_terms);
    let terms = analysis.all_terms();
    let mut any_evidence = false;

    // The same evidence planner the single-project path uses. This
    // module previously ran `workspace.search` and read whatever it
    // ranked highest, which is why `orbit chat` in workspace mode kept
    // answering a question about `SessionRuntime` out of
    // `tests/grounding.rs` while every unit test of the new pipeline
    // passed: the pipeline was never on this path at all.
    //
    // The planner decides; this function only translates its decisions
    // into `workspace.*` action calls, so the two front ends cannot
    // drift apart again.
    if !terms.is_empty() {
        for project in &bounded_scope {
            let Some(agenda) = plan_project_evidence(
                project_registry,
                project,
                question,
                context_terms,
                &bounded_scope,
            ) else {
                continue;
            };
            if agenda.is_empty() {
                continue;
            }
            any_evidence = true;
            orbit_retrieval::agenda::trace(&agenda, "orbit-retrieval pipeline", Some(project));
            if let Some(report) = &agenda.diagnostics {
                tracing::debug!(project = %project, "evidence selection:\n{report}");
            }

            // Whole files first, then the exact declaration spans: a
            // model answers from what is nearest its question.
            for read in &agenda.reads {
                call(
                    registry,
                    action_ctx,
                    confirmation,
                    history,
                    "workspace.read_file",
                    json!({
                        "project": project,
                        "path": read.path.to_string_lossy(),
                        "max_bytes": OVERVIEW_READ_BYTES,
                        "truncate": true,
                    }),
                    &mut sources,
                    &mut records,
                    &mut next_call_id,
                    events,
                )
                .await;
            }

            for span in &agenda.symbol_spans {
                call(
                    registry,
                    action_ctx,
                    confirmation,
                    history,
                    "workspace.read_file",
                    json!({
                        "project": project,
                        "path": span.path.to_string_lossy(),
                        "line_start": span.line_start,
                        "line_end": span.line_end,
                        "max_bytes": OVERVIEW_READ_BYTES,
                        "truncate": true,
                    }),
                    &mut sources,
                    &mut records,
                    &mut next_call_id,
                    events,
                )
                .await;
            }

            if let Some(anchor) = agenda.anchor {
                history.push(Message::system(anchor));
            }
        }
    }

    // Nothing matched: fall back to whatever reads like an overview,
    // which is what answers "what does this project do".
    if !any_evidence {
        for project in &bounded_scope {
            for path in
                overview_candidates(registry, action_ctx, confirmation, project, events).await
            {
                call(
                    registry,
                    action_ctx,
                    confirmation,
                    history,
                    "workspace.read_file",
                    json!({ "project": project, "path": path, "max_bytes": OVERVIEW_READ_BYTES, "truncate": true }),
                    &mut sources,
                    &mut records,
                    &mut next_call_id,
                    events,
                )
                .await;
            }
        }
    }

    events.emit(EventPayload::RetrievalCompleted {
        scope: scope.projects.clone(),
        action_count: records.len(),
        source_count: sources.len(),
    });

    RetrievalOutcome {
        scope,
        sources,
        records,
        terms,
    }
}

/// Analyze `question` into search terms, excluding the names and aliases
/// of the projects already being searched.
fn analyze_question(
    question: &str,
    scope_names: &[String],
    registry: &ProjectRegistry,
    context_terms: &[String],
) -> orbit_project::AnalyzedQuery {
    let mut excluded: HashSet<String> = HashSet::new();
    for name in scope_names {
        if let Ok(entry) = registry.resolve_project(name) {
            excluded.extend(orbit_project::content_terms(&entry.name));
            for alias in &entry.aliases {
                excluded.extend(orbit_project::content_terms(alias));
            }
        }
    }

    let mut analysis = orbit_project::analyze_with_context(question, context_terms);
    analysis.terms.retain(|t| !excluded.contains(t));
    analysis.context_terms.retain(|t| !excluded.contains(t));
    analysis
}

async fn overview_candidates(
    registry: &ActionRegistry,
    action_ctx: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
    project: &str,
    events: &EventEmitter,
) -> Vec<String> {
    let (_, result) = registry
        .execute_observed(
            action_ctx,
            "workspace.list_project_files",
            orbit_core::ActionInput(json!({ "project": project })),
            confirmation,
            events,
            &events.next_execution_id(),
        )
        .await;
    let Ok(output) = result else {
        return Vec::new();
    };
    let Some(files) = output.data["files"].as_array() else {
        return Vec::new();
    };

    let mut scored: Vec<(u32, String)> = files
        .iter()
        .filter_map(|f| f["path"].as_str())
        .filter_map(|path| score_overview_candidate(path).map(|score| (score, path.to_string())))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_OVERVIEW_READS_PER_PROJECT)
        .map(|(_, path)| path)
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn call(
    registry: &ActionRegistry,
    action_ctx: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
    history: &mut Vec<Message>,
    name: &str,
    arguments: Value,
    sources: &mut Vec<SourceReference>,
    records: &mut Vec<ExecutionRecord>,
    next_call_id: &mut u32,
    events: &EventEmitter,
) -> Option<Value> {
    let (record, result) = registry
        .execute_observed(
            action_ctx,
            name,
            orbit_core::ActionInput(arguments.clone()),
            confirmation,
            events,
            &events.next_execution_id(),
        )
        .await;
    records.push(record);

    match result {
        Ok(output) => {
            sources.extend(output.sources.iter().cloned());
            let id = format!("orbit_workspace_auto_{next_call_id}");
            *next_call_id += 1;
            history.push(Message::assistant_tool_calls(vec![ToolCall {
                id: id.clone(),
                name: name.to_string(),
                arguments,
            }]));
            history.push(Message::tool_result(&id, output.to_model_text()));
            tracing::debug!(
                action = name,
                total_sources = sources.len(),
                "workspace retrieval step executed"
            );
            Some(output.data)
        }
        Err(err) => {
            tracing::debug!(action = name, error = %err, "workspace retrieval step skipped");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(yaml: &str) -> ProjectRegistry {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cfg = crate::WorkspaceConfig::parse(yaml).unwrap();
        for name in cfg.projects.keys() {
            std::fs::create_dir_all(root.join(name).join(".orbit")).unwrap();
            std::fs::write(
                root.join(name).join(".orbit/project.yaml"),
                format!("version: 1\nproject:\n  name: {name}\n"),
            )
            .unwrap();
        }
        // Leak the tempdir so its path stays valid for the test's duration.
        std::mem::forget(tmp);
        ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap()
    }

    fn workspace_yaml() -> &'static str {
        "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n    aliases: [\"onboard computer\"]\n  docs:\n    path: ./docs\n  mission-tools:\n    path: ./mission-tools\n    aliases: [\"mission analysis\"]\ndefaults:\n  project: docs\n"
    }

    #[test]
    fn finds_single_exact_project_mention() {
        let registry = registry_with(workspace_yaml());
        let mentions = find_project_mentions("What does the OBC project do?", &registry);
        assert_eq!(mentions, vec!["obc".to_string()]);
    }

    #[test]
    fn finds_multi_word_alias_mentions() {
        let registry = registry_with(workspace_yaml());
        let mentions = find_project_mentions(
            "In mission-tools, how is link budget calculated?",
            &registry,
        );
        assert_eq!(mentions, vec!["mission-tools".to_string()]);
    }

    #[test]
    fn finds_two_projects_in_reading_order() {
        let registry = registry_with(workspace_yaml());
        let mentions =
            find_project_mentions("Compare docs and OBC regarding STM32 selection", &registry);
        assert_eq!(mentions, vec!["docs".to_string(), "obc".to_string()]);
    }

    #[test]
    fn scope_falls_back_to_default_project_when_nothing_is_mentioned() {
        let registry = registry_with(workspace_yaml());
        let scope = resolve_scope("What is the release process?", None, &registry);
        assert_eq!(scope.projects, vec!["docs".to_string()]);
        assert!(scope.used_default);
    }

    #[test]
    fn scope_is_workspace_level_for_a_listing_question() {
        let registry = registry_with(workspace_yaml());
        let scope = resolve_scope("What projects are available?", None, &registry);
        assert!(scope.projects.is_empty());
        assert!(!scope.used_default);
    }

    #[test]
    fn explicit_projects_bypass_text_scanning_entirely() {
        let registry = registry_with(workspace_yaml());
        let explicit = vec!["mission-tools".to_string()];
        let scope = resolve_scope("What does the OBC project do?", Some(&explicit), &registry);
        assert_eq!(scope.projects, vec!["mission-tools".to_string()]);
    }

    /// A project already being searched must not also be a search term:
    /// its own name matches nearly every file inside it and would drown
    /// out the actual subject.
    #[test]
    fn the_query_excludes_the_names_of_the_projects_being_searched() {
        let registry = registry_with(workspace_yaml());
        let scope = vec!["docs".to_string(), "obc".to_string()];
        let analysis = analyze_question(
            "Compare docs and OBC regarding STM32 selection",
            &scope,
            &registry,
            &[],
        );
        let terms = analysis.all_terms();
        assert!(terms.contains(&"stm32".to_string()), "{terms:?}");
        assert!(!terms.contains(&"docs".to_string()), "{terms:?}");
        assert!(!terms.contains(&"obc".to_string()), "{terms:?}");
    }

    /// A pure overview question has no subject of its own, so there is
    /// nothing to search for and retrieval falls back to overview reads.
    #[test]
    fn a_pure_overview_question_yields_no_search_terms() {
        let registry = registry_with(workspace_yaml());
        let scope = vec!["obc".to_string()];
        let analysis = analyze_question("What does the OBC project do?", &scope, &registry, &[]);
        assert!(analysis.all_terms().is_empty(), "{analysis:?}");
    }

    /// A thin follow-up must inherit the previous turn's subject.
    #[test]
    fn a_follow_up_inherits_context_terms() {
        let registry = registry_with(workspace_yaml());
        let scope = vec!["obc".to_string()];
        let analysis = analyze_question(
            "Now explain how cancellation works.",
            &scope,
            &registry,
            &["session".to_string(), "runtime".to_string()],
        );
        let terms = analysis.all_terms();
        assert!(terms.contains(&"cancel".to_string()), "{terms:?}");
        assert!(terms.contains(&"session".to_string()), "{terms:?}");
    }

    async fn built_registry_with_content() -> (tempfile::TempDir, ProjectRegistry) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for (name, readme) in [
            (
                "obc",
                "# OBC\n\nThe onboard computer runs the flight software and watchdog.\n",
            ),
            (
                "docs",
                "# Docs\n\nCentral documentation. STM32 selection rationale: low power draw.\n",
            ),
        ] {
            std::fs::create_dir_all(root.join(name).join(".orbit")).unwrap();
            std::fs::write(
                root.join(name).join(".orbit/project.yaml"),
                format!(
                    "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n"
                ),
            )
            .unwrap();
            std::fs::write(root.join(name).join("README.md"), readme).unwrap();
        }
        let cfg = crate::WorkspaceConfig::parse(
            "version: 1\nworkspace:\n  name: Lab\nprojects:\n  obc:\n    path: ./obc\n  docs:\n    path: ./docs\n",
        )
        .unwrap();
        let registry =
            ProjectRegistry::load(root.clone(), root.join(".orbit/workspace.yaml"), cfg).unwrap();
        (tmp, registry)
    }

    #[tokio::test]
    async fn named_project_overview_question_only_retrieves_that_project() {
        let (_tmp, project_registry) = built_registry_with_content().await;
        let project_registry = std::sync::Arc::new(project_registry);
        let action_registry = crate::build_registry(
            project_registry.clone(),
            std::sync::Arc::new(orbit_core::AlwaysDeny),
        )
        .unwrap();
        let action_ctx = project_registry.workspace_action_context();

        let mut history = Vec::new();
        let retrieved = run(
            &action_registry,
            &action_ctx,
            &project_registry,
            &orbit_core::AlwaysDeny,
            "What does the OBC project do?",
            None,
            &mut history,
            &orbit_core::EventEmitter::null(),
            &[],
        )
        .await;
        #[allow(unused_variables)]
        let (scope, sources, records) = (retrieved.scope, retrieved.sources, retrieved.records);

        assert_eq!(scope.projects, vec!["obc".to_string()]);
        assert!(!sources.is_empty());
        assert!(
            sources
                .iter()
                .all(|s| s.path.to_string_lossy().starts_with("obc:"))
        );
        let all_text: String = history.iter().map(|m| m.content.clone()).collect();
        assert!(all_text.contains("watchdog"));
        assert!(
            !all_text.contains("STM32"),
            "docs must not be retrieved for an obc-only question"
        );
    }

    #[tokio::test]
    async fn comparison_question_searches_both_named_projects() {
        let (_tmp, project_registry) = built_registry_with_content().await;
        let project_registry = std::sync::Arc::new(project_registry);
        let action_registry = crate::build_registry(
            project_registry.clone(),
            std::sync::Arc::new(orbit_core::AlwaysDeny),
        )
        .unwrap();
        let action_ctx = project_registry.workspace_action_context();

        let mut history = Vec::new();
        let retrieved = run(
            &action_registry,
            &action_ctx,
            &project_registry,
            &orbit_core::AlwaysDeny,
            "Compare docs and OBC regarding STM32 selection",
            None,
            &mut history,
            &orbit_core::EventEmitter::null(),
            &[],
        )
        .await;
        #[allow(unused_variables)]
        let (scope, sources, records) = (retrieved.scope, retrieved.sources, retrieved.records);

        assert_eq!(scope.projects, vec!["docs".to_string(), "obc".to_string()]);
        let projects_in_sources: HashSet<_> = sources
            .iter()
            .map(|s| {
                s.path
                    .to_string_lossy()
                    .split(':')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(projects_in_sources.contains("docs"));
    }

    #[tokio::test]
    async fn listing_question_stays_workspace_level_with_no_project_scope() {
        let (_tmp, project_registry) = built_registry_with_content().await;
        let project_registry = std::sync::Arc::new(project_registry);
        let action_registry = crate::build_registry(
            project_registry.clone(),
            std::sync::Arc::new(orbit_core::AlwaysDeny),
        )
        .unwrap();
        let action_ctx = project_registry.workspace_action_context();

        let mut history = Vec::new();
        let retrieved = run(
            &action_registry,
            &action_ctx,
            &project_registry,
            &orbit_core::AlwaysDeny,
            "What projects are available?",
            None,
            &mut history,
            &orbit_core::EventEmitter::null(),
            &[],
        )
        .await;
        #[allow(unused_variables)]
        let (scope, sources, records) = (retrieved.scope, retrieved.sources, retrieved.records);

        assert!(scope.projects.is_empty());
        assert!(
            records
                .iter()
                .any(|r| r.action == "workspace.list_projects")
        );
    }
}
