//! What to put in front of the model, and in what order.
//!
//! This is the layer both retrieval front ends share. There are two of
//! them — `orbit-agent` for a single project, `orbit-workspace` for a
//! scope of several — and they differ only in which *actions* they call
//! (`project.read_file` versus `workspace.read_file`). Everything before
//! that, from planning the query to ordering the evidence, is the same
//! decision made about the same repository.
//!
//! Keeping it here is not tidiness. When the pipeline was wired into
//! `orbit-agent` alone, `orbit chat` in workspace mode kept running the
//! old lexical search and reading whatever it ranked highest — so a
//! question about `SessionRuntime` was answered out of
//! `tests/grounding.rs` while every unit test of the new pipeline
//! passed. One planner with two thin action adapters is what makes that
//! class of divergence impossible: a front end that does not call this
//! has no retrieval at all, rather than silently keeping an older one.

use std::path::PathBuf;

use orbit_project::DiscoveredFile;

use crate::evidence::{SourceSpan, SpanBudget};
use crate::pipeline::{Pipeline, PipelineInput, RetrievalOutput};
use crate::plan::EvidenceType;
use crate::select::SelectionLimits;

/// How many whole files to read when there is no symbol bundle.
pub const MAX_WHOLE_FILE_READS: usize = 3;
/// How many to read when there is one.
///
/// The bundle already carries the direct answer in a few dozen lines. A
/// second and third whole document after it does not add evidence, it
/// adds twelve kilobytes each.
pub const MAX_READS_WITH_SYMBOL_EVIDENCE: usize = 1;

/// One file to read whole, with the evidence type that earned it a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRead {
    pub path: PathBuf,
    pub evidence_type: EvidenceType,
}

/// The ordered plan for one turn's context.
#[derive(Debug, Clone, Default)]
pub struct EvidenceAgenda {
    /// Whole files to read, in order, *before* the symbol spans.
    pub reads: Vec<PlannedRead>,
    /// Exact regions of the named symbol's declaration and its most
    /// relevant methods. Read last — see [`EvidenceAgenda::reads`].
    pub symbol_spans: Vec<SourceSpan>,
    /// A trusted instruction naming where the entity is declared.
    pub anchor: Option<String>,
    /// Structural explanation for `--verbose`.
    pub diagnostics: Option<String>,
    /// Everything the selector chose, with its type, for diagnostics and
    /// for tests that assert on ordering.
    pub selected: Vec<PlannedRead>,
}

impl EvidenceAgenda {
    pub fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.symbol_spans.is_empty()
    }

    /// The paths in the order the model will encounter them.
    ///
    /// Whole-file reads first, then the symbol spans. The ordering is
    /// load-bearing: a model answers from what is nearest its question,
    /// and an 8 KB architecture document read *after* the declaration
    /// produced an answer about the surrounding subsystem instead of
    /// about the type.
    pub fn context_order(&self) -> Vec<PathBuf> {
        self.reads
            .iter()
            .map(|r| r.path.clone())
            .chain(self.symbol_spans.iter().map(|s| s.path.clone()))
            .collect()
    }
}

/// Build the agenda for one question over one project's files.
///
/// `pipeline` is borrowed so a caller holding a session-lifetime index
/// does not rebuild it per turn.
pub fn build(
    pipeline: &Pipeline,
    input: &PipelineInput,
    limits: &SelectionLimits,
) -> EvidenceAgenda {
    let output = pipeline.run(input, limits);
    from_output(pipeline, output)
}

/// Build the agenda from an already-computed pipeline run.
pub fn from_output(pipeline: &Pipeline, output: RetrievalOutput) -> EvidenceAgenda {
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

    let quoted: Vec<PathBuf> = symbol_spans.iter().map(|s| s.path.clone()).collect();
    let budget = if symbol_spans.is_empty() {
        MAX_WHOLE_FILE_READS
    } else {
        MAX_READS_WITH_SYMBOL_EVIDENCE
    };

    // Selection is ordered for coverage — it alternates between kinds of
    // evidence deliberately. Reading is a different decision: only a
    // handful of files fit in a local model's context, so the read list
    // leads with the types this intent asked for.
    let wanted = &output.plan.preferred_evidence_types;
    let mut preferred: Vec<&_> = output
        .selection
        .items
        .iter()
        .filter(|i| wanted.contains(&i.evidence_type))
        .collect();
    preferred.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
    });
    let rest = output
        .selection
        .items
        .iter()
        .filter(|i| !wanted.contains(&i.evidence_type));

    let mut reads: Vec<PlannedRead> = Vec::new();
    for item in preferred.into_iter().chain(rest) {
        // The declaring file's precise spans are better evidence than its
        // first twelve kilobytes, and reading both spends the budget
        // twice on one file.
        if quoted.contains(&item.candidate.path) {
            continue;
        }
        if reads.iter().any(|r| r.path == item.candidate.path) {
            continue;
        }
        reads.push(PlannedRead {
            path: item.candidate.path.clone(),
            evidence_type: item.evidence_type,
        });
        if reads.len() >= budget {
            break;
        }
    }

    let anchor = output.symbol_evidence.as_ref().map(|evidence| {
        format!(
            "The repository declares `{}` as a {} at {}:{}-{}, with {} field(s) and {} \
             method(s). That declaration is the definition of `{}`; explain it and its \
             fields directly. Do not describe the tests that exercise it, and do not \
             substitute a general overview of the surrounding subsystem for the type itself.",
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

    let source = output
        .symbol_evidence
        .as_ref()
        .and_then(|evidence| pipeline.corpus().get(&evidence.definition.path))
        .map(|file| file.content.clone());

    let selected: Vec<PlannedRead> = output
        .selection
        .items
        .iter()
        .map(|item| PlannedRead {
            path: item.candidate.path.clone(),
            evidence_type: item.evidence_type,
        })
        .collect();

    let mut diagnostics = output.diagnostics.report_with_evidence(
        &output.plan,
        &output.selection,
        output.symbol_evidence.as_ref(),
        source.as_deref(),
        8,
    );
    diagnostics.push_str("progressive reads (whole file, in order):\n");
    for read in &reads {
        diagnostics.push_str(&format!(
            "  {} [{}]\n",
            read.path.display(),
            read.evidence_type.as_str()
        ));
    }
    diagnostics.push_str("final provider-context order:\n");
    for (index, path) in reads
        .iter()
        .map(|r| r.path.clone())
        .chain(symbol_spans.iter().map(|s| s.path.clone()))
        .enumerate()
    {
        diagnostics.push_str(&format!("  {}. {}\n", index + 1, path.display()));
    }

    EvidenceAgenda {
        reads,
        symbol_spans,
        anchor,
        diagnostics: Some(diagnostics),
        selected,
    }
}

/// Build a pipeline for a project's discovered files.
///
/// A convenience for callers that hold an `ActionContext` rather than a
/// prepared index.
pub fn pipeline_for(files: Vec<DiscoveredFile>) -> Pipeline {
    Pipeline::new(files)
}

/// Emit the agenda as structured `tracing` records.
///
/// Metadata only: paths, evidence types, and counts. No file content
/// reaches these fields, so `--verbose` can never leak what search
/// itself would not have returned.
pub fn trace(agenda: &EvidenceAgenda, implementation: &str, project: Option<&str>) {
    tracing::debug!(
        retrieval_implementation = implementation,
        project = project.unwrap_or("-"),
        selected = agenda.selected.len(),
        whole_file_reads = agenda.reads.len(),
        symbol_spans = agenda.symbol_spans.len(),
        has_anchor = agenda.anchor.is_some(),
        "evidence agenda built"
    );
    for (index, item) in agenda.selected.iter().enumerate() {
        tracing::debug!(
            rank = index + 1,
            path = %item.path.display(),
            evidence_type = item.evidence_type.as_str(),
            "selected evidence"
        );
    }
    for (index, path) in agenda.context_order().iter().enumerate() {
        tracing::debug!(
            position = index + 1,
            path = %path.display(),
            "provider context order"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::EvidenceType;
    use std::fs;
    use tempfile::TempDir;

    /// The repository shape the reported failure had: a real
    /// implementation, a verbose test file repeating the question, and
    /// domain documentation.
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
            "src/session.rs",
            "//! Sessions.\n\
             \n\
             /// A stateful conversation.\n\
             pub struct SessionRuntime {\n\
             \x20   id: SessionId,\n\
             \x20   /// Held for the duration of a turn.\n\
             \x20   state: tokio::sync::Mutex<SessionState>,\n\
             \x20   current_cancel: std::sync::Mutex<Option<CancellationToken>>,\n\
             }\n\
             \n\
             impl SessionRuntime {\n\
             \x20   /// Store a turn result in the session state.\n\
             \x20   pub async fn record_state(&self, outcome: TurnOutcome) {\n\
             \x20       self.state.lock().await.push(outcome);\n\
             \x20   }\n\
             }\n",
        );

        let mut grounding = String::from("//! Grounding tests.\n\n");
        for i in 0..60 {
            grounding.push_str(&format!(
                "/// Explain SessionRuntime and how it stores session state.\n\
                 #[test]\n\
                 fn grounding_session_runtime_state_{i}() {{\n\
                 \x20   // SessionRuntime stores session state.\n\
                 }}\n\n"
            ));
        }
        write("tests/grounding.rs", &grounding);

        write(
            "docs/SESSIONS.md",
            "# Sessions\n\n\
             A session keeps conversation state in memory.\n\n\
             ## State\n\n\
             The session runtime owns the history.\n",
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

    fn agenda_for(question: &str) -> EvidenceAgenda {
        let (_dir, files) = repository();
        let pipeline = Pipeline::new(files);
        build(
            &pipeline,
            &PipelineInput {
                question: question.to_string(),
                ..Default::default()
            },
            &SelectionLimits::default(),
        )
    }

    #[test]
    fn the_declaring_file_is_quoted_by_span_not_read_whole() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        assert!(!agenda.symbol_spans.is_empty());
        assert_eq!(agenda.symbol_spans[0].path, PathBuf::from("src/session.rs"));
        // Reading it whole as well would spend the budget twice.
        assert!(
            !agenda.reads.iter().any(|r| r.path.ends_with("session.rs")),
            "{:?}",
            agenda.reads
        );
    }

    #[test]
    fn a_verbose_test_file_is_never_the_primary_context() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        let order = agenda.context_order();
        assert!(!order.is_empty());

        let test_position = order.iter().position(|p| p.ends_with("grounding.rs"));
        // The declaration is always in the context; the test may or may
        // not be, but it can never be the only thing or come last.
        assert!(order.iter().any(|p| p.ends_with("session.rs")), "{order:?}");
        if let Some(position) = test_position {
            assert!(position < order.len() - 1, "test file was last: {order:?}");
        }
    }

    #[test]
    fn whole_file_reads_are_bounded_when_a_bundle_exists() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        assert!(agenda.reads.len() <= MAX_READS_WITH_SYMBOL_EVIDENCE);
    }

    #[test]
    fn an_anchor_names_the_declaration() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        let anchor = agenda.anchor.expect("anchor");
        assert!(anchor.contains("SessionRuntime"));
        assert!(anchor.contains("src/session.rs"));
        assert!(anchor.contains("Do not describe the tests"));
    }

    #[test]
    fn diagnostics_report_the_provider_context_order() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        let report = agenda.diagnostics.expect("diagnostics");
        assert!(report.contains("progressive reads (whole file, in order):"));
        assert!(report.contains("final provider-context order:"));
    }

    #[test]
    fn a_question_with_no_entity_still_plans_reads() {
        let agenda = agenda_for("Explain the session architecture.");
        assert!(agenda.symbol_spans.is_empty());
        assert!(!agenda.reads.is_empty());
        assert!(agenda.reads.len() <= MAX_WHOLE_FILE_READS);
    }

    #[test]
    fn selected_evidence_carries_its_type() {
        let agenda = agenda_for("Explain SessionRuntime and how it stores session state.");
        assert!(
            agenda
                .selected
                .iter()
                .any(|s| s.evidence_type == EvidenceType::Definition),
            "{:?}",
            agenda.selected
        );
    }
}
