use orbit_actions::{ActionContext, ActionRegistry};
use std::collections::HashSet;

use orbit_core::{
    ActionInput, ConfirmationProvider, EventEmitter, EventPayload, ExecutionRecord, Message,
    RetrievalConfidence, SourceReference, ToolCall,
};
use serde_json::{Value, json};

/// Question shapes broad enough that there's no obvious keyword to search
/// for -- "What does this repository do?" has nothing in it a keyword
/// search could use, so the model would otherwise have to *already* know
/// the project's own vocabulary before it could search for it.
const OVERVIEW_SUBJECTS: &[&str] = &[
    "this repo",
    "this project",
    "this codebase",
    "this application",
    "this system",
    "this tool",
];
const OVERVIEW_VERBS: &[&str] = &[
    "what does",
    "what is",
    "what's",
    "explain",
    "describe",
    "purpose of",
    "tell me about",
    "overview of",
    "give me an overview",
];

// Generous enough to cover most real READMEs whole (Orbit's own is ~5.8
// KB) while still being a hard bound, not "read the whole file".
const OVERVIEW_READ_BYTES: u64 = 12_000;
const MAX_OVERVIEW_READS: usize = 2;

/// How many of the strongest search hits are read in full.
///
/// A search excerpt is one line; answering "explain the session
/// architecture" needs the surrounding prose. Reading the top-ranked
/// files is what turns a list of matching lines into usable evidence,
/// and it is done here rather than asked of the user, because the
/// ranking already knows which files are most likely relevant.
const MAX_FOLLOW_UP_READS: usize = 3;
const FOLLOW_UP_READ_BYTES: u64 = 12_000;
/// Results considered when choosing which files to read in full.
const SEARCH_LIMIT: usize = 12;

/// What one deterministic retrieval step produced.
#[derive(Debug, Clone, Default)]
pub struct RetrievalOutcome {
    pub sources: Vec<SourceReference>,
    pub records: Vec<ExecutionRecord>,
    /// Terms actually searched for, for `--verbose` diagnostics.
    pub terms: Vec<String>,
}

impl RetrievalOutcome {
    /// Confidence is judged on distinct *files*, so several matches
    /// inside one document do not look like corroboration.
    pub fn confidence(&self) -> RetrievalConfidence {
        let files: HashSet<_> = self
            .sources
            .iter()
            .map(|s| s.split_project_prefix().1)
            .collect();
        RetrievalConfidence::from_distinct_files(files.len())
    }
}

/// Matches "What does this repository do?", "Explain this project.",
/// "What is this codebase for?" and similar broad questions that have no
/// specific keyword a search could key off of.
pub fn is_broad_overview_question(question: &str) -> bool {
    let q = question.to_lowercase();
    OVERVIEW_SUBJECTS.iter().any(|s| q.contains(s)) && OVERVIEW_VERBS.iter().any(|v| q.contains(v))
}

/// Deterministically call `project.information`, then `project.read_file`
/// on whichever overview-shaped docs actually exist -- all *before* the
/// model ever sees the question. This is the fix for a small local model
/// unreliably deciding, on its own, to call the right tools in the right
/// order for a vague question: grounding no longer depends on that
/// decision.
///
/// An earlier version of this also ran `project.search` for the project's
/// own name as a broad third step. It was dropped: a bare project name is
/// often a generic word (a real project here is literally named
/// `assistant`), so that search matched incidental substrings in unrelated
/// files -- `Cargo.toml`'s `repository = ".../assistant"` line, for
/// example -- and those matches became noisy, irrelevant entries in the
/// final answer's sources. `project.read_file` on ranked overview docs is
/// precise by construction (it only reads files that look like READMEs,
/// instructions, or specs); a bare keyword search on the project's own
/// name is not, so keeping only the precise step is a deliberate
/// application-code decision, not something left to the model to avoid.
///
/// Every call goes through the exact same `ActionRegistry::execute` a
/// model-initiated call would: identical permission enforcement, identical
/// execution records. A step that fails (permission denied, nothing
/// configured, file not found) is skipped silently -- this is a best-effort
/// head start, not a requirement, and it never bypasses a project's
/// permission configuration.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    registry: &ActionRegistry,
    context: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
    history: &mut Vec<Message>,
    events: &EventEmitter,
    question: &str,
    context_terms: &[String],
) -> RetrievalOutcome {
    let mut sources = Vec::new();
    let mut records = Vec::new();
    let mut next_call_id = 0u32;

    let project = context.config.project.name.clone();
    events.emit(EventPayload::RetrievalStarted {
        scope: vec![project.clone()],
    });

    execute_synthetic(
        registry,
        context,
        confirmation,
        history,
        "project.information",
        json!({}),
        &mut sources,
        &mut records,
        &mut next_call_id,
        events,
    )
    .await;

    let analysis = orbit_project::analyze_with_context(question, context_terms);
    let terms = analysis.all_terms();
    let mut read_paths: Vec<String> = Vec::new();

    // Step 1-2: lexical search over the project. Ranking already favors
    // filenames, path components, and headings, so a question about
    // architecture reaches `docs/ARCHITECTURE.md` without a special case
    // for documentation.
    if !terms.is_empty() {
        let output = execute_synthetic(
            registry,
            context,
            confirmation,
            history,
            "project.search",
            json!({ "query": analysis.to_search_string(), "limit": SEARCH_LIMIT }),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
        read_paths = strongest_paths(output.as_ref(), MAX_FOLLOW_UP_READS);
    }

    // Step 3-4: read the strongest matches in full. A one-line excerpt
    // rarely answers "explain X"; the surrounding prose does.
    for path in &read_paths {
        execute_synthetic(
            registry,
            context,
            confirmation,
            history,
            "project.read_file",
            json!({ "path": path, "max_bytes": FOLLOW_UP_READ_BYTES, "truncate": true }),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
    }

    // Fallback: nothing matched (or there was nothing to search for), so
    // fall back to whatever reads like an overview. This is what answers
    // "what does this project do", which has no keyword of its own.
    if read_paths.is_empty() {
        for path in overview_candidates(registry, context, confirmation, events).await {
            execute_synthetic(
                registry,
                context,
                confirmation,
                history,
                "project.read_file",
                json!({ "path": path, "max_bytes": OVERVIEW_READ_BYTES }),
                &mut sources,
                &mut records,
                &mut next_call_id,
                events,
            )
            .await;
        }
    }

    events.emit(EventPayload::RetrievalCompleted {
        scope: vec![project],
        action_count: records.len(),
        source_count: sources.len(),
    });

    RetrievalOutcome {
        sources,
        records,
        terms,
    }
}

/// Distinct file paths from a `project.search` result, strongest first.
fn strongest_paths(output: Option<&Value>, limit: usize) -> Vec<String> {
    let Some(results) = output.and_then(|o| o["results"].as_array()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for result in results {
        let Some(path) = result["path"].as_str() else {
            continue;
        };
        if seen.insert(path.to_string()) {
            paths.push(path.to_string());
        }
        if paths.len() >= limit {
            break;
        }
    }
    paths
}

/// Rank the project's own allowed files by how likely they are to be a
/// human-written overview doc, using only the file list `project.list_files`
/// already exposes -- adapts to whatever a project actually has (README,
/// CLAUDE.md, a spec under docs/, ...) instead of assuming one fixed path.
async fn overview_candidates(
    registry: &ActionRegistry,
    context: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
    events: &EventEmitter,
) -> Vec<String> {
    let (_, result) = registry
        .execute_observed(
            context,
            "project.list_files",
            ActionInput::empty(),
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
        .take(MAX_OVERVIEW_READS)
        .map(|(_, path)| path)
        .collect()
}

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

#[allow(clippy::too_many_arguments)]
async fn execute_synthetic(
    registry: &ActionRegistry,
    context: &ActionContext,
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
            context,
            name,
            ActionInput(arguments.clone()),
            confirmation,
            events,
            &events.next_execution_id(),
        )
        .await;
    records.push(record);

    match result {
        Ok(output) => {
            sources.extend(output.sources.iter().cloned());
            let id = format!("orbit_auto_{next_call_id}");
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
                "deterministic retrieval step executed"
            );
            Some(output.data)
        }
        Err(err) => {
            tracing::debug!(
                action = name,
                error = %err,
                "deterministic retrieval step skipped"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_three_reported_phrasings() {
        assert!(is_broad_overview_question("What does this repository do?"));
        assert!(is_broad_overview_question("Explain this project."));
        assert!(is_broad_overview_question("What is this codebase for?"));
    }

    #[test]
    fn does_not_match_specific_questions() {
        assert!(!is_broad_overview_question("What does the watchdog do?"));
        assert!(!is_broad_overview_question(
            "Why was the ESP32-C3 selected?"
        ));
        assert!(!is_broad_overview_question("Run the test command"));
    }

    #[test]
    fn scores_readme_above_claude_md_above_unrelated_files() {
        assert!(score_overview_candidate("README.md") > score_overview_candidate("CLAUDE.md"));
        assert!(
            score_overview_candidate("CLAUDE.md")
                > score_overview_candidate("docs/PROJECT_SPEC.md")
        );
        assert_eq!(score_overview_candidate("src/main.rs"), None);
    }

    #[test]
    fn ignores_files_nested_more_than_one_level_deep() {
        assert_eq!(score_overview_candidate("a/b/README.md"), None);
        assert!(score_overview_candidate("docs/PROJECT_SPEC.md").is_some());
    }
}
