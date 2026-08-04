//! Deterministic query analysis.
//!
//! A conversational question is not a search query. "Now explain how
//! cancellation works." contains one term worth retrieving on, and five
//! that would only add noise. This module turns a sentence into the terms
//! worth searching for, and reports whether the sentence can stand on its
//! own or refers back to something already discussed.
//!
//! Nothing here consults a model. Given the same question and the same
//! context, the same query always comes out.

use crate::lexical;

/// The result of analyzing one question.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnalyzedQuery {
    /// Subject terms from the question itself, in order of appearance,
    /// normalized and deduplicated.
    pub terms: Vec<String>,
    /// Terms carried in from the conversation's topic state, used only
    /// when the question does not stand on its own.
    pub context_terms: Vec<String>,
    /// The significant span of the original question, kept verbatim so an
    /// exact-phrase match can still be rewarded.
    pub phrase: Option<String>,
    /// The question refers back to earlier conversation (`that`, `it`,
    /// `isso`) rather than naming its whole subject.
    pub has_reference: bool,
    /// The question has no retrievable subject of its own — either it is
    /// pure filler, or its only subject is a reference word.
    pub needs_context: bool,
}

impl AnalyzedQuery {
    /// Every term to search for: the question's own terms first, then
    /// context terms that add something new. Order matters, because
    /// callers truncate to a budget.
    pub fn all_terms(&self) -> Vec<String> {
        let mut all = self.terms.clone();
        for term in &self.context_terms {
            if !all.contains(term) {
                all.push(term.clone());
            }
        }
        all
    }

    /// A search string for engines that take text rather than terms.
    pub fn to_search_string(&self) -> String {
        self.all_terms().join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.context_terms.is_empty()
    }
}

/// Maximum context terms merged into one query. Enough to carry a subject
/// across a follow-up, small enough that the previous turn cannot drown
/// out the current one.
pub const MAX_CONTEXT_TERMS: usize = 6;

/// Analyze `question` on its own, with no conversational context.
pub fn analyze(question: &str) -> AnalyzedQuery {
    analyze_with_context(question, &[])
}

/// Analyze `question`, drawing on `context_terms` from earlier turns.
///
/// Context is merged when the question refers back to something (`Now
/// compare that with docs`) or has too little of its own subject to
/// retrieve on (`Now explain how cancellation works` — one term). It is
/// deliberately *not* merged into a question that already names a full
/// subject, so a new topic is not polluted by the previous one.
pub fn analyze_with_context(question: &str, context_terms: &[String]) -> AnalyzedQuery {
    let terms = lexical::content_terms(question);

    let has_reference = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(lexical::is_reference_word);

    // One term is a topic, not a question: "cancellation" alone does not
    // say cancellation *of what*, and no terms at all says nothing.
    // Either way the question cannot be retrieved on by itself, which is
    // what carrying context in fixes.
    let needs_context = terms.len() <= 1;

    let merged = if needs_context || has_reference {
        context_terms
            .iter()
            .filter(|t| !terms.contains(t))
            .take(MAX_CONTEXT_TERMS)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    AnalyzedQuery {
        phrase: significant_phrase(question, &terms),
        terms,
        context_terms: merged,
        has_reference,
        needs_context,
    }
}

/// The longest run of consecutive non-filler words in the original
/// question, preserved verbatim.
///
/// This is what an exact-phrase bonus scores against: "session
/// architecture" should rank a line containing that exact wording above
/// one that merely mentions both words far apart.
fn significant_phrase(question: &str, terms: &[String]) -> Option<String> {
    if terms.len() < 2 {
        return None;
    }
    let words: Vec<&str> = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    let mut best: Vec<&str> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for word in words {
        if lexical::is_stopword(word) || word.chars().count() < 2 {
            if current.len() > best.len() {
                best = std::mem::take(&mut current);
            } else {
                current.clear();
            }
        } else {
            current.push(word);
        }
    }
    if current.len() > best.len() {
        best = current;
    }

    if best.len() >= 2 {
        Some(best.join(" ").to_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(question: &str) -> Vec<String> {
        analyze(question).terms
    }

    #[test]
    fn extracts_the_subject_of_a_conversational_question() {
        assert_eq!(
            terms("Explain the session architecture."),
            vec![lexical::stem("session"), lexical::stem("architecture")]
        );
    }

    #[test]
    fn strips_filler_that_would_otherwise_dominate() {
        // Every one of these words appears in the failing question and
        // none of them should drive retrieval.
        let analysis = analyze("Now explain how cancellation works.");
        assert_eq!(analysis.terms, vec!["cancel".to_string()]);
        for filler in ["now", "explain", "how", "works"] {
            assert!(!analysis.terms.contains(&filler.to_string()));
        }
    }

    #[test]
    fn a_thin_follow_up_asks_for_context() {
        let analysis = analyze("Now explain how cancellation works.");
        assert!(
            analysis.needs_context,
            "one term is not a self-contained subject"
        );
    }

    #[test]
    fn a_self_contained_question_does_not_pull_in_context() {
        let context = vec!["stm32".to_string(), "decision".to_string()];
        let analysis = analyze_with_context("Explain the session architecture.", &context);
        assert!(!analysis.needs_context);
        assert!(
            analysis.context_terms.is_empty(),
            "a new subject must not inherit the previous topic: {analysis:?}"
        );
    }

    #[test]
    fn a_thin_follow_up_merges_the_previous_topic() {
        let context = vec!["session".to_string(), "runtime".to_string()];
        let analysis = analyze_with_context("Now explain how cancellation works.", &context);
        assert_eq!(analysis.terms, vec!["cancel".to_string()]);
        assert_eq!(
            analysis.all_terms(),
            vec![
                "cancel".to_string(),
                "session".to_string(),
                "runtime".to_string()
            ]
        );
    }

    #[test]
    fn a_referring_question_merges_the_previous_topic() {
        let context = vec!["stm32".to_string(), "selection".to_string()];
        let analysis = analyze_with_context("Now compare that with docs.", &context);
        assert!(analysis.has_reference);
        assert!(
            analysis.all_terms().contains(&"stm32".to_string()),
            "{analysis:?}"
        );
        assert!(analysis.all_terms().contains(&"docs".to_string()));
    }

    #[test]
    fn context_is_bounded() {
        let context: Vec<String> = (0..50).map(|i| format!("term{i}")).collect();
        let analysis = analyze_with_context("Now explain cancellation.", &context);
        assert!(analysis.context_terms.len() <= MAX_CONTEXT_TERMS);
    }

    #[test]
    fn extracts_a_verbatim_phrase_for_exact_matching() {
        assert_eq!(
            analyze("Explain the session architecture.")
                .phrase
                .as_deref(),
            Some("session architecture")
        );
        assert_eq!(
            analyze("Why was STM32 selected?").phrase.as_deref(),
            Some("stm32 selected")
        );
    }

    #[test]
    fn a_single_term_question_has_no_phrase() {
        assert_eq!(analyze("cancellation").phrase, None);
    }

    #[test]
    fn portuguese_questions_are_analyzed_not_broken() {
        let analysis = analyze("Explique a arquitetura da sessão");
        assert!(
            analysis.terms.contains(&"arquitetura".to_string()),
            "{analysis:?}"
        );
        assert!(
            analysis.terms.contains(&"sessão".to_string()),
            "{analysis:?}"
        );
        assert!(!analysis.terms.contains(&"explique".to_string()));
    }

    #[test]
    fn an_empty_or_pure_filler_question_yields_nothing() {
        assert!(analyze("").is_empty());
        assert!(analyze("please explain how this works").needs_context);
    }

    #[test]
    fn is_deterministic() {
        let context = vec!["session".to_string()];
        let a = analyze_with_context("Now explain how cancellation works.", &context);
        let b = analyze_with_context("Now explain how cancellation works.", &context);
        assert_eq!(a, b);
    }
}
