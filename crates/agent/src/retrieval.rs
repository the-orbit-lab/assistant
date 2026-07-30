use orbit_actions::{ActionContext, ActionRegistry};
use orbit_core::{
    ActionInput, ConfirmationProvider, ExecutionRecord, Message, SourceReference, ToolCall,
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
pub async fn run(
    registry: &ActionRegistry,
    context: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
    history: &mut Vec<Message>,
) -> (Vec<SourceReference>, Vec<ExecutionRecord>) {
    let mut sources = Vec::new();
    let mut records = Vec::new();
    let mut next_call_id = 0u32;

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
    )
    .await;

    for path in overview_candidates(registry, context, confirmation).await {
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
        )
        .await;
    }

    (sources, records)
}

/// Rank the project's own allowed files by how likely they are to be a
/// human-written overview doc, using only the file list `project.list_files`
/// already exposes -- adapts to whatever a project actually has (README,
/// CLAUDE.md, a spec under docs/, ...) instead of assuming one fixed path.
async fn overview_candidates(
    registry: &ActionRegistry,
    context: &ActionContext,
    confirmation: &dyn ConfirmationProvider,
) -> Vec<String> {
    let (_, result) = registry
        .execute(
            context,
            "project.list_files",
            ActionInput::empty(),
            confirmation,
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
) {
    let (record, result) = registry
        .execute(context, name, ActionInput(arguments.clone()), confirmation)
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
        }
        Err(err) => {
            tracing::debug!(
                action = name,
                error = %err,
                "deterministic retrieval step skipped"
            );
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
