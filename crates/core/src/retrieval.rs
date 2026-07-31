//! How well a turn is grounded in repository evidence.
//!
//! Shared vocabulary between the retrieval implementations (single-project
//! and workspace) and the front ends that consume them, so "we did not
//! find enough to answer from" is reported the same way everywhere.

use serde::{Deserialize, Serialize};

/// Distinct files a turn must be grounded in before its evidence is
/// considered solid. One matching document can be a coincidence; two
/// independent ones rarely are.
pub const CONFIDENT_SOURCE_FILES: usize = 2;

/// How much repository evidence a deterministic retrieval step produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalConfidence {
    /// Nothing was retrieved. Any answer would come from the model's
    /// general knowledge, not from this repository.
    None,
    /// Something was retrieved, but thin enough that it may not support a
    /// confident answer.
    Low,
    /// Several independent files matched.
    High,
}

impl RetrievalConfidence {
    /// Classify by how many *distinct files* were cited. Counting files
    /// rather than sources stops one heavily-matching document from
    /// looking like broad agreement across a repository.
    pub fn from_distinct_files(files: usize) -> Self {
        match files {
            0 => RetrievalConfidence::None,
            n if n < CONFIDENT_SOURCE_FILES => RetrievalConfidence::Low,
            _ => RetrievalConfidence::High,
        }
    }

    /// Whether the model should be told not to answer as if its general
    /// knowledge described this repository.
    pub fn needs_grounding_warning(self) -> bool {
        !matches!(self, RetrievalConfidence::High)
    }
}
