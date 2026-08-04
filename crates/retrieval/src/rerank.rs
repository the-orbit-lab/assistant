//! Reranking: judging *evidence quality*, not term overlap.
//!
//! Fusion has already decided which candidates several generators agree
//! on. What it cannot decide is the question at the heart of the reported
//! failure: given a file that defines `SessionRuntime` and a much longer
//! file that mentions it in passing six times, which one actually answers
//! "Explain SessionRuntime"?
//!
//! Term statistics cannot answer that, because the two files differ in
//! *kind*, not in degree. So this layer computes features about what a
//! candidate **is**:
//!
//! - does it declare the thing the question named;
//! - is the file itself about the subject, or does it merely contain it;
//! - what sort of evidence is it (definition, architecture doc, ADR, test);
//! - how well does that sort match what this intent needs.
//!
//! Every feature is deterministic and computed from the repository. A
//! local model may reorder the shortlist afterwards (see
//! [`ModelRerank`]), but it can only permute candidates this layer
//! already produced — it can never add one, name a path, or read a file.

use std::path::Path;

use orbit_project::content_terms;

use crate::candidate::{Candidate, Corpus, FileFacts, FileKind, plan_terms};
use crate::fusion::FusedCandidate;
use crate::plan::{EvidenceType, RetrievalIntent, RetrievalPlan};
use crate::symbols::SymbolKind;

/// Below this share of a file's lines mentioning the subject, a file is
/// not *about* the subject. Chosen to sit well under the ratio of a
/// genuine topic document and well above an in-passing reference; the
/// threshold only applies once a file is long enough for a ratio to mean
/// anything (see [`MIN_LINES_FOR_RATIO`]).
const INCIDENTAL_MENTION_RATIO: f64 = 0.08;

/// Ratios are noise on short files: one mention in a six-line file is
/// 17% and says nothing. Short files are judged on their other features.
const MIN_LINES_FOR_RATIO: usize = 40;

/// Deterministic, inspectable features of one candidate.
///
/// Kept as data rather than folded straight into a number so that
/// `--verbose` can explain a ranking and a test can assert on a single
/// signal without reverse-engineering the total.
#[derive(Debug, Clone, Default)]
pub struct EvidenceFeatures {
    /// This candidate declares an entity the question named.
    pub defines_entity: bool,
    /// The file's name or title is about the question's subject.
    pub subject_alignment: f64,
    /// Share of the file's lines that mention the subject.
    pub mention_ratio: f64,
    pub mention_lines: usize,
    pub line_count: usize,
    /// How many independent generators proposed this.
    pub agreement: usize,
    /// A markdown heading names the subject.
    pub heading_match: bool,
    /// The terms appear, but the file is about something else.
    pub incidental: bool,
    /// Rank of this evidence type in the plan's preferences, if listed.
    pub intent_rank: Option<usize>,
}

/// A candidate with its type, its features, and its final score.
#[derive(Debug, Clone)]
pub struct RankedEvidence {
    pub candidate: Candidate,
    pub evidence_type: EvidenceType,
    pub features: EvidenceFeatures,
    /// Score from fusion, before this layer's judgment.
    pub fused_score: f64,
    pub score: f64,
    pub origin_summary: String,
}

impl RankedEvidence {
    /// A one-line explanation of why this ranked where it did.
    pub fn explain(&self) -> String {
        format!(
            "{}:{} [{}] score={:.4} fused={:.4} origins={} defines={} subject={:.2} \
             mentions={}/{} ({:.3}) heading={} agreement={} incidental={}",
            self.candidate.path.display(),
            self.candidate.line_start,
            self.evidence_type.as_str(),
            self.score,
            self.fused_score,
            self.origin_summary,
            self.features.defines_entity,
            self.features.subject_alignment,
            self.features.mention_lines,
            self.features.line_count,
            self.features.mention_ratio,
            self.features.heading_match,
            self.features.agreement,
            self.features.incidental,
        )
    }
}

/// Is this path a test?
///
/// Structural, not name-specific: Rust puts integration tests in a
/// `tests/` directory by convention, and unit-test modules live in files
/// whose own name says so. This classifies evidence; it never excludes a
/// file, because "how is this tested?" is a real question.
pub fn is_test_path(path: &Path) -> bool {
    path.components().any(|c| {
        let text = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        text == "tests" || text == "test"
    }) || path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| {
            let lower = s.to_ascii_lowercase();
            lower.starts_with("test_") || lower.ends_with("_test") || lower.ends_with("_tests")
        })
        .unwrap_or(false)
}

/// Is this an architecture decision record?
///
/// Detected by the ADR convention itself — a numbered file under a
/// directory named for decisions or ADRs — rather than by listing the
/// repository's actual filenames.
fn is_adr_path(path: &Path) -> bool {
    let in_adr_dir = path.components().any(|c| {
        let text = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        text == "adr" || text == "adrs" || text == "architecture" || text == "decisions"
    });
    let numbered = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| {
            let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.len() >= 3
        })
        .unwrap_or(false);
    in_adr_dir && numbered
}

/// Terms that mark a document as being about system structure or
/// requirements. These describe *document genres*, not this repository.
const ARCHITECTURE_DOC_TERMS: &[&str] = &["architectur", "design", "overview", "structur"];
const REQUIREMENT_DOC_TERMS: &[&str] = &["requir", "spec", "specif", "acceptanc", "criteria"];

fn any_term_in(haystack: &[String], needles: &[&str]) -> bool {
    haystack
        .iter()
        .any(|term| needles.iter().any(|needle| term.starts_with(needle)))
}

/// How much the file's own identity — its name and its title — is about
/// the question's subject.
///
/// This is the signal that separates `docs/SESSIONS.md` from
/// `docs/SEARCH.md` for a question about sessions, regardless of how many
/// times either says the word.
fn subject_alignment(file: &FileFacts, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let filename_hits = terms
        .iter()
        .filter(|t| file.filename_terms.contains(t))
        .count() as f64;
    let title_terms: Vec<String> = file.title.as_deref().map(content_terms).unwrap_or_default();
    let title_hits = terms.iter().filter(|t| title_terms.contains(t)).count() as f64;
    let path_hits = terms.iter().filter(|t| file.path_terms.contains(t)).count() as f64;

    let total = terms.len() as f64;
    // Filename and title are what a file calls itself; a directory
    // component is weaker, because it is shared with every sibling.
    (filename_hits / total) * 0.5 + (title_hits / total) * 0.35 + (path_hits / total) * 0.15
}

/// Classify what a candidate is.
fn classify(
    candidate: &Candidate,
    file: Option<&FileFacts>,
    features: &EvidenceFeatures,
) -> EvidenceType {
    let path = candidate.path.as_path();

    if is_test_path(path) {
        return EvidenceType::Test;
    }
    if is_adr_path(path) {
        return EvidenceType::Adr;
    }

    let kind = file.map(|f| f.kind).unwrap_or(FileKind::Other);

    if matches!(kind, FileKind::Rust) {
        return match candidate.symbol_kind {
            Some(k) if k.is_definition() => EvidenceType::Definition,
            Some(_) => EvidenceType::Implementation,
            // A lexical hit in a Rust file that declares nothing is
            // implementation unless the file is not about the subject.
            None if features.incidental => EvidenceType::IncidentalReference,
            None => EvidenceType::Implementation,
        };
    }

    if features.incidental {
        return EvidenceType::IncidentalReference;
    }

    let mut identity: Vec<String> = file.map(|f| f.filename_terms.clone()).unwrap_or_default();
    if let Some(title) = file.and_then(|f| f.title.as_deref()) {
        identity.extend(content_terms(title));
    }
    if let Some(section) = candidate.section.as_deref() {
        identity.extend(content_terms(section));
    }

    if any_term_in(&identity, REQUIREMENT_DOC_TERMS) {
        return EvidenceType::Requirement;
    }
    if any_term_in(&identity, ARCHITECTURE_DOC_TERMS) {
        return EvidenceType::Architecture;
    }

    match kind {
        FileKind::Markdown | FileKind::Config | FileKind::Other => {
            EvidenceType::DomainDocumentation
        }
        FileKind::Rust => EvidenceType::Implementation,
    }
}

/// Compute the deterministic features of one fused candidate.
fn features(
    fused: &FusedCandidate,
    file: Option<&FileFacts>,
    terms: &[String],
) -> EvidenceFeatures {
    let mut features = EvidenceFeatures {
        defines_entity: fused
            .candidate
            .symbol_kind
            .map(SymbolKind::is_definition)
            .unwrap_or(false)
            && fused.candidate.defines_entity.is_some(),
        agreement: fused.agreement(),
        heading_match: fused.candidate.section.is_some()
            && fused.proposed_by(crate::candidate::CandidateOrigin::Heading),
        ..Default::default()
    };

    if let Some(file) = file {
        features.line_count = file.line_count;
        features.mention_lines = file.mention_lines(terms);
        features.mention_ratio = if file.line_count > 0 {
            features.mention_lines as f64 / file.line_count as f64
        } else {
            0.0
        };
        features.subject_alignment = subject_alignment(file, terms);
    }

    // A file is an incidental reference when nothing about it says it is
    // about the subject: it declares none of the named entities, it is
    // not named or titled for them, no heading announces them, and the
    // mentions are sparse relative to its length.
    features.incidental = !features.defines_entity
        && features.subject_alignment == 0.0
        && !features.heading_match
        && features.line_count >= MIN_LINES_FOR_RATIO
        && features.mention_ratio < INCIDENTAL_MENTION_RATIO;

    // `intent_rank` is filled in by the caller, once the type is known.
    features
}

/// Weights for the final deterministic score.
///
/// The fused score carries "several generators agreed"; everything added
/// here carries "this is the right *kind* of evidence". The definition
/// bonus is the largest single term, because for an explanation question
/// the declaration is the answer and nothing else substitutes for it.
const DEFINITION_BONUS: f64 = 0.60;
const SUBJECT_ALIGNMENT_WEIGHT: f64 = 0.45;
const HEADING_BONUS: f64 = 0.20;
const AGREEMENT_WEIGHT: f64 = 0.10;
/// Intent alignment decays with position in the plan's preference list.
const INTENT_WEIGHT: f64 = 0.30;
/// An incidental mention is demoted, not deleted: the classification is a
/// judgment about likelihood, and a hard exclusion would make a genuinely
/// relevant passage unreachable. The selector caps how many may appear.
const INCIDENTAL_PENALTY: f64 = 0.70;
/// Fusion scores are small by construction (≈1/60 per first place); this
/// puts them on the same scale as the feature bonuses so neither side is
/// decorative.
const FUSED_SCALE: f64 = 12.0;

fn score(fused_score: f64, features: &EvidenceFeatures) -> f64 {
    let mut score = fused_score * FUSED_SCALE;

    if features.defines_entity {
        score += DEFINITION_BONUS;
    }
    score += features.subject_alignment * SUBJECT_ALIGNMENT_WEIGHT;
    if features.heading_match {
        score += HEADING_BONUS;
    }
    // Corroboration past the second generator adds little.
    score += (features.agreement.min(3) as f64 - 1.0).max(0.0) * AGREEMENT_WEIGHT;

    if let Some(rank) = features.intent_rank {
        score += INTENT_WEIGHT / (1.0 + rank as f64);
    }

    if features.incidental {
        score -= INCIDENTAL_PENALTY;
    }

    score
}

/// Rerank fused candidates by evidence quality.
///
/// Output is sorted best-first and is fully deterministic: ties break on
/// path, then line.
pub fn rerank(
    fused: &[FusedCandidate],
    corpus: &Corpus,
    plan: &RetrievalPlan,
) -> Vec<RankedEvidence> {
    let terms = plan_terms(plan);

    let mut ranked: Vec<RankedEvidence> = fused
        .iter()
        .map(|entry| {
            let file = corpus.get(&entry.candidate.path);
            let mut computed = features(entry, file, &terms);
            let evidence_type = classify(&entry.candidate, file, &computed);
            computed.intent_rank = plan
                .preferred_evidence_types
                .iter()
                .position(|t| *t == evidence_type);

            RankedEvidence {
                score: score(entry.score, &computed),
                evidence_type,
                features: computed,
                candidate: entry.candidate.clone(),
                fused_score: entry.score,
                origin_summary: entry.origin_summary(),
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
            .then_with(|| a.candidate.line_start.cmp(&b.candidate.line_start))
    });
    ranked
}

/// How many candidates a model reranker may ever be shown.
///
/// Bounded so a reranking call has a predictable cost and cannot become
/// the slow part of answering a question.
pub const MODEL_RERANK_LIMIT: usize = 12;

/// A model's proposed reordering, after validation.
#[derive(Debug, Clone)]
pub struct ModelRerank {
    /// Indices into the shortlist, best first. Always a permutation of a
    /// subset of `0..shortlist.len()` with no repeats.
    pub order: Vec<usize>,
}

/// Validate a model's reranking response against the shortlist it was
/// given.
///
/// Deliberately strict. A reranker is an optional refinement, and a
/// malformed or hallucinated response must degrade to the deterministic
/// order rather than corrupt it — so anything unexpected returns `None`:
/// an index that does not exist, a repeat, or an empty list. The model
/// never supplies a path, so no response can introduce a file that
/// retrieval did not already produce.
pub fn validate_model_rerank(raw: &str, shortlist_len: usize) -> Option<ModelRerank> {
    if shortlist_len == 0 {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let array = match &value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map.get("order")?.as_array()?.clone(),
        _ => return None,
    };
    if array.is_empty() || array.len() > shortlist_len {
        return None;
    }

    let mut order = Vec::with_capacity(array.len());
    for item in array {
        let index = usize::try_from(item.as_u64()?).ok()?;
        if index >= shortlist_len || order.contains(&index) {
            return None;
        }
        order.push(index);
    }
    Some(ModelRerank { order })
}

/// Apply a validated reordering to the shortlist.
///
/// Candidates the model omitted keep their deterministic order and follow
/// the ones it ranked, so a partial answer can only promote — never drop
/// evidence.
pub fn apply_model_rerank(
    ranked: Vec<RankedEvidence>,
    rerank: &ModelRerank,
) -> Vec<RankedEvidence> {
    let mut used = vec![false; ranked.len()];
    let mut reordered = Vec::with_capacity(ranked.len());
    for index in &rerank.order {
        if let Some(item) = ranked.get(*index) {
            reordered.push(item.clone());
            used[*index] = true;
        }
    }
    for (index, item) in ranked.into_iter().enumerate() {
        if !used[index] {
            reordered.push(item);
        }
    }
    reordered
}

/// The prompt shown to an optional local reranker.
///
/// It receives paths, evidence types, and short excerpts — never file
/// contents beyond what retrieval already returned — and is asked only
/// for an ordering.
pub fn model_rerank_prompt(question: &str, shortlist: &[RankedEvidence]) -> String {
    let mut prompt = String::from(
        "Rank the numbered candidates by how well each answers the question.\n\
         Reply with a JSON array of candidate numbers, best first, and nothing else.\n\
         Use only the numbers shown. Do not add paths, files, or commentary.\n\n",
    );
    prompt.push_str(&format!("Question: {question}\n\nCandidates:\n"));
    for (index, item) in shortlist.iter().enumerate() {
        let excerpt: String = item.candidate.excerpt.chars().take(160).collect();
        prompt.push_str(&format!(
            "{index}. [{}] {}:{} — {}\n",
            item.evidence_type.as_str(),
            item.candidate.path.display(),
            item.candidate.line_start,
            excerpt.replace('\n', " ").trim()
        ));
    }
    prompt
}

/// Whether this intent benefits from a model reranking pass at all.
///
/// Deterministic features already settle the cases they were designed
/// for; spending a model call on them adds latency and non-reproducibility
/// for nothing.
pub fn wants_model_rerank(plan: &RetrievalPlan) -> bool {
    matches!(
        plan.intent,
        RetrievalIntent::DecisionExplanation
            | RetrievalIntent::RequirementComparison
            | RetrievalIntent::FailureInvestigation
            | RetrievalIntent::GeneralProjectQuestion
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_and_adr_paths_are_recognized_structurally() {
        assert!(is_test_path(Path::new("crates/session/tests/grounding.rs")));
        assert!(is_test_path(Path::new("src/session_tests.rs")));
        assert!(!is_test_path(Path::new("crates/session/src/session.rs")));

        assert!(is_adr_path(Path::new(
            "docs/architecture/0001-scope-deviation.md"
        )));
        assert!(!is_adr_path(Path::new("docs/ARCHITECTURE.md")));
    }

    #[test]
    fn a_malformed_rerank_response_is_rejected() {
        assert!(validate_model_rerank("[0, 1, 2]", 3).is_some());
        assert!(validate_model_rerank(r#"{"order": [2, 0]}"#, 3).is_some());
        // Out of range, repeated, empty, wrong shape, prose.
        assert!(validate_model_rerank("[0, 9]", 3).is_none());
        assert!(validate_model_rerank("[1, 1]", 3).is_none());
        assert!(validate_model_rerank("[]", 3).is_none());
        assert!(validate_model_rerank(r#"{"paths": ["a.rs"]}"#, 3).is_none());
        assert!(validate_model_rerank("the first one", 3).is_none());
        assert!(validate_model_rerank("[0]", 0).is_none());
        // Longer than the shortlist means it invented entries.
        assert!(validate_model_rerank("[0, 1, 2, 0]", 2).is_none());
    }

    #[test]
    fn a_partial_rerank_promotes_but_never_drops() {
        let item = |path: &str| RankedEvidence {
            candidate: Candidate {
                path: PathBuf::from(path),
                line_start: 1,
                line_end: 1,
                excerpt: String::new(),
                section: None,
                origin: crate::candidate::CandidateOrigin::Lexical,
                generator_score: 1.0,
                symbol_kind: None,
                defines_entity: None,
            },
            evidence_type: EvidenceType::DomainDocumentation,
            features: EvidenceFeatures::default(),
            fused_score: 0.0,
            score: 0.0,
            origin_summary: String::new(),
        };
        let ranked = vec![item("a.md"), item("b.md"), item("c.md")];
        let rerank = validate_model_rerank("[2]", 3).unwrap();
        let applied = apply_model_rerank(ranked, &rerank);

        let paths: Vec<String> = applied
            .iter()
            .map(|r| r.candidate.path.to_string_lossy().to_string())
            .collect();
        assert_eq!(paths, vec!["c.md", "a.md", "b.md"]);
    }

    #[test]
    fn the_rerank_prompt_asks_only_for_an_ordering() {
        let prompt = model_rerank_prompt("Explain SessionRuntime", &[]);
        assert!(prompt.contains("JSON array"));
        assert!(prompt.contains("Do not add paths"));
    }

    #[test]
    fn deterministic_intents_skip_the_model_reranker() {
        let symbol = crate::plan::plan("Explain SessionRuntime", &[], &[], None);
        assert!(!wants_model_rerank(&symbol));
        let decision = crate::plan::plan("Why was STM32 selected?", &[], &[], None);
        assert!(wants_model_rerank(&decision));
    }
}
