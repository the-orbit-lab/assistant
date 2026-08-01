use orbit_actions::{ActionContext, ActionRegistry};
use std::collections::HashSet;

use orbit_core::{
    ActionInput, ConfirmationProvider, EventEmitter, EventPayload, ExecutionRecord, Message,
    RetrievalConfidence, SourceReference, ToolCall,
};
use orbit_retrieval::evidence::{SourceSpan, SpanBudget};
use orbit_retrieval::pipeline::{Pipeline, PipelineInput};
use orbit_retrieval::select::SelectionLimits;
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
/// Whole-file reads allowed *after* a symbol bundle has been quoted.
///
/// The bundle already carries the direct answer in a few dozen lines. A
/// second and third whole document after it does not add evidence, it
/// adds twelve kilobytes each — and because they arrive last, they are
/// the freshest thing in the model's context when it starts to answer.
/// That is how a question about a type gets answered from an
/// architecture overview.
const MAX_READS_WITH_SYMBOL_EVIDENCE: usize = 1;
const FOLLOW_UP_READ_BYTES: u64 = 12_000;
/// Ranked and rejected rows included in the `--verbose` retrieval report.
const DIAGNOSTIC_ROWS: usize = 8;

/// What one deterministic retrieval step produced.
#[derive(Debug, Clone, Default)]
pub struct RetrievalOutcome {
    pub sources: Vec<SourceReference>,
    pub records: Vec<ExecutionRecord>,
    /// Terms actually searched for, for `--verbose` diagnostics.
    pub terms: Vec<String>,
    /// How the evidence pipeline reached its selection, for `--verbose`.
    ///
    /// Structural only -- paths, evidence types, and scores. No file is
    /// opened to produce it and it can never expose an excluded file.
    pub diagnostics: Option<String>,
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
    let mut diagnostics: Option<String> = None;

    // Step 1-2: decide what evidence this question needs.
    //
    // The two-stage pipeline (plan → generate → fuse → rerank → select)
    // decides *which files* are worth reading; a plain lexical ranking
    // cannot, because it measures how often a file repeats the question's
    // words rather than whether the file is about the subject. See
    // `orbit_retrieval`.
    //
    // The pipeline only ever ranks files that project discovery already
    // produced, and it reads nothing itself: every file that reaches the
    // model below goes through `project.read_file` and the same
    // permission enforcement a model-initiated call would face.
    // This replaces an earlier synthetic `project.search` step, which
    // cited every line the ranking matched. Those excerpt citations were
    // the visible half of the reported failure: an answer about
    // `SessionRuntime` listed five test files and an unrelated module,
    // because a lexical hit is not evidence — it is a place a word
    // occurs. Citing only what was actually selected and read makes the
    // source list mean "this is what the answer rests on". The model
    // still has `project.search` as a tool when it wants breadth.
    let mut symbol_spans: Vec<SourceSpan> = Vec::new();
    let mut symbol_anchor: Option<String> = None;
    if !terms.is_empty() {
        let planned = plan_evidence(context, question, context_terms);
        read_paths = planned.read_paths;
        diagnostics = planned.diagnostics;
        symbol_spans = planned.symbol_spans;
        symbol_anchor = planned.symbol_anchor;
    }

    // Step 4: read the strongest matches in full. A one-line excerpt
    // rarely answers "explain X"; the surrounding prose does. The file
    // whose declaration was already quoted is skipped: its precise spans
    // are better evidence than its first twelve kilobytes, and reading
    // both would spend the budget twice on one file.
    let quoted: Vec<String> = symbol_spans
        .iter()
        .map(|s| s.path.to_string_lossy().to_string())
        .collect();
    let whole_file_budget = if symbol_spans.is_empty() {
        MAX_FOLLOW_UP_READS
    } else {
        MAX_READS_WITH_SYMBOL_EVIDENCE
    };
    for path in read_paths
        .iter()
        .filter(|p| !quoted.contains(p))
        .take(whole_file_budget)
    {
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

    // Step 5: quote the declaration and its most relevant methods, each
    // as a ranged read through the same permission-checked action a
    // whole-file read uses. Reading only these lines is what keeps a
    // 570-line `impl` block from costing the rest of the evidence its
    // place in the model's context.
    //
    // These come *last*, after the supporting documents, and the
    // ordering is load-bearing. A model answers from what is nearest
    // its question: with an 8 KB architecture document read after the
    // declaration, an answer about `SessionRuntime` came back
    // describing session lifecycle in general. The direct evidence has
    // to be the last thing it reads, and it is also the only evidence
    // that is definitionally about the subject rather than merely
    // ranked as relevant to it.
    for span in &symbol_spans {
        execute_synthetic(
            registry,
            context,
            confirmation,
            history,
            "project.read_file",
            json!({
                "path": span.path.to_string_lossy(),
                "line_start": span.line_start,
                "line_end": span.line_end,
                "max_bytes": FOLLOW_UP_READ_BYTES,
                "truncate": true,
            }),
            &mut sources,
            &mut records,
            &mut next_call_id,
            events,
        )
        .await;
    }

    // A trusted instruction naming the declaration, so the model treats
    // it as the definition rather than as one more retrieved document.
    // This states a fact retrieval established -- the AST says this file
    // and these lines declare the entity -- and never asks the model to
    // find or verify anything itself.
    if let Some(anchor) = &symbol_anchor {
        history.push(Message::system(anchor.clone()));
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
        diagnostics,
    }
}

/// Choose which files to read in full, and explain the choice.
///
/// Runs the two-stage retrieval pipeline over the project's discovered
/// files. Returns project-relative paths in selection order, plus a
/// diagnostics report for `--verbose`.
///
/// Discovery failure is not an error here: retrieval is a best-effort
/// head start, and the overview fallback still applies. Note also that
/// the pipeline is built per turn — the index costs roughly 200 ms on a
/// repository of this size — because `ActionContext` is rebuilt per turn
/// and a project's files can change between questions. A session-lifetime
/// cache belongs with the session, not here.
fn plan_evidence(
    context: &ActionContext,
    question: &str,
    context_terms: &[String],
) -> EvidencePlan {
    let Ok(files) = orbit_project::discover_files(&context.root, &context.config) else {
        return EvidencePlan::default();
    };

    let pipeline = Pipeline::new(files);
    let output = pipeline.run(
        &PipelineInput {
            question: question.to_string(),
            context_terms: context_terms.to_vec(),
            // The session's topic terms already carry the conversation
            // forward; only paths produced by real retrieval would be
            // eligible here, never a path the model mentioned in prose.
            recent_paths: Vec::new(),
            target_projects: vec![context.config.project.name.clone()],
        },
        &SelectionLimits::default(),
    );

    let paths = read_order(
        &output.selection.items,
        &output.plan.preferred_evidence_types,
        MAX_FOLLOW_UP_READS,
    );

    // The exact regions of the declaration and its most relevant
    // methods. These are quoted *before* anything the ranking produced:
    // a declaration is not a guess about relevance, it is the thing the
    // question named, and a model that sees it first has the fields in
    // front of it when it starts to answer.
    let symbol_spans = output
        .symbol_evidence
        .as_ref()
        .map(|evidence| {
            evidence.budgeted_spans(
                &output.plan.terms_about(&evidence.name),
                &SpanBudget::default(),
            )
        })
        .unwrap_or_default();

    let source = output
        .symbol_evidence
        .as_ref()
        .and_then(|evidence| pipeline.corpus().get(&evidence.definition.path))
        .map(|file| file.content.clone());

    let report = output.diagnostics.report_with_evidence(
        &output.plan,
        &output.selection,
        output.symbol_evidence.as_ref(),
        source.as_deref(),
        DIAGNOSTIC_ROWS,
    );

    let symbol_anchor = output.symbol_evidence.as_ref().map(|evidence| {
        format!(
            "The repository declares `{}` as a {} at {}:{}-{}, with {} field(s) \
             and {} method(s). That declaration is the definition of `{}`; \
             explain it and its fields directly. Do not describe the tests \
             that exercise it, and do not substitute a general overview of \
             the surrounding subsystem for the type itself.",
            evidence.name,
            evidence.kind.as_str(),
            evidence.definition.path.display(),
            evidence.definition.line_start,
            evidence.definition.line_end,
            evidence.fields.len(),
            evidence.method_count(),
            evidence.name,
        )
    });

    EvidencePlan {
        symbol_spans,
        symbol_anchor,
        read_paths: paths,
        diagnostics: Some(report),
    }
}

/// What the pipeline decided this turn should put in front of the model.
#[derive(Debug, Default)]
struct EvidencePlan {
    /// Exact regions of the named symbol's declaration and methods.
    symbol_spans: Vec<SourceSpan>,
    /// A trusted instruction naming where the entity is declared.
    symbol_anchor: Option<String>,
    /// Whole files to read after them.
    read_paths: Vec<String>,
    diagnostics: Option<String>,
}

/// Order the selected evidence for *reading*, as opposed to for coverage.
///
/// The selection is deliberately diversified: it alternates between kinds
/// of evidence so the set as a whole can support an explanation. Reading
/// is a different decision. Only a handful of files fit in a local
/// model's context, and a long one crowds out everything read after it,
/// so the read list leads with the evidence types this intent actually
/// asked for, ordered by score, and falls back to the rest only to fill
/// the quota.
///
/// Without this, "Explain SessionRuntime" reads a 536-line test file
/// alongside the declaration, and the answer describes the tests.
fn read_order(
    items: &[orbit_retrieval::rerank::RankedEvidence],
    preferred: &[orbit_retrieval::plan::EvidenceType],
    limit: usize,
) -> Vec<String> {
    let mut wanted: Vec<&orbit_retrieval::rerank::RankedEvidence> = items
        .iter()
        .filter(|i| preferred.contains(&i.evidence_type))
        .collect();
    wanted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
    });

    let rest = items
        .iter()
        .filter(|i| !preferred.contains(&i.evidence_type));

    let mut paths: Vec<String> = Vec::new();
    for item in wanted.into_iter().chain(rest) {
        let path = item.candidate.path.to_string_lossy().to_string();
        if !paths.contains(&path) {
            paths.push(path);
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

    /// The reported failure, at the read-selection layer.
    ///
    /// A long test file that legitimately made the evidence set must not
    /// consume one of the three read slots ahead of the declaration and
    /// the subject's own documentation — reading it is what made the
    /// answer describe the tests instead of the type.
    #[test]
    fn reads_lead_with_the_evidence_types_the_intent_asked_for() {
        use orbit_retrieval::candidate::{Candidate, CandidateOrigin};
        use orbit_retrieval::plan::EvidenceType;
        use orbit_retrieval::rerank::{EvidenceFeatures, RankedEvidence};

        let item = |path: &str, kind: EvidenceType, score: f64| RankedEvidence {
            candidate: Candidate {
                path: std::path::PathBuf::from(path),
                line_start: 1,
                line_end: 1,
                excerpt: String::new(),
                section: None,
                origin: CandidateOrigin::Lexical,
                generator_score: score,
                symbol_kind: None,
                defines_entity: None,
            },
            evidence_type: kind,
            features: EvidenceFeatures::default(),
            fused_score: score,
            score,
            origin_summary: String::new(),
        };

        // Selection order deliberately interleaves kinds; the test file
        // sits above one of the documentation entries.
        let items = vec![
            item(
                "crates/session/src/session.rs",
                EvidenceType::Definition,
                1.48,
            ),
            item("docs/SESSIONS.md", EvidenceType::DomainDocumentation, 1.30),
            item(
                "crates/session/tests/session_runtime.rs",
                EvidenceType::Test,
                0.56,
            ),
            item("docs/ARCHITECTURE.md", EvidenceType::Architecture, 0.44),
        ];
        let preferred = vec![
            EvidenceType::Definition,
            EvidenceType::Implementation,
            EvidenceType::DomainDocumentation,
            EvidenceType::Architecture,
        ];

        assert_eq!(
            read_order(&items, &preferred, 3),
            vec![
                "crates/session/src/session.rs".to_string(),
                "docs/SESSIONS.md".to_string(),
                "docs/ARCHITECTURE.md".to_string(),
            ]
        );
    }

    /// A type the intent did not ask for is still reachable — it fills
    /// the quota rather than being excluded, because "how is this
    /// tested?" is a real question and the caps are about ordering.
    #[test]
    fn unpreferred_evidence_still_fills_the_read_quota() {
        use orbit_retrieval::candidate::{Candidate, CandidateOrigin};
        use orbit_retrieval::plan::EvidenceType;
        use orbit_retrieval::rerank::{EvidenceFeatures, RankedEvidence};

        let only_a_test = vec![RankedEvidence {
            candidate: Candidate {
                path: std::path::PathBuf::from("crates/a/tests/t.rs"),
                line_start: 1,
                line_end: 1,
                excerpt: String::new(),
                section: None,
                origin: CandidateOrigin::Lexical,
                generator_score: 1.0,
                symbol_kind: None,
                defines_entity: None,
            },
            evidence_type: EvidenceType::Test,
            features: EvidenceFeatures::default(),
            fused_score: 1.0,
            score: 1.0,
            origin_summary: String::new(),
        }];

        assert_eq!(
            read_order(&only_a_test, &[EvidenceType::Definition], 3),
            vec!["crates/a/tests/t.rs".to_string()]
        );
    }
}
