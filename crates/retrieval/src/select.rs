//! Evidence selection: choosing a small, *diverse* set to answer from.
//!
//! Ranking and selecting are different jobs, and conflating them is the
//! second half of the reported failure. Even with a perfect ranking,
//! taking the top N by score gives N variations of the same claim: the
//! three highest-scoring passages of one long document, or five test
//! files that all exercise the same type. An answer built on that has one
//! source wearing five hats.
//!
//! So selection maximizes *marginal* value, in the spirit of Maximal
//! Marginal Relevance: at each step it takes the candidate with the best
//! score **after** subtracting how much it repeats what is already
//! chosen. Repetition is measured on the two axes that actually produce
//! redundancy here — the same file, and the same kind of evidence — and
//! hard caps bound both, so no single file or category can occupy the
//! whole set no matter how well it scores.
//!
//! The result is a set that can support an explanation: the declaration,
//! the implementation around it, and the document that describes why.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::plan::{EvidenceType, RetrievalPlan};
use crate::rerank::RankedEvidence;

/// Limits on the final evidence set.
#[derive(Debug, Clone)]
pub struct SelectionLimits {
    /// Total pieces of evidence selected.
    pub max_items: usize,
    /// Most pieces that may come from any one file.
    pub max_per_file: usize,
    /// Most pieces of any one evidence type.
    pub max_per_type: usize,
    /// Most incidental references, which are kept only to fill a set that
    /// would otherwise be too thin to answer from.
    pub max_incidental: usize,
    /// How strongly repetition is penalized, 0.0..=1.0.
    pub redundancy_penalty: f64,
    /// Most Test candidates allowed while implementation evidence
    /// exists.
    ///
    /// Tests corroborate behavior; they do not define it. A test file
    /// repeats the type's name on every line and describes what the
    /// author *expected*, which reads exactly like documentation to a
    /// ranking and not at all like it to a reader. When the repository
    /// has no implementation to offer, this cap lifts — "how is this
    /// tested" is a real question, and a test is better than nothing.
    pub max_tests_with_implementation: usize,
    /// Score below which a candidate is not evidence at all.
    ///
    /// Without a floor, a question the repository cannot answer still
    /// produces a full set — the six least-bad incidental mentions — and
    /// an answer built on those looks grounded while being about nothing.
    /// Returning less, or nothing, lets the grounding policy say so.
    pub min_score: f64,
}

impl Default for SelectionLimits {
    fn default() -> Self {
        Self {
            // Enough for a declaration, its implementation, and two
            // documents — beyond that a local model's context is the
            // binding constraint, not the retrieval.
            max_items: 6,
            max_per_file: 2,
            max_per_type: 3,
            max_incidental: 1,
            // One test may corroborate; two start to look like the
            // subject.
            max_tests_with_implementation: 1,
            redundancy_penalty: 0.6,
            // Zero is a meaningful boundary rather than a tuned constant:
            // the reranker's incidental penalty is what pushes a score
            // negative, so this admits exactly the candidates it did not
            // judge to be about something else.
            min_score: 0.0,
        }
    }
}

/// The chosen evidence, plus what the choosing did.
#[derive(Debug, Clone)]
pub struct Selection {
    pub items: Vec<RankedEvidence>,
    /// Candidates dropped because a cap was already full, with the reason.
    pub rejected: Vec<(RankedEvidence, RejectionReason)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    FileCapReached,
    TypeCapReached,
    IncidentalCapReached,
    TestCapReached,
    BelowScoreFloor,
    SetFull,
}

impl RejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RejectionReason::FileCapReached => "file cap",
            RejectionReason::TypeCapReached => "type cap",
            RejectionReason::IncidentalCapReached => "incidental cap",
            RejectionReason::TestCapReached => "test cap (implementation exists)",
            RejectionReason::BelowScoreFloor => "below score floor",
            RejectionReason::SetFull => "set full",
        }
    }
}

impl Selection {
    /// Distinct files in the selection, which is what grounding
    /// confidence is judged on.
    pub fn distinct_files(&self) -> usize {
        let mut paths: Vec<&PathBuf> = self.items.iter().map(|i| &i.candidate.path).collect();
        paths.sort();
        paths.dedup();
        paths.len()
    }

    /// Whether the selection contains a declaration of something the
    /// question named. An explanation without one is usually incomplete.
    pub fn has_definition(&self) -> bool {
        self.items
            .iter()
            .any(|i| i.evidence_type == EvidenceType::Definition)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Evidence that *is* the subject, rather than describing or exercising
/// it: the declaration and the code implementing it.
pub fn is_direct_implementation(evidence_type: EvidenceType) -> bool {
    matches!(
        evidence_type,
        EvidenceType::Definition | EvidenceType::Implementation
    )
}

/// How much a candidate repeats what is already selected, 0.0..=1.0.
///
/// Same file is the strongest form of redundancy: a second passage from a
/// document already quoted adds detail, not corroboration. Same evidence
/// type is weaker but real — a third test tells you little the first two
/// did not.
fn redundancy(candidate: &RankedEvidence, selected: &[RankedEvidence]) -> f64 {
    if selected.is_empty() {
        return 0.0;
    }
    let same_file = selected
        .iter()
        .filter(|s| s.candidate.path == candidate.candidate.path)
        .count();
    let same_type = selected
        .iter()
        .filter(|s| s.evidence_type == candidate.evidence_type)
        .count();

    let file_component = (same_file as f64 * 0.6).min(1.0);
    let type_component = (same_type as f64 * 0.25).min(0.75);
    file_component.max(type_component).min(1.0)
}

/// Select a diverse evidence set from a reranked list.
///
/// Greedy and deterministic: at each step the candidate with the highest
/// score-minus-redundancy wins, and exact ties break on path then line.
pub fn select(
    ranked: &[RankedEvidence],
    plan: &RetrievalPlan,
    limits: &SelectionLimits,
) -> Selection {
    let mut selected: Vec<RankedEvidence> = Vec::new();
    let mut rejected: Vec<(RankedEvidence, RejectionReason)> = Vec::new();
    let mut per_file: HashMap<PathBuf, usize> = HashMap::new();
    let mut per_type: HashMap<EvidenceType, usize> = HashMap::new();
    let mut incidental = 0usize;
    let mut tests = 0usize;
    let mut taken = vec![false; ranked.len()];

    // Does the repository have any direct implementation of the subject
    // at all? If it does, tests are held to a stricter cap and one slot
    // is held open so the implementation cannot be squeezed out by
    // better-scoring prose. If it does not, neither rule applies —
    // withholding the only evidence there is would be worse than the
    // problem they solve.
    let implementation_exists = ranked
        .iter()
        .any(|c| c.score >= limits.min_score && is_direct_implementation(c.evidence_type));

    // The plan's first preferred type is what this intent most needs. It
    // is not forced into the set — if the repository has no declaration
    // of the subject, saying so is better than promoting something else
    // into its place — but among comparable candidates it wins.
    let wanted = plan.preferred_evidence_types.first().copied();

    while selected.len() < limits.max_items {
        let mut best: Option<(usize, f64)> = None;

        for (index, candidate) in ranked.iter().enumerate() {
            if taken[index] || candidate.score < limits.min_score {
                continue;
            }

            let file_count = per_file
                .get(&candidate.candidate.path)
                .copied()
                .unwrap_or(0);
            if file_count >= limits.max_per_file {
                continue;
            }
            if per_type.get(&candidate.evidence_type).copied().unwrap_or(0) >= limits.max_per_type {
                continue;
            }
            if candidate.evidence_type == EvidenceType::IncidentalReference
                && incidental >= limits.max_incidental
            {
                continue;
            }
            if candidate.evidence_type == EvidenceType::Test
                && implementation_exists
                && tests >= limits.max_tests_with_implementation
            {
                continue;
            }
            // Hold the last slot for a definition or implementation while
            // the set still has neither. Without this, four documents and
            // a test fill the set and the answer explains the subject
            // from everything except the code that defines it.
            let last_slot = selected.len() + 1 == limits.max_items;
            if last_slot
                && implementation_exists
                && !selected
                    .iter()
                    .any(|s| is_direct_implementation(s.evidence_type))
                && !is_direct_implementation(candidate.evidence_type)
            {
                continue;
            }

            let mut adjusted =
                candidate.score - redundancy(candidate, &selected) * limits.redundancy_penalty;
            // A small nudge, applied only while the set still lacks the
            // type this intent is built around.
            if Some(candidate.evidence_type) == wanted
                && !selected.iter().any(|s| Some(s.evidence_type) == wanted)
            {
                adjusted += 0.15;
            }

            let better = match &best {
                None => true,
                Some((best_index, best_score)) => {
                    adjusted > *best_score
                        || (adjusted == *best_score
                            && (&candidate.candidate.path, candidate.candidate.line_start)
                                < (
                                    &ranked[*best_index].candidate.path,
                                    ranked[*best_index].candidate.line_start,
                                ))
                }
            };
            if better {
                best = Some((index, adjusted));
            }
        }

        let Some((index, _)) = best else { break };
        taken[index] = true;
        let chosen = ranked[index].clone();
        *per_file.entry(chosen.candidate.path.clone()).or_default() += 1;
        *per_type.entry(chosen.evidence_type).or_default() += 1;
        if chosen.evidence_type == EvidenceType::IncidentalReference {
            incidental += 1;
        }
        if chosen.evidence_type == EvidenceType::Test {
            tests += 1;
        }
        selected.push(chosen);
    }

    // Record why everything else was left out, for diagnostics.
    for (index, candidate) in ranked.iter().enumerate() {
        if taken[index] {
            continue;
        }
        let reason = if candidate.score < limits.min_score {
            RejectionReason::BelowScoreFloor
        } else if per_file
            .get(&candidate.candidate.path)
            .copied()
            .unwrap_or(0)
            >= limits.max_per_file
        {
            RejectionReason::FileCapReached
        } else if per_type.get(&candidate.evidence_type).copied().unwrap_or(0)
            >= limits.max_per_type
        {
            RejectionReason::TypeCapReached
        } else if candidate.evidence_type == EvidenceType::IncidentalReference
            && incidental >= limits.max_incidental
        {
            RejectionReason::IncidentalCapReached
        } else if candidate.evidence_type == EvidenceType::Test
            && implementation_exists
            && tests >= limits.max_tests_with_implementation
        {
            RejectionReason::TestCapReached
        } else {
            RejectionReason::SetFull
        };
        rejected.push((candidate.clone(), reason));
    }

    Selection {
        items: selected,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateOrigin};
    use crate::plan;
    use crate::rerank::EvidenceFeatures;

    fn evidence(path: &str, line: usize, kind: EvidenceType, score: f64) -> RankedEvidence {
        RankedEvidence {
            candidate: Candidate {
                path: PathBuf::from(path),
                line_start: line,
                line_end: line,
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
        }
    }

    fn symbol_plan() -> RetrievalPlan {
        plan::plan("Explain SessionRuntime.", &[], &[], None)
    }

    /// The reported failure, at the selection layer: one document's five
    /// best passages must not become the whole answer.
    #[test]
    fn one_file_cannot_fill_the_evidence_set() {
        let ranked: Vec<RankedEvidence> = (1..=5)
            .map(|i| {
                evidence(
                    "docs/SEARCH.md",
                    i * 10,
                    EvidenceType::DomainDocumentation,
                    10.0 - i as f64 * 0.1,
                )
            })
            .chain([evidence(
                "crates/session/src/session.rs",
                112,
                EvidenceType::Definition,
                1.0,
            )])
            .collect();

        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        let from_search = selection
            .items
            .iter()
            .filter(|i| i.candidate.path.ends_with("SEARCH.md"))
            .count();
        assert_eq!(from_search, 2, "{:?}", selection.items);
        assert!(selection.has_definition());
        assert_eq!(selection.distinct_files(), 2);
    }

    #[test]
    fn no_evidence_type_may_dominate() {
        let ranked: Vec<RankedEvidence> = (1..=6)
            .map(|i| {
                evidence(
                    &format!("crates/a/tests/t{i}.rs"),
                    1,
                    EvidenceType::Test,
                    10.0 - i as f64 * 0.1,
                )
            })
            .chain([evidence(
                "docs/A.md",
                1,
                EvidenceType::DomainDocumentation,
                1.0,
            )])
            .collect();

        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        let tests = selection
            .items
            .iter()
            .filter(|i| i.evidence_type == EvidenceType::Test)
            .count();
        assert_eq!(tests, 3);
        assert!(
            selection
                .items
                .iter()
                .any(|i| i.evidence_type == EvidenceType::DomainDocumentation)
        );
    }

    #[test]
    fn incidental_references_are_capped_but_not_banned() {
        let ranked: Vec<RankedEvidence> = (1..=4)
            .map(|i| {
                evidence(
                    &format!("docs/other{i}.md"),
                    1,
                    EvidenceType::IncidentalReference,
                    10.0,
                )
            })
            .collect();

        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert_eq!(selection.items.len(), 1);
        assert!(
            selection
                .rejected
                .iter()
                .any(|(_, reason)| *reason == RejectionReason::IncidentalCapReached)
        );
    }

    #[test]
    fn a_lower_scoring_definition_is_promoted_for_a_symbol_question() {
        let ranked = vec![
            evidence("docs/A.md", 1, EvidenceType::DomainDocumentation, 5.0),
            evidence("docs/B.md", 1, EvidenceType::Architecture, 4.9),
            evidence("crates/x/src/x.rs", 10, EvidenceType::Definition, 4.85),
        ];
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        // The nudge is small: it reorders comparable candidates, it does
        // not override a decisively better one.
        assert_eq!(
            selection.items[0].evidence_type,
            EvidenceType::Definition,
            "{:?}",
            selection.items
        );
    }

    #[test]
    fn a_missing_definition_is_not_invented() {
        let ranked = vec![
            evidence("docs/A.md", 1, EvidenceType::DomainDocumentation, 5.0),
            evidence("docs/B.md", 1, EvidenceType::Architecture, 4.0),
        ];
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert!(!selection.has_definition());
        assert_eq!(selection.items.len(), 2);
    }

    /// A question the repository cannot answer must select nothing, so
    /// the grounding policy can say so. Filling the set with the
    /// least-bad incidental mentions produces an answer that looks
    /// sourced and is about nothing.
    #[test]
    fn candidates_the_reranker_scored_negative_are_not_evidence() {
        let ranked: Vec<RankedEvidence> = (1..=5)
            .map(|i| {
                evidence(
                    &format!("crates/a/src/f{i}.rs"),
                    1,
                    EvidenceType::IncidentalReference,
                    -0.5,
                )
            })
            .collect();

        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert!(selection.is_empty());
        assert!(
            selection
                .rejected
                .iter()
                .all(|(_, reason)| *reason == RejectionReason::BelowScoreFloor)
        );
    }

    #[test]
    fn selection_is_deterministic_under_ties() {
        let ranked = vec![
            evidence("z.md", 1, EvidenceType::DomainDocumentation, 5.0),
            evidence("a.md", 1, EvidenceType::DomainDocumentation, 5.0),
            evidence("m.md", 1, EvidenceType::DomainDocumentation, 5.0),
        ];
        let first = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        let second = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        let paths = |s: &Selection| -> Vec<PathBuf> {
            s.items.iter().map(|i| i.candidate.path.clone()).collect()
        };
        assert_eq!(paths(&first), paths(&second));
        assert_eq!(paths(&first)[0], PathBuf::from("a.md"));
    }

    #[test]
    fn selecting_from_nothing_yields_nothing() {
        let selection = select(&[], &symbol_plan(), &SelectionLimits::default());
        assert!(selection.is_empty());
        assert_eq!(selection.distinct_files(), 0);
    }

    #[test]
    fn every_unselected_candidate_gets_a_recorded_reason() {
        let ranked: Vec<RankedEvidence> = (1..=12)
            .map(|i| {
                evidence(
                    &format!("docs/d{i}.md"),
                    1,
                    EvidenceType::DomainDocumentation,
                    10.0 - i as f64 * 0.1,
                )
            })
            .collect();
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert_eq!(selection.items.len() + selection.rejected.len(), 12);
    }

    /// Tests corroborate; they do not define. While the repository has
    /// any implementation of the subject, at most one test may appear.
    #[test]
    fn tests_cannot_crowd_out_the_implementation() {
        let ranked = vec![
            // Test files score high: they repeat the symbol name on
            // every line and read like documentation to a ranking.
            evidence(
                "crates/s/tests/session_runtime.rs",
                1,
                EvidenceType::Test,
                9.0,
            ),
            evidence("crates/s/tests/grounding.rs", 1, EvidenceType::Test, 8.9),
            evidence("crates/s/tests/more.rs", 1, EvidenceType::Test, 8.8),
            evidence(
                "crates/s/src/session.rs",
                112,
                EvidenceType::Definition,
                1.0,
            ),
        ];
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());

        let tests = selection
            .items
            .iter()
            .filter(|i| i.evidence_type == EvidenceType::Test)
            .count();
        assert_eq!(tests, 1, "{:?}", selection.items);
        assert!(selection.has_definition());
        assert!(
            selection
                .rejected
                .iter()
                .any(|(_, r)| *r == RejectionReason::TestCapReached)
        );
    }

    /// The cap lifts when there is nothing else: withholding the only
    /// evidence the repository has is worse than the problem it solves.
    #[test]
    fn tests_are_unrestricted_when_no_implementation_exists() {
        let ranked: Vec<RankedEvidence> = (1..=3)
            .map(|i| {
                evidence(
                    &format!("crates/s/tests/t{i}.rs"),
                    1,
                    EvidenceType::Test,
                    9.0,
                )
            })
            .collect();
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert_eq!(selection.items.len(), 3);
    }

    /// The last slot is held for the code that defines the subject, so
    /// better-scoring prose cannot squeeze it out entirely.
    #[test]
    fn a_slot_is_reserved_for_direct_implementation() {
        let mut ranked: Vec<RankedEvidence> = (1..=5)
            .map(|i| {
                evidence(
                    &format!("docs/d{i}.md"),
                    1,
                    EvidenceType::DomainDocumentation,
                    9.0 - i as f64 * 0.01,
                )
            })
            .collect();
        // Scores far below every document.
        ranked.push(evidence(
            "crates/s/src/s.rs",
            10,
            EvidenceType::Definition,
            0.2,
        ));

        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert!(
            selection.has_definition(),
            "reserved slot was not honored: {:?}",
            selection.items
        );
        // The per-type cap holds documentation to three, so the set is
        // three documents plus the definition rather than six documents.
        assert_eq!(selection.items.len(), 4, "{:?}", selection.items);
    }

    /// The reservation must not fabricate a definition that does not
    /// exist — a set of documents is still a valid answer.
    #[test]
    fn the_reserved_slot_does_not_invent_implementation() {
        let ranked: Vec<RankedEvidence> = (1..=8)
            .map(|i| {
                evidence(
                    &format!("docs/d{i}.md"),
                    1,
                    EvidenceType::DomainDocumentation,
                    9.0 - i as f64 * 0.01,
                )
            })
            .collect();
        let selection = select(&ranked, &symbol_plan(), &SelectionLimits::default());
        assert!(!selection.has_definition());
        assert!(!selection.is_empty());
    }
}
