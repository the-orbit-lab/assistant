//! Reciprocal Rank Fusion: one ranking out of several incomparable ones.
//!
//! The five generators produce scores on five unrelated scales — a BM25
//! sum, a symbol-order decay, a filename hit count. Adding or weighting
//! those numbers directly would be meaningless, and tuning the weights
//! would be endless. RRF ignores the scores entirely and uses only
//! *position*:
//!
//! ```text
//! score(d) = Σ  weight(g) / (K + rank_g(d))
//!         over generators g that ranked d
//! ```
//!
//! Two consequences make this the right fit:
//!
//! - **Agreement beats intensity.** A file ranked 3rd by three generators
//!   beats one ranked 1st by a single generator. That is exactly the
//!   signal missing from the reported failure, where one generator's
//!   enthusiasm for a verbose document went unchallenged.
//! - **No scale tuning.** Adding a sixth generator later needs no
//!   renormalization of the other five.
//!
//! `K` damps the difference between the very top ranks, so being 1st
//! rather than 2nd in one list is not decisive on its own. 60 is the
//! value from the original RRF paper and behaves well here.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::candidate::{Candidate, CandidateOrigin};

/// Rank-damping constant. See the module docs.
const K: f64 = 60.0;

/// Per-generator weight.
///
/// These are deliberately close to each other: fusion is supposed to
/// measure agreement, and a large weight would let one generator decide
/// the outcome alone, which is the failure mode this layer exists to
/// prevent. The symbol generator is trusted slightly more because a
/// declaration is a fact about the code, not a guess about relevance, and
/// context slightly less because it reflects the previous question rather
/// than this one.
fn weight(origin: CandidateOrigin) -> f64 {
    match origin {
        CandidateOrigin::Symbol => 1.3,
        CandidateOrigin::Lexical => 1.0,
        CandidateOrigin::Path => 1.0,
        CandidateOrigin::Heading => 1.0,
        CandidateOrigin::Context => 0.7,
    }
}

/// A candidate after fusion, carrying who proposed it and how highly.
#[derive(Debug, Clone)]
pub struct FusedCandidate {
    pub candidate: Candidate,
    pub score: f64,
    /// Every generator that proposed this evidence, with its rank there.
    pub origins: Vec<(CandidateOrigin, usize)>,
}

impl FusedCandidate {
    /// How many independent generators proposed this.
    ///
    /// Used by the reranker as a corroboration signal.
    pub fn agreement(&self) -> usize {
        self.origins.len()
    }

    pub fn proposed_by(&self, origin: CandidateOrigin) -> bool {
        self.origins.iter().any(|(o, _)| *o == origin)
    }

    /// A compact `lexical#1,path#3` description for diagnostics.
    pub fn origin_summary(&self) -> String {
        self.origins
            .iter()
            .map(|(origin, rank)| format!("{}#{}", origin.as_str(), rank + 1))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Where two candidates count as the same evidence.
///
/// Line-exact identity would treat the struct declaration and the `impl`
/// twenty lines below as unrelated, so agreement between a generator that
/// found one and a generator that found the other would be lost. Grouping
/// by overlapping spans instead lets them corroborate each other, while
/// still keeping distant parts of a long document distinct.
fn overlaps(a: &Candidate, b: &Candidate) -> bool {
    a.path == b.path && a.line_start <= b.line_end && b.line_start <= a.line_end
}

/// Fuse independent ranked lists into one.
///
/// Input lists must already be in each generator's own rank order;
/// positions in the slice *are* the ranks. Output is sorted by fused
/// score, with ties broken on path then line so the result is
/// byte-identical across runs.
pub fn fuse(lists: &[Vec<Candidate>]) -> Vec<FusedCandidate> {
    let mut fused: Vec<FusedCandidate> = Vec::new();
    // Only used to keep the linear scan below off the hot path for
    // corpora with many candidates in one file.
    let mut by_path: HashMap<PathBuf, Vec<usize>> = HashMap::new();

    for list in lists {
        for (rank, candidate) in list.iter().enumerate() {
            let contribution = weight(candidate.origin) / (K + rank as f64 + 1.0);

            let existing = by_path.get(&candidate.path).and_then(|positions| {
                positions
                    .iter()
                    .copied()
                    .find(|position| overlaps(&fused[*position].candidate, candidate))
            });

            match existing {
                Some(position) => {
                    let entry = &mut fused[position];
                    entry.score += contribution;
                    entry.origins.push((candidate.origin, rank));
                    // Prefer the most specific known location: a symbol
                    // declaration says more than "somewhere in this file".
                    if is_more_specific(candidate, &entry.candidate) {
                        let origins = std::mem::take(&mut entry.origins);
                        let score = entry.score;
                        *entry = FusedCandidate {
                            candidate: candidate.clone(),
                            score,
                            origins,
                        };
                    }
                }
                None => {
                    by_path
                        .entry(candidate.path.clone())
                        .or_default()
                        .push(fused.len());
                    fused.push(FusedCandidate {
                        candidate: candidate.clone(),
                        score: contribution,
                        origins: vec![(candidate.origin, rank)],
                    });
                }
            }
        }
    }

    for entry in &mut fused {
        entry.origins.sort();
    }

    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
            .then_with(|| a.candidate.line_start.cmp(&b.candidate.line_start))
    });
    fused
}

/// Is `new` a better description of this evidence than `current`?
///
/// A symbol declaration is the most specific thing we can point at; after
/// that, a narrower line span beats a wider one.
fn is_more_specific(new: &Candidate, current: &Candidate) -> bool {
    let rank = |c: &Candidate| match c.origin {
        CandidateOrigin::Symbol => 0,
        CandidateOrigin::Heading => 1,
        CandidateOrigin::Lexical => 2,
        CandidateOrigin::Path | CandidateOrigin::Context => 3,
    };
    match rank(new).cmp(&rank(current)) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => {
            let span = |c: &Candidate| c.line_end.saturating_sub(c.line_start);
            span(new) < span(current)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn candidate(path: &str, line: usize, origin: CandidateOrigin) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            line_start: line,
            line_end: line,
            excerpt: String::new(),
            section: None,
            origin,
            generator_score: 1.0,
            symbol_kind: None,
            defines_entity: None,
        }
    }

    /// The core property: broad agreement beats one generator's top pick.
    #[test]
    fn agreement_across_generators_beats_a_single_first_place() {
        let agreed = "crates/session/src/session.rs";
        let lexical = vec![
            candidate("docs/SEARCH.md", 10, CandidateOrigin::Lexical),
            candidate(agreed, 100, CandidateOrigin::Lexical),
        ];
        let symbol = vec![candidate(agreed, 100, CandidateOrigin::Symbol)];
        let path = vec![candidate(agreed, 100, CandidateOrigin::Path)];

        let fused = fuse(&[lexical, symbol, path]);
        assert_eq!(fused[0].candidate.path, PathBuf::from(agreed));
        assert_eq!(fused[0].agreement(), 3);
        assert_eq!(fused[1].candidate.path, PathBuf::from("docs/SEARCH.md"));
        assert_eq!(fused[1].agreement(), 1);
    }

    #[test]
    fn overlapping_spans_in_one_file_corroborate_each_other() {
        let mut declaration = candidate("a.rs", 10, CandidateOrigin::Symbol);
        declaration.line_end = 30;
        let mention = candidate("a.rs", 20, CandidateOrigin::Lexical);

        let fused = fuse(&[vec![mention], vec![declaration]]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].agreement(), 2);
        // The declaration is the more specific description, so it wins.
        assert_eq!(fused[0].candidate.origin, CandidateOrigin::Symbol);
    }

    #[test]
    fn distant_regions_of_one_file_stay_separate() {
        let early = candidate("docs/BIG.md", 5, CandidateOrigin::Lexical);
        let late = candidate("docs/BIG.md", 400, CandidateOrigin::Lexical);
        let fused = fuse(&[vec![early, late]]);
        assert_eq!(fused.len(), 2);
    }

    #[test]
    fn fusion_uses_rank_not_generator_score() {
        // A wildly larger generator score must not change the outcome.
        let mut huge = candidate("a.md", 1, CandidateOrigin::Lexical);
        huge.generator_score = 10_000.0;
        let mut small = candidate("b.rs", 1, CandidateOrigin::Symbol);
        small.generator_score = 0.001;

        let fused = fuse(&[vec![huge], vec![small]]);
        // The symbol generator's weight decides this, not the scores.
        assert_eq!(fused[0].candidate.path, PathBuf::from("b.rs"));
    }

    #[test]
    fn fusion_is_deterministic_including_ties() {
        let lists = vec![vec![
            candidate("b.rs", 1, CandidateOrigin::Lexical),
            candidate("a.rs", 1, CandidateOrigin::Lexical),
        ]];
        let first: Vec<PathBuf> = fuse(&lists).into_iter().map(|f| f.candidate.path).collect();
        let second: Vec<PathBuf> = fuse(&lists).into_iter().map(|f| f.candidate.path).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn empty_input_fuses_to_nothing() {
        assert!(fuse(&[]).is_empty());
        assert!(fuse(&[Vec::new(), Vec::new()]).is_empty());
    }

    #[test]
    fn origin_summary_reports_every_contributing_generator() {
        let path = "a.rs";
        let fused = fuse(&[
            vec![candidate(path, 1, CandidateOrigin::Lexical)],
            vec![candidate(path, 1, CandidateOrigin::Symbol)],
        ]);
        assert_eq!(fused[0].origin_summary(), "lexical#1,symbol#1");
    }
}
