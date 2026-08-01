//! The pipeline: plan → generate → fuse → rerank → select.
//!
//! This module wires the layers together and records what each one did.
//! It contains no ranking logic of its own — deliberately, so that a
//! change to how evidence is scored has exactly one home ([`crate::rerank`])
//! and a change to how a set is composed has exactly one other
//! ([`crate::select`]).
//!
//! The corpus is read and parsed once, in [`Pipeline::new`], and reused
//! for every question in a session. Re-reading and re-parsing a whole
//! repository per turn is the difference between a pipeline that can run
//! on every question and one that cannot.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use orbit_project::DiscoveredFile;

use crate::candidate::{
    Candidate, Corpus, context_candidates, heading_candidates, lexical_candidates, path_candidates,
    symbol_candidates,
};
use crate::evidence::{SpanBudget, SymbolEvidence, extract};
use crate::fusion::{FusedCandidate, fuse};
use crate::plan::{RetrievalIntent, RetrievalPlan, plan};
use crate::rerank::{RankedEvidence, rerank};
use crate::select::{Selection, SelectionLimits, select};

/// What the caller knows about this turn.
#[derive(Debug, Default, Clone)]
pub struct PipelineInput {
    pub question: String,
    /// Subject terms carried over from the conversation.
    pub context_terms: Vec<String>,
    /// Paths cited by *real retrieval* in previous turns. Paths the model
    /// merely mentioned must never be passed here.
    pub recent_paths: Vec<PathBuf>,
    pub target_projects: Vec<String>,
}

/// Per-stage timings, for the performance budget.
#[derive(Debug, Default, Clone)]
pub struct StageTimings {
    pub plan: Duration,
    pub generate: Duration,
    pub fuse: Duration,
    pub rerank: Duration,
    pub select: Duration,
}

impl StageTimings {
    pub fn total(&self) -> Duration {
        self.plan + self.generate + self.fuse + self.rerank + self.select
    }
}

/// Everything needed to explain a retrieval, without re-running it.
#[derive(Debug, Clone)]
pub struct Diagnostics {
    /// Candidate counts per generator, in pipeline order.
    pub generator_counts: Vec<(&'static str, usize)>,
    pub fused_count: usize,
    /// The reranked shortlist, best first.
    pub ranked: Vec<RankedEvidence>,
    pub timings: StageTimings,
    /// Whether a model reranking pass was applied.
    pub model_reranked: bool,
}

impl Diagnostics {
    /// Multi-line explanation for `--verbose`.
    ///
    /// Structural only: paths, evidence types, scores, and the excerpts
    /// search already returns. No file is opened to produce it, so it can
    /// never reveal content the search itself would not have.
    pub fn report(&self, plan: &RetrievalPlan, selection: &Selection, top: usize) -> String {
        self.report_with_evidence(plan, selection, None, None, top)
    }

    /// The full report, including the symbol bundle and the exact text
    /// that will be quoted to the model.
    ///
    /// `source` is the declaring file's content, already read by the
    /// corpus; nothing here opens a file, and every span shown is one the
    /// agent will fetch through `project.read_file` under the same
    /// permission checks.
    pub fn report_with_evidence(
        &self,
        plan: &RetrievalPlan,
        selection: &Selection,
        symbol_evidence: Option<&SymbolEvidence>,
        source: Option<&str>,
        top: usize,
    ) -> String {
        let mut out = String::new();
        out.push_str(&format!("plan:             {}\n", plan.summary()));
        out.push_str(&format!(
            "preferred types:  {:?}\n",
            plan.preferred_evidence_types
        ));
        out.push_str("generators:       ");
        out.push_str(
            &self
                .generator_counts
                .iter()
                .map(|(name, count)| format!("{name}={count}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        out.push('\n');
        out.push_str(&format!("fused candidates: {}\n", self.fused_count));
        out.push_str(&format!(
            "model rerank:     {}\n",
            if self.model_reranked { "applied" } else { "no" }
        ));
        out.push_str(&format!(
            "timings(ms):      plan={:.1} generate={:.1} fuse={:.1} rerank={:.1} select={:.1} total={:.1}\n",
            self.timings.plan.as_secs_f64() * 1000.0,
            self.timings.generate.as_secs_f64() * 1000.0,
            self.timings.fuse.as_secs_f64() * 1000.0,
            self.timings.rerank.as_secs_f64() * 1000.0,
            self.timings.select.as_secs_f64() * 1000.0,
            self.timings.total().as_secs_f64() * 1000.0,
        ));

        out.push_str("ranked:\n");
        for item in self.ranked.iter().take(top) {
            out.push_str(&format!("  {}\n", item.explain()));
        }

        if let Some(evidence) = symbol_evidence {
            out.push_str(&format!(
                "symbol evidence:  {} {} at {} ({} fields, {} impl blocks, {} methods)\n",
                evidence.kind.as_str(),
                evidence.name,
                evidence.definition.locator(),
                evidence.fields.len(),
                evidence.impl_blocks.len(),
                evidence.method_count(),
            ));
            for field in &evidence.fields {
                out.push_str(&format!(
                    "  field  {}: {}  [{}]\n",
                    field.name,
                    field.type_text,
                    field.span.locator()
                ));
            }
            for block in &evidence.impl_blocks {
                out.push_str(&format!(
                    "  {}  [{}] {} methods\n",
                    block.header(&evidence.name),
                    block.span.locator(),
                    block.methods.len()
                ));
            }

            let spans =
                evidence.budgeted_spans(&plan.terms_about(&evidence.name), &SpanBudget::default());
            out.push_str("symbol spans sent to the model:\n");
            for span in &spans {
                out.push_str(&format!(
                    "  {} ({} lines)\n",
                    span.locator(),
                    span.line_count()
                ));
            }

            // The exact excerpt, so a failure can be diagnosed from the
            // log without guessing what the model actually saw.
            if let Some(source) = source {
                out.push_str("excerpt sent to the model:\n");
                for span in &spans {
                    out.push_str(&format!("  --- {} ---\n", span.locator()));
                    for line in crate::evidence::slice_span(source, span).lines() {
                        out.push_str(&format!("  | {line}\n"));
                    }
                }
            }
        }

        out.push_str("selected:\n");
        for item in &selection.items {
            out.push_str(&format!(
                "  {}:{}-{} [{}] score={:.4}\n",
                item.candidate.path.display(),
                item.candidate.line_start,
                item.candidate.line_end,
                item.evidence_type.as_str(),
                item.score,
            ));
        }
        if !selection.rejected.is_empty() {
            out.push_str("rejected:\n");
            for (item, reason) in selection.rejected.iter().take(top) {
                out.push_str(&format!(
                    "  {}:{} [{}] — {}\n",
                    item.candidate.path.display(),
                    item.candidate.line_start,
                    item.evidence_type.as_str(),
                    reason.as_str(),
                ));
            }
        }
        out
    }
}

/// The result of one retrieval.
#[derive(Debug, Clone)]
pub struct RetrievalOutput {
    pub plan: RetrievalPlan,
    pub selection: Selection,
    pub diagnostics: Diagnostics,
    /// AST-backed evidence for the entity this question named, when the
    /// question names one and the repository declares it.
    ///
    /// This is the direct answer to a symbol question and it does not go
    /// through ranking at all — a declaration is not a guess about
    /// relevance, it is the thing that was asked about. The agent quotes
    /// its spans before anything the ranking produced.
    pub symbol_evidence: Option<SymbolEvidence>,
}

/// A prepared corpus that can answer many questions.
pub struct Pipeline {
    files: Vec<DiscoveredFile>,
    corpus: Corpus,
}

impl Pipeline {
    /// Read and index the corpus. This is the expensive step; do it once.
    pub fn new(files: Vec<DiscoveredFile>) -> Self {
        let corpus = Corpus::build(&files);
        Self { files, corpus }
    }

    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.corpus.symbols().len()
    }

    /// Run the full pipeline for one question.
    pub fn run(&self, input: &PipelineInput, limits: &SelectionLimits) -> RetrievalOutput {
        let mut timings = StageTimings::default();

        let started = Instant::now();
        let plan = plan(
            &input.question,
            &input.context_terms,
            &input.target_projects,
            Some(self.corpus.symbols()),
        );
        timings.plan = started.elapsed();

        let started = Instant::now();
        let lists: Vec<(&'static str, Vec<Candidate>)> = vec![
            ("lexical", lexical_candidates(&self.corpus, &plan)),
            ("symbol", symbol_candidates(&self.corpus, &plan)),
            ("path", path_candidates(&self.corpus, &plan)),
            ("heading", heading_candidates(&self.corpus, &plan)),
            (
                "context",
                context_candidates(&self.corpus, &input.recent_paths),
            ),
        ];
        timings.generate = started.elapsed();

        let generator_counts: Vec<(&'static str, usize)> = lists
            .iter()
            .map(|(name, list)| (*name, list.len()))
            .collect();

        let started = Instant::now();
        let candidate_lists: Vec<Vec<Candidate>> =
            lists.into_iter().map(|(_, list)| list).collect();
        let fused: Vec<FusedCandidate> = fuse(&candidate_lists);
        timings.fuse = started.elapsed();

        let started = Instant::now();
        let ranked = rerank(&fused, &self.corpus, &plan);
        timings.rerank = started.elapsed();

        let symbol_evidence = self.symbol_evidence(&plan);

        let started = Instant::now();
        let selection = select(&ranked, &plan, limits);
        timings.select = started.elapsed();

        RetrievalOutput {
            diagnostics: Diagnostics {
                generator_counts,
                fused_count: fused.len(),
                ranked,
                timings,
                model_reranked: false,
            },
            plan,
            selection,
            symbol_evidence,
        }
    }

    /// Build the AST evidence bundle for the question's entity.
    ///
    /// Only for a question that actually names one: "Explain the session
    /// architecture" has no entity and must not be handed an arbitrary
    /// type's declaration. The symbol index supplies the file, so the
    /// path is always one discovery already produced.
    fn symbol_evidence(&self, plan: &RetrievalPlan) -> Option<SymbolEvidence> {
        if !matches!(
            plan.intent,
            RetrievalIntent::SymbolExplanation
                | RetrievalIntent::ArchitectureExplanation
                | RetrievalIntent::ImplementationLocation
        ) {
            return None;
        }

        for entity in &plan.entities {
            for symbol in self.corpus.symbols().lookup(entity) {
                if !symbol.kind.is_definition() {
                    continue;
                }
                let Some(file) = self.corpus.get(&symbol.path) else {
                    continue;
                };
                if let Some(bundle) = extract(&symbol.path, &file.content, &symbol.name) {
                    return Some(bundle);
                }
            }
        }
        None
    }

    /// The spans of the symbol bundle, bounded, in the order they should
    /// be put in front of a model.
    pub fn symbol_spans(
        output: &RetrievalOutput,
        budget: &SpanBudget,
    ) -> Vec<crate::evidence::SourceSpan> {
        output
            .symbol_evidence
            .as_ref()
            .map(|evidence| {
                evidence.budgeted_spans(&output.plan.terms_about(&evidence.name), budget)
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::SpanBudget;
    use crate::plan::EvidenceType;
    use std::fs;
    use tempfile::TempDir;

    /// A repository shaped like the one that produced the reported
    /// failure: the subject is defined in code, described in its own
    /// document, and mentioned repeatedly by a longer document about
    /// something else.
    fn repository() -> (TempDir, Vec<DiscoveredFile>) {
        let dir = TempDir::new().unwrap();
        let mut written: Vec<String> = Vec::new();
        let mut write = |relative: &str, body: &str| {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            written.push(relative.to_string());
        };

        write(
            "crates/session/src/session.rs",
            "//! Stateful multi-turn sessions.\n\
             use std::collections::HashMap;\n\
             \n\
             /// Owns the conversation state for one session.\n\
             pub struct SessionRuntime {\n\
             \x20   history: Vec<String>,\n\
             \x20   sources: HashMap<String, usize>,\n\
             }\n\
             \n\
             impl SessionRuntime {\n\
             \x20   pub fn new() -> Self {\n\
             \x20       Self { history: Vec::new(), sources: HashMap::new() }\n\
             \x20   }\n\
             \n\
             \x20   pub fn record(&mut self, message: String) {\n\
             \x20       self.history.push(message);\n\
             \x20   }\n\
             }\n",
        );

        let mut sessions_doc = String::from("# Sessions\n\n");
        sessions_doc.push_str(
            "A session keeps conversation state in process memory.\n\
             The session runtime owns the history and the collected sources.\n\n\
             ## State\n\n\
             Session state is discarded when the process ends.\n\
             Nothing about a session is written to disk.\n\n\
             ## Cancellation\n\n\
             A running session turn can be cancelled at any point.\n",
        );
        write("docs/SESSIONS.md", &sessions_doc);

        let mut search_doc = String::from("# Search and retrieval\n\n");
        for section in 0..8 {
            search_doc.push_str(&format!("## Ranking stage {section}\n\n"));
            search_doc.push_str(
                "Search ranks lines by BM25 over normalized tokens.\n\
                 Ranking is deterministic and reproducible across runs.\n\
                 The session runtime calls search when it builds state.\n\
                 Scores are scaled to integers so ordering never drifts.\n\
                 Excerpts are truncated per file before global ranking.\n\
                 Term coverage rewards lines matching more query terms.\n\n",
            );
        }
        write("docs/SEARCH.md", &search_doc);

        write(
            "crates/session/tests/session_runtime.rs",
            "use orbit_session::SessionRuntime;\n\
             \n\
             #[test]\n\
             fn a_session_runtime_records_history() {\n\
             \x20   let mut runtime = SessionRuntime::new();\n\
             \x20   runtime.record(\"hello\".to_string());\n\
             }\n",
        );

        let files = written
            .iter()
            .map(|relative| {
                let absolute = dir.path().join(relative);
                DiscoveredFile {
                    relative_path: PathBuf::from(relative),
                    size: fs::metadata(&absolute).unwrap().len(),
                    absolute_path: absolute,
                    is_text: true,
                }
            })
            .collect();
        (dir, files)
    }

    fn ask(pipeline: &Pipeline, question: &str) -> RetrievalOutput {
        pipeline.run(
            &PipelineInput {
                question: question.to_string(),
                ..Default::default()
            },
            &SelectionLimits::default(),
        )
    }

    /// The reported failure, end to end.
    #[test]
    fn the_reported_question_selects_the_definition_not_the_verbose_document() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );

        let first = &output.selection.items[0];
        assert_eq!(
            first.candidate.path,
            PathBuf::from("crates/session/src/session.rs"),
            "{}",
            output
                .diagnostics
                .report(&output.plan, &output.selection, 10)
        );
        assert_eq!(first.evidence_type, EvidenceType::Definition);

        // The verbose off-topic document must not dominate the set.
        let from_search = output
            .selection
            .items
            .iter()
            .filter(|i| i.candidate.path.ends_with("SEARCH.md"))
            .count();
        assert!(
            from_search <= 1,
            "{}",
            output
                .diagnostics
                .report(&output.plan, &output.selection, 10)
        );
    }

    #[test]
    fn the_subjects_own_document_is_selected_too() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );

        assert!(
            output
                .selection
                .items
                .iter()
                .any(|i| i.candidate.path.ends_with("SESSIONS.md")),
            "{}",
            output
                .diagnostics
                .report(&output.plan, &output.selection, 10)
        );
    }

    #[test]
    fn the_selection_is_diverse_across_files() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );
        assert!(
            output.selection.distinct_files() >= 3,
            "{:?}",
            output.selection.items
        );
    }

    #[test]
    fn every_stage_reports_candidates() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(&pipeline, "Explain SessionRuntime.");

        let counts = &output.diagnostics.generator_counts;
        assert_eq!(counts.len(), 5);
        for (name, count) in counts {
            if *name != "context" {
                assert!(*count > 0, "generator {name} produced nothing");
            }
        }
        assert!(output.diagnostics.fused_count > 0);
    }

    #[test]
    fn the_report_explains_without_reading_files() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(&pipeline, "Explain SessionRuntime.");
        let report = output
            .diagnostics
            .report(&output.plan, &output.selection, 5);

        assert!(report.contains("plan:"));
        assert!(report.contains("generators:"));
        assert!(report.contains("selected:"));
        assert!(report.contains("SessionRuntime"));
    }

    #[test]
    fn retrieval_is_deterministic() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let paths = |output: &RetrievalOutput| -> Vec<(PathBuf, usize)> {
            output
                .selection
                .items
                .iter()
                .map(|i| (i.candidate.path.clone(), i.candidate.line_start))
                .collect()
        };
        let first = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );
        let second = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );
        assert_eq!(paths(&first), paths(&second));
    }

    #[test]
    fn a_question_about_nothing_in_the_repository_selects_nothing() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = ask(&pipeline, "Explain the hydraulic landing gear actuator.");
        assert!(
            output.selection.is_empty(),
            "{}",
            output
                .diagnostics
                .report(&output.plan, &output.selection, 10)
        );
    }

    #[test]
    fn context_paths_anchor_a_follow_up() {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        let output = pipeline.run(
            &PipelineInput {
                question: "How does cancellation work?".to_string(),
                context_terms: vec!["session".to_string()],
                recent_paths: vec![PathBuf::from("docs/SESSIONS.md")],
                ..Default::default()
            },
            &SelectionLimits::default(),
        );
        assert!(
            output
                .selection
                .items
                .iter()
                .any(|i| i.candidate.path.ends_with("SESSIONS.md")),
            "{}",
            output
                .diagnostics
                .report(&output.plan, &output.selection, 10)
        );
    }

    /// The failure this whole design exists to prevent, stated as the
    /// user reported it: a test file that repeats the exact question and
    /// the symbol name many times must not outrank the struct that
    /// declares the state the question asks about.
    fn adversarial_repository() -> (TempDir, Vec<DiscoveredFile>) {
        let dir = TempDir::new().unwrap();
        let mut written: Vec<String> = Vec::new();
        let mut write = |relative: &str, body: &str| {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            written.push(relative.to_string());
        };

        write(
            "crates/session/src/session.rs",
            "//! Sessions.\n\
             use std::sync::Arc;\n\
             \n\
             /// A stateful conversation. Owns everything a turn needs.\n\
             pub struct SessionRuntime {\n\
             \x20   id: SessionId,\n\
             \x20   /// Held for the duration of a turn.\n\
             \x20   state: tokio::sync::Mutex<SessionState>,\n\
             \x20   current_cancel: std::sync::Mutex<Option<CancellationToken>>,\n\
             \x20   sources: Vec<SourceReference>,\n\
             \x20   streaming: bool,\n\
             }\n\
             \n\
             impl SessionRuntime {\n\
             \x20   /// Store a turn result in the session state.\n\
             \x20   pub async fn record_state(&self, outcome: TurnOutcome) {\n\
             \x20       self.state.lock().await.push(outcome);\n\
             \x20   }\n\
             \n\
             \x20   pub fn cancel_current_turn(&self) -> bool {\n\
             \x20       true\n\
             \x20   }\n\
             }\n",
        );

        // The adversary: a long test file repeating the exact question
        // and the symbol name on nearly every line.
        let mut test_file = String::from("use orbit_session::SessionRuntime;\n\n");
        for i in 0..40 {
            test_file.push_str(&format!(
                "/// Explain SessionRuntime and how it stores session state.\n\
                 #[test]\n\
                 fn session_runtime_stores_session_state_{i}() {{\n\
                 \x20   let runtime = SessionRuntime::new();\n\
                 \x20   // SessionRuntime stores session state here.\n\
                 \x20   assert!(runtime.stores_session_state());\n\
                 }}\n\n"
            ));
        }
        write("crates/session/tests/session_runtime.rs", &test_file);

        let mut grounding = String::from("//! Grounding tests.\n\n");
        for i in 0..30 {
            grounding.push_str(&format!(
                "#[test]\nfn grounding_session_runtime_state_{i}() {{\n\
                 \x20   // SessionRuntime session state grounding.\n}}\n\n"
            ));
        }
        write("crates/session/tests/grounding.rs", &grounding);

        let files = written
            .iter()
            .map(|relative| {
                let absolute = dir.path().join(relative);
                DiscoveredFile {
                    relative_path: PathBuf::from(relative),
                    size: fs::metadata(&absolute).unwrap().len(),
                    absolute_path: absolute,
                    is_text: true,
                }
            })
            .collect();
        (dir, files)
    }

    #[test]
    fn a_repetitive_test_file_never_outranks_the_declaration() {
        let (_dir, files) = adversarial_repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );
        let report = output
            .diagnostics
            .report(&output.plan, &output.selection, 10);

        let first = &output.selection.items[0];
        assert_eq!(
            first.candidate.path,
            PathBuf::from("crates/session/src/session.rs"),
            "{report}"
        );
        assert!(
            crate::select::is_direct_implementation(first.evidence_type),
            "{report}"
        );

        // At most one test, and never before the implementation.
        let first_test = output
            .selection
            .items
            .iter()
            .position(|i| i.evidence_type == EvidenceType::Test);
        let first_impl = output
            .selection
            .items
            .iter()
            .position(|i| crate::select::is_direct_implementation(i.evidence_type))
            .expect("implementation selected");
        if let Some(test_index) = first_test {
            assert!(test_index > first_impl, "{report}");
        }
    }

    /// The provider context must contain the complete struct — every
    /// field and its type — before any test evidence.
    #[test]
    fn the_symbol_bundle_carries_the_whole_struct_and_its_impl() {
        let (_dir, files) = adversarial_repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );

        let evidence = output
            .symbol_evidence
            .as_ref()
            .expect("SessionRuntime bundle");
        assert_eq!(evidence.name, "SessionRuntime");
        assert_eq!(
            evidence.definition.path,
            PathBuf::from("crates/session/src/session.rs")
        );

        let fields: Vec<&str> = evidence.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            fields,
            vec!["id", "state", "current_cancel", "sources", "streaming"]
        );
        let state = &evidence.fields[1];
        assert_eq!(state.type_text, "tokio::sync::Mutex<SessionState>");

        assert_eq!(evidence.impl_blocks.len(), 1);
        let methods: Vec<&str> = evidence.impl_blocks[0]
            .methods
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(methods, vec!["record_state", "cancel_current_turn"]);

        // And the spans the agent will quote lead with the declaration.
        let spans = evidence.budgeted_spans(&output.plan.concepts, &SpanBudget::default());
        assert_eq!(spans[0], evidence.definition);
        assert!(spans.len() > 1, "no method spans: {spans:?}");
    }

    /// The rendered excerpt — what the model actually sees — must show
    /// the fields, not just the name.
    #[test]
    fn the_excerpt_sent_to_the_model_contains_the_state_fields() {
        let (_dir, files) = adversarial_repository();
        let pipeline = Pipeline::new(files);
        let output = ask(
            &pipeline,
            "Explain SessionRuntime and how it stores session state.",
        );
        let evidence = output.symbol_evidence.as_ref().unwrap();
        let source = pipeline
            .corpus()
            .get(&evidence.definition.path)
            .map(|f| f.content.clone())
            .unwrap();

        let excerpt = crate::evidence::slice_span(&source, &evidence.definition);
        assert!(excerpt.contains("pub struct SessionRuntime {"), "{excerpt}");
        assert!(
            excerpt.contains("state: tokio::sync::Mutex<SessionState>"),
            "{excerpt}"
        );
        assert!(excerpt.contains("current_cancel"), "{excerpt}");

        // The diagnostics must be able to show it verbatim.
        let report = output.diagnostics.report_with_evidence(
            &output.plan,
            &output.selection,
            Some(evidence),
            Some(&source),
            10,
        );
        assert!(report.contains("excerpt sent to the model:"), "{report}");
        assert!(
            report.contains("state: tokio::sync::Mutex<SessionState>"),
            "{report}"
        );
    }

    /// A question naming no entity gets no bundle — the machinery must
    /// not hand an arbitrary declaration to an unrelated question.
    #[test]
    fn a_question_without_an_entity_gets_no_symbol_bundle() {
        let (_dir, files) = adversarial_repository();
        let pipeline = Pipeline::new(files);
        let output = ask(&pipeline, "Explain the session architecture.");
        assert!(output.symbol_evidence.is_none());
    }
}
