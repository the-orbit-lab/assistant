//! Query planning: deciding *what needs to be found* before anything is
//! searched for.
//!
//! Separating this from candidate generation is what lets the rest of the
//! pipeline behave differently for "Explain SessionRuntime" than for "Why
//! was STM32 selected?" without either generator growing a special case.
//! The plan says which entities matter, which concepts surround them, and
//! what kind of evidence would actually answer the question.
//!
//! Planning is deterministic and works with no model available. A
//! local-model rewrite could refine a plan later, but it can only ever
//! narrow or reorder what deterministic extraction already produced —
//! never introduce a path or project of its own.

use serde::{Deserialize, Serialize};

use crate::symbols::SymbolIndex;

/// What kind of question this is, which decides what evidence is worth
/// preferring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalIntent {
    /// "Explain SessionRuntime" — a named entity is the subject.
    SymbolExplanation,
    /// "Explain the session architecture" — how a subsystem fits together.
    ArchitectureExplanation,
    /// "How is cancellation checked during streaming?" — a question about
    /// a *mechanism*, naming no type.
    ///
    /// Distinct from `ArchitectureExplanation`, which asks how parts fit
    /// together and is answered by documentation. A behavior question is
    /// answered by the code that runs: the check, the loop, the event
    /// emitted. Answering it from prose is what produced an invented API.
    BehaviorExplanation,
    /// "Where is the watchdog implemented?"
    ImplementationLocation,
    /// "Does it satisfy the requirement?"
    RequirementComparison,
    /// "Why was STM32 selected?"
    DecisionExplanation,
    /// "Why does the build fail with X?"
    FailureInvestigation,
    /// Anything else.
    GeneralProjectQuestion,
}

/// What a candidate *is*, used to diversify the final evidence set and to
/// keep incidental mentions out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    /// A declaration of the requested entity.
    Definition,
    /// Code that implements behavior.
    Implementation,
    /// Documentation about this specific subject.
    DomainDocumentation,
    /// Documentation about how the system fits together.
    Architecture,
    Requirement,
    Adr,
    Test,
    CommandOutput,
    /// The terms appear, but the passage is about something else.
    IncidentalReference,
}

impl EvidenceType {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceType::Definition => "Definition",
            EvidenceType::Implementation => "Implementation",
            EvidenceType::DomainDocumentation => "DomainDocumentation",
            EvidenceType::Architecture => "Architecture",
            EvidenceType::Requirement => "Requirement",
            EvidenceType::Adr => "ADR",
            EvidenceType::Test => "Test",
            EvidenceType::CommandOutput => "CommandOutput",
            EvidenceType::IncidentalReference => "IncidentalReference",
        }
    }
}

/// A structured description of what this question needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalPlan {
    pub intent: RetrievalIntent,
    /// Identifiers the question names, in their original form.
    pub entities: Vec<String>,
    /// Normalized prose terms surrounding the entities.
    pub concepts: Vec<String>,
    /// Normalized terms for the lexical generator.
    pub lexical_terms: Vec<String>,
    /// The question's distinctive phrase, for the exact-phrase bonus.
    ///
    /// Carried separately from the terms so the lexical generator needs
    /// only one pass over the corpus. Issuing a second query for the
    /// phrase would double the cost of the most expensive generator to
    /// obtain a signal the first pass can already apply.
    pub phrase: Option<String>,
    /// Identifiers for the symbol generator.
    pub symbol_queries: Vec<String>,
    pub target_projects: Vec<String>,
    /// Evidence types worth preferring, strongest first.
    pub preferred_evidence_types: Vec<EvidenceType>,
}

impl RetrievalPlan {
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty() && self.concepts.is_empty()
    }

    /// Terms describing what the question asks *about* `entity`.
    ///
    /// The concepts, plus the words of every other entity named. That
    /// second part matters: "Explain SessionRuntime and how it stores
    /// session state" names two entities, and stripping both from the
    /// concepts leaves `["stor"]` — which matches nothing, so ranking
    /// the type's methods falls back to source order and the answer is
    /// built from whichever constructor happens to come first.
    ///
    /// A sibling entity is the subject; the bundle's own name is not,
    /// because every method of `SessionRuntime` mentions sessions.
    pub fn terms_about(&self, entity: &str) -> Vec<String> {
        let own: Vec<String> = orbit_project::content_terms(entity);
        let mut terms = self.concepts.clone();
        for other in self.entities.iter().filter(|e| e.as_str() != entity) {
            for term in orbit_project::content_terms(other) {
                if !own.contains(&term) && !terms.contains(&term) {
                    terms.push(term);
                }
            }
        }
        terms
    }

    /// A one-line summary for `--verbose` diagnostics.
    pub fn summary(&self) -> String {
        format!(
            "intent={:?} entities={:?} concepts={:?}",
            self.intent, self.entities, self.concepts
        )
    }
}

/// Phrases that signal each intent. Checked in priority order, because a
/// question can contain several ("why was X selected and where is it
/// implemented") and the first match is the primary need.
const DECISION_MARKERS: &[&str] = &[
    "why was",
    "why did",
    "why is",
    "decision",
    "rationale",
    "chosen",
    "choose",
    "selected",
    "adr",
    "trade-off",
    "tradeoff",
    "instead of",
    "por que",
    "porque",
    "decisão",
    "escolhido",
];
const REQUIREMENT_MARKERS: &[&str] = &[
    "requirement",
    "requirements",
    "satisfy",
    "satisfies",
    "comply",
    "compliant",
    "spec says",
    "requisito",
    "requisitos",
];
const FAILURE_MARKERS: &[&str] = &[
    "error",
    "fails",
    "failing",
    "failure",
    "panic",
    "crash",
    "bug",
    "broken",
    "regression",
    "stack trace",
    "erro",
    "falha",
];
const LOCATION_MARKERS: &[&str] = &[
    "where is",
    "where are",
    "find the",
    "locate",
    "which file",
    "what file",
    "implemented in",
    "implementation of",
    "onde está",
    "onde fica",
];
/// Phrases asking how something *happens*, as opposed to how it is
/// arranged. "How is X checked", "what happens when", "when does X run".
/// These are answered by control flow, not by an overview.
const BEHAVIOR_MARKERS: &[&str] = &[
    "how is",
    "how are",
    "how does",
    "how do",
    "what happens",
    "when is",
    "when are",
    "when does",
    "checked",
    "handled",
    "enforced",
    "triggered",
    "propagated",
    "released",
    "during",
    "como funciona",
    "o que acontece",
    "quando",
];
const ARCHITECTURE_MARKERS: &[&str] = &[
    "architecture",
    "design",
    "how does",
    "how do",
    "how is",
    "fits together",
    "flow",
    "pipeline",
    "lifecycle",
    "overview",
    "arquitetura",
    "fluxo",
];

/// Words naming a structure rather than a process. A question carrying
/// one is about how parts are arranged, so it stays an architecture
/// question even when phrased as "how does ... work".
const STRUCTURAL_MARKERS: &[&str] = &[
    "architecture",
    "arquitetura",
    "layout",
    "structure",
    "overview",
    "fits together",
];

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Does this word look like a code identifier rather than prose?
///
/// `SessionRuntime` and `run_turn` are entities; `session` and `runtime`
/// on their own are concepts. Requiring an internal case change or an
/// underscore keeps ordinary capitalized words (a sentence's first word,
/// "Orbit") from being treated as symbols.
fn looks_like_identifier(word: &str) -> bool {
    if word.len() < 3 {
        return false;
    }
    let has_underscore = word.contains('_');
    let mut seen_lower = false;
    let mut inner_upper = false;
    for (index, c) in word.chars().enumerate() {
        if c.is_lowercase() {
            seen_lower = true;
        }
        if c.is_uppercase() && index > 0 && seen_lower {
            inner_upper = true;
        }
    }
    inner_upper || has_underscore
}

/// Extract entity candidates from the question text.
///
/// Three sources, in decreasing confidence: backtick-quoted spans (the
/// user marked it as code), identifier-shaped words, and multi-word runs
/// that normalize onto a symbol the index actually contains (so "session
/// runtime" finds `SessionRuntime` without guessing).
fn extract_entities(question: &str, index: Option<&SymbolIndex>) -> Vec<String> {
    let mut entities: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if !candidate.is_empty() && !entities.contains(&candidate) {
            entities.push(candidate);
        }
    };

    // 1. Anything the user quoted as code.
    let mut rest = question;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let quoted = after[..close].trim();
        if !quoted.is_empty() && !quoted.contains(' ') {
            push(quoted.to_string());
        }
        rest = &after[close + 1..];
    }

    let words: Vec<&str> = question
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();

    // 2. Identifier-shaped words.
    for word in &words {
        if looks_like_identifier(word) {
            push((*word).to_string());
        }
    }

    // 3. Multi-word runs that name a real symbol. Only the index can
    //    confirm this, which is what stops "session state" from being
    //    invented as an entity when no such type exists.
    if let Some(index) = index {
        for window in 2..=3usize {
            for pair in words.windows(window) {
                let joined = pair.join(" ");
                if index.contains(&joined)
                    && let Some(symbol) = index.lookup(&joined).first()
                {
                    push(symbol.name.clone());
                }
            }
        }
        // A single word can name a symbol too (`ActionRegistry` written
        // as `actionregistry`). Two guards keep this from turning
        // ordinary prose into entities, which is not hypothetical: a
        // repository containing `fn explain` would otherwise make
        // "explain" the subject of every question that used the verb.
        //
        // The word must survive query analysis — so filler and container
        // words are out — and it must name a *type*, not a function or
        // module. A `mod session` is not what "the session architecture"
        // is asking about.
        for word in &words {
            if word.len() < 4 || orbit_project::is_stopword(word) {
                continue;
            }
            if let Some(symbol) = index.lookup(word).first()
                && symbol.kind.is_definition()
            {
                push(symbol.name.clone());
            }
        }
    }

    entities
}

fn classify(question_lower: &str, has_entity: bool) -> RetrievalIntent {
    // Most specific first: a question can mention "architecture" while
    // really asking why a decision was made.
    if contains_any(question_lower, FAILURE_MARKERS) {
        return RetrievalIntent::FailureInvestigation;
    }
    if contains_any(question_lower, REQUIREMENT_MARKERS) {
        return RetrievalIntent::RequirementComparison;
    }
    if contains_any(question_lower, DECISION_MARKERS) {
        return RetrievalIntent::DecisionExplanation;
    }
    if contains_any(question_lower, LOCATION_MARKERS) {
        return RetrievalIntent::ImplementationLocation;
    }
    // A behavior question without a named type is the case that used to
    // fall through to `ArchitectureExplanation` and be answered from
    // documentation. Checked before the architecture markers because
    // "how does" appears in both lists: a question containing it is
    // asking about a mechanism unless it also names a structural word.
    if !has_entity
        && contains_any(question_lower, BEHAVIOR_MARKERS)
        && !contains_any(question_lower, STRUCTURAL_MARKERS)
    {
        return RetrievalIntent::BehaviorExplanation;
    }
    if has_entity {
        // "How does SessionRuntime cancel a turn" is about the subsystem
        // around a named type; "Explain SessionRuntime" is about the type.
        if contains_any(question_lower, ARCHITECTURE_MARKERS) {
            return RetrievalIntent::ArchitectureExplanation;
        }
        return RetrievalIntent::SymbolExplanation;
    }
    if contains_any(question_lower, ARCHITECTURE_MARKERS) {
        return RetrievalIntent::ArchitectureExplanation;
    }
    RetrievalIntent::GeneralProjectQuestion
}

fn preferred_types(intent: RetrievalIntent) -> Vec<EvidenceType> {
    use EvidenceType::*;
    match intent {
        // Implementation first, and deliberately ahead of documentation:
        // a mechanism is described accurately only by the code that runs
        // it. Documentation still qualifies, but it corroborates rather
        // than substitutes for the control flow.
        RetrievalIntent::BehaviorExplanation => {
            vec![Implementation, Definition, DomainDocumentation, Test]
        }
        RetrievalIntent::SymbolExplanation => {
            vec![
                Definition,
                Implementation,
                DomainDocumentation,
                Architecture,
            ]
        }
        RetrievalIntent::ArchitectureExplanation => {
            vec![
                Architecture,
                DomainDocumentation,
                Definition,
                Implementation,
            ]
        }
        RetrievalIntent::ImplementationLocation => {
            vec![Definition, Implementation, Test, DomainDocumentation]
        }
        RetrievalIntent::RequirementComparison => {
            vec![Requirement, Implementation, Test, DomainDocumentation]
        }
        RetrievalIntent::DecisionExplanation => {
            vec![Adr, Architecture, DomainDocumentation, Implementation]
        }
        RetrievalIntent::FailureInvestigation => {
            vec![Implementation, Test, CommandOutput, DomainDocumentation]
        }
        RetrievalIntent::GeneralProjectQuestion => {
            vec![
                DomainDocumentation,
                Architecture,
                Definition,
                Implementation,
            ]
        }
    }
}

/// Build a plan from a question, optional conversational context terms,
/// and the projects already selected.
pub fn plan(
    question: &str,
    context_terms: &[String],
    target_projects: &[String],
    index: Option<&SymbolIndex>,
) -> RetrievalPlan {
    let analysis = orbit_project::analyze_with_context(question, context_terms);
    let entities = extract_entities(question, index);

    // Concepts are the prose terms that are not just an entity restated,
    // so "SessionRuntime session state" contributes `state` as a concept
    // without `session` competing with the entity itself.
    let entity_terms: Vec<String> = entities
        .iter()
        .flat_map(|e| orbit_project::content_terms(e))
        .collect();
    let concepts: Vec<String> = analysis
        .all_terms()
        .into_iter()
        .filter(|t| !entity_terms.contains(t))
        .collect();

    let intent = classify(&question.to_lowercase(), !entities.is_empty());

    // The lexical generator gets the full term set — entities included,
    // so prose that spells a type out still matches — plus the phrase.
    let lexical_terms = analysis.all_terms();

    RetrievalPlan {
        intent,
        symbol_queries: entities.clone(),
        entities,
        concepts,
        lexical_terms,
        phrase: analysis.phrase.clone(),
        target_projects: target_projects.to_vec(),
        preferred_evidence_types: preferred_types(intent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolIndex;
    use std::path::Path;

    fn plan_for(question: &str) -> RetrievalPlan {
        plan(question, &[], &[], None)
    }

    #[test]
    fn the_reported_question_plans_as_a_symbol_explanation() {
        let plan = plan_for("Explain SessionRuntime and how it stores session state.");
        assert_eq!(plan.intent, RetrievalIntent::SymbolExplanation);
        assert_eq!(plan.entities, vec!["SessionRuntime".to_string()]);
        // Concepts are normalized terms, so `state` arrives stemmed. The
        // entity's own words (`session`, `runtime`) are excluded, because
        // the symbol generator already covers them.
        assert!(plan.concepts.contains(&"stat".to_string()), "{plan:?}");
        assert!(!plan.concepts.contains(&"session".to_string()), "{plan:?}");
        assert_eq!(plan.preferred_evidence_types[0], EvidenceType::Definition);
    }

    #[test]
    fn a_follow_up_inherits_its_entity_from_context() {
        // "How does cancellation work?" names no entity of its own; the
        // context supplies the subject.
        let plan = plan(
            "How does cancellation work?",
            &["session".to_string(), "runtime".to_string()],
            &[],
            None,
        );
        // A mechanism question, not a structural one: it asks what the
        // code does, so it must be answered by the code.
        assert_eq!(plan.intent, RetrievalIntent::BehaviorExplanation);
        assert_eq!(
            plan.preferred_evidence_types[0],
            EvidenceType::Implementation
        );
        assert!(plan.concepts.contains(&"cancel".to_string()), "{plan:?}");
        assert!(plan.concepts.contains(&"session".to_string()), "{plan:?}");
    }

    #[test]
    fn classifies_the_documented_intents() {
        assert_eq!(
            plan_for("Why was STM32 selected?").intent,
            RetrievalIntent::DecisionExplanation
        );
        assert_eq!(
            plan_for("Does it satisfy the requirement?").intent,
            RetrievalIntent::RequirementComparison
        );
        assert_eq!(
            plan_for("Where is the watchdog implemented?").intent,
            RetrievalIntent::ImplementationLocation
        );
        assert_eq!(
            plan_for("Why does the build fail with a panic?").intent,
            RetrievalIntent::FailureInvestigation
        );
        assert_eq!(
            plan_for("Explain the session architecture.").intent,
            RetrievalIntent::ArchitectureExplanation
        );
        assert_eq!(
            plan_for("Tell me about the watchdog.").intent,
            RetrievalIntent::GeneralProjectQuestion
        );
    }

    #[test]
    fn recognizes_identifier_shaped_entities() {
        assert_eq!(
            plan_for("Explain ActionRegistry architecture.").entities,
            vec!["ActionRegistry".to_string()]
        );
        assert_eq!(
            plan_for("What does cancel_current_turn do?").entities,
            vec!["cancel_current_turn".to_string()]
        );
        assert_eq!(
            plan_for("Explain `SourceReference` please.").entities,
            vec!["SourceReference".to_string()]
        );
    }

    /// Ordinary capitalized prose must not become an entity, or every
    /// sentence's first word would be treated as a type.
    #[test]
    fn ordinary_words_are_not_entities() {
        assert!(
            plan_for("Explain the session architecture.")
                .entities
                .is_empty()
        );
        assert!(plan_for("Orbit is an assistant.").entities.is_empty());
    }

    #[test]
    fn a_named_entity_with_an_architecture_word_is_an_architecture_question() {
        let plan = plan_for("Explain ActionRegistry architecture.");
        assert_eq!(plan.intent, RetrievalIntent::ArchitectureExplanation);
        assert_eq!(plan.entities, vec!["ActionRegistry".to_string()]);
        assert_eq!(plan.preferred_evidence_types[0], EvidenceType::Architecture);
    }

    /// A repository that happens to contain `fn explain` must not make
    /// "explain" the subject of every question that uses the verb — and
    /// `mod session` must not make "session" an entity, because the
    /// question is about the subsystem, not about a module item.
    #[test]
    fn prose_words_that_collide_with_symbol_names_are_not_entities() {
        let index = SymbolIndex::from_sources([(
            Path::new("src/lib.rs"),
            "pub mod session {}\n\
             pub fn explain() {}\n\
             pub struct SessionRuntime;\n",
        )]);

        let architecture = plan("Explain the session architecture.", &[], &[], Some(&index));
        assert!(
            architecture.entities.is_empty(),
            "{:?}",
            architecture.entities
        );

        // A real type is still found through the same path.
        let symbol = plan("Explain SessionRuntime.", &[], &[], Some(&index));
        assert_eq!(symbol.entities, vec!["SessionRuntime".to_string()]);
    }

    #[test]
    fn planning_is_deterministic() {
        let a = plan_for("Explain SessionRuntime and how it stores session state.");
        let b = plan_for("Explain SessionRuntime and how it stores session state.");
        assert_eq!(a, b);
    }

    #[test]
    fn a_plan_carries_the_selected_projects() {
        let plan = plan(
            "Explain SessionRuntime",
            &[],
            &["assistant".to_string()],
            None,
        );
        assert_eq!(plan.target_projects, vec!["assistant".to_string()]);
    }
}
