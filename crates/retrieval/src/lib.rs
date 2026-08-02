//! Two-stage retrieval: generate candidates broadly, then decide which of
//! them are actually worth showing the model.
//!
//! Lexical ranking alone answers "which lines mention these words". It
//! cannot answer "which file *defines* this thing", which is what an
//! explanation question needs. A long document that repeats the query
//! terms out-scores the twenty-line struct that defines them, every time,
//! and no amount of extra BM25 bonuses fixes that — the two are different
//! questions.
//!
//! So the pipeline is split into layers that each do one job:
//!
//! ```text
//! Query Planner  → what needs to be found          (plan)
//! Generators     → independent candidate sets      (candidate)
//! Fusion         → one ranking out of many         (fusion)
//! Reranker       → evidence quality, not term hits (rerank)
//! Selector       → a diverse, bounded final set    (select)
//! ```
//!
//! Layers stay separate on purpose: a scoring change belongs in the
//! reranker, a coverage change in the selector, and neither should require
//! touching the other.

pub mod agenda;
pub mod candidate;
pub mod evidence;
pub mod fusion;
pub mod pipeline;
pub mod plan;
pub mod rerank;
pub mod select;
pub mod symbols;
