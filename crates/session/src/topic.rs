//! What the conversation is currently about.
//!
//! A follow-up question is usually not self-contained: "Now explain how
//! cancellation works" means cancellation *of the thing we were just
//! discussing*, and "Now compare that with docs" means compare *that
//! decision*. Retrieval needs those referents, and the model cannot be
//! trusted to supply them — it is exactly the step that fails by
//! answering from general knowledge instead.
//!
//! So the session keeps a compact, structured record of the subject
//! instead of replaying the whole transcript into every query. Feeding
//! the entire conversation back would drown the current question in
//! earlier terms; feeding nothing back makes follow-ups unanswerable.

use std::collections::HashSet;

use orbit_core::SourceReference;

/// How many subject terms are carried between turns. Enough to describe a
/// topic, small enough that it cannot outweigh the current question.
const MAX_SUBJECT_TERMS: usize = 8;
/// How many recently cited files are remembered, for follow-ups that ask
/// about "the implementation" without naming it.
const MAX_RECENT_PATHS: usize = 8;
/// How many turns a subject term survives without being mentioned again.
/// A topic that stops coming up stops steering retrieval.
const SUBJECT_TTL_TURNS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubjectTerm {
    term: String,
    /// Turn number this term was last seen in.
    last_seen: u64,
}

/// The conversation's current subject, in structured form.
#[derive(Debug, Clone, Default)]
pub struct TopicState {
    subjects: Vec<SubjectTerm>,
    /// Registered projects the conversation is scoped to.
    projects: Vec<String>,
    /// Distinctive names seen in retrieved sources — file stems, symbol
    /// names — which make good follow-up query terms.
    entities: Vec<String>,
    /// Paths of sources cited recently, most recent first.
    recent_paths: Vec<String>,
    /// The last question could not be resolved on its own.
    unresolved_reference: bool,
    turn: u64,
}

impl TopicState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Terms to merge into the next query, most relevant first.
    ///
    /// Subject terms come first (what we are talking about), then
    /// entities (what was actually found). Projects are deliberately
    /// excluded: project scope is resolved separately and precisely, and
    /// a project name as a search term matches almost everything in it.
    pub fn context_terms(&self) -> Vec<String> {
        let mut terms: Vec<String> = self.subjects.iter().map(|s| s.term.clone()).collect();
        for entity in &self.entities {
            if !terms.contains(entity) {
                terms.push(entity.clone());
            }
        }
        terms
    }

    pub fn projects(&self) -> &[String] {
        &self.projects
    }

    pub fn recent_paths(&self) -> &[String] {
        &self.recent_paths
    }

    pub fn has_unresolved_reference(&self) -> bool {
        self.unresolved_reference
    }

    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty() && self.entities.is_empty()
    }

    pub fn set_projects(&mut self, projects: &[String]) {
        self.projects = projects.to_vec();
    }

    /// Record a question's own subject terms, and whether it stood on its
    /// own.
    ///
    /// A question that names its subject *replaces* nothing but refreshes
    /// what it mentions; a purely referential question leaves the subject
    /// intact, which is what lets it inherit one.
    pub fn observe_question(&mut self, terms: &[String], has_unresolved_reference: bool) {
        self.turn += 1;
        self.unresolved_reference = has_unresolved_reference;

        for term in terms {
            if let Some(existing) = self.subjects.iter_mut().find(|s| &s.term == term) {
                existing.last_seen = self.turn;
            } else {
                self.subjects.push(SubjectTerm {
                    term: term.clone(),
                    last_seen: self.turn,
                });
            }
        }

        // Drop stale subjects, then keep the most recently mentioned.
        let turn = self.turn;
        self.subjects
            .retain(|s| turn.saturating_sub(s.last_seen) < SUBJECT_TTL_TURNS as u64);
        self.subjects.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        self.subjects.truncate(MAX_SUBJECT_TERMS);
    }

    /// Record the sources a turn actually grounded on.
    ///
    /// Only real retrieval output reaches here, so the topic state can
    /// never be steered by a path the model merely mentioned.
    pub fn observe_sources(&mut self, sources: &[SourceReference]) {
        let mut seen: HashSet<String> = self.recent_paths.iter().cloned().collect();
        let mut fresh = Vec::new();

        for source in sources {
            let (_, path) = source.split_project_prefix();
            let display = path.to_string_lossy().to_string();
            if seen.insert(display.clone()) {
                fresh.push(display.clone());
            }
            // A file's own name is usually the best short label for what
            // it is about: `SESSIONS.md` → `session`.
            if let Some(stem_name) = path.file_stem().and_then(|s| s.to_str()) {
                for term in orbit_project::content_terms(stem_name) {
                    if !self.entities.contains(&term) {
                        self.entities.push(term);
                    }
                }
            }
        }

        // Most recent first, bounded.
        fresh.extend(std::mem::take(&mut self.recent_paths));
        fresh.truncate(MAX_RECENT_PATHS);
        self.recent_paths = fresh;

        if self.entities.len() > MAX_SUBJECT_TERMS {
            self.entities
                .drain(..self.entities.len() - MAX_SUBJECT_TERMS);
        }
    }

    /// Forget everything, for `/clear`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn terms(question: &str) -> Vec<String> {
        orbit_project::content_terms(question)
    }

    #[test]
    fn a_new_topic_state_contributes_nothing() {
        let state = TopicState::new();
        assert!(state.is_empty());
        assert!(state.context_terms().is_empty());
    }

    #[test]
    fn remembers_the_subject_of_a_question() {
        let mut state = TopicState::new();
        state.observe_question(&terms("Explain the session architecture."), false);
        let context = state.context_terms();
        assert!(
            context.contains(&orbit_project::stem("session")),
            "{context:?}"
        );
        assert!(
            context.contains(&orbit_project::stem("architecture")),
            "{context:?}"
        );
    }

    /// The exact reported follow-up: the second question carries one term,
    /// and needs the first question's subject to be answerable.
    #[test]
    fn a_follow_up_can_inherit_the_previous_subject() {
        let mut state = TopicState::new();
        state.observe_question(&terms("Explain the session architecture."), false);

        let analysis = orbit_project::analyze_with_context(
            "Now explain how cancellation works.",
            &state.context_terms(),
        );
        let all = analysis.all_terms();
        assert!(all.contains(&"cancel".to_string()), "{all:?}");
        assert!(all.contains(&orbit_project::stem("session")), "{all:?}");
    }

    #[test]
    fn records_entities_from_real_sources_only() {
        let mut state = TopicState::new();
        state.observe_sources(&[
            SourceReference::lines(PathBuf::from("docs:docs/SESSIONS.md"), 3, 4),
            SourceReference::whole_file(PathBuf::from("crates/session/src/runtime.rs")),
        ]);

        let context = state.context_terms();
        assert!(
            context.contains(&orbit_project::stem("sessions")),
            "{context:?}"
        );
        assert!(
            context.contains(&orbit_project::stem("runtime")),
            "{context:?}"
        );
        // The project prefix is stripped, so paths stay usable.
        assert!(
            state
                .recent_paths()
                .contains(&"docs/SESSIONS.md".to_string())
        );
    }

    #[test]
    fn a_stale_subject_stops_steering_retrieval() {
        let mut state = TopicState::new();
        state.observe_question(&terms("Explain the STM32 selection."), false);
        assert!(state.context_terms().contains(&"stm32".to_string()));

        for _ in 0..SUBJECT_TTL_TURNS {
            state.observe_question(&terms("Explain the watchdog timer."), false);
        }
        assert!(
            !state.context_terms().contains(&"stm32".to_string()),
            "a topic nobody has mentioned for several turns must expire: {:?}",
            state.context_terms()
        );
    }

    #[test]
    fn subject_terms_are_bounded() {
        let mut state = TopicState::new();
        for i in 0..50 {
            state.observe_question(&[format!("term{i}")], false);
        }
        assert!(state.context_terms().len() <= MAX_SUBJECT_TERMS * 2);
        assert!(state.subjects.len() <= MAX_SUBJECT_TERMS);
    }

    #[test]
    fn recent_paths_are_bounded_and_most_recent_first() {
        let mut state = TopicState::new();
        for i in 0..20 {
            state.observe_sources(&[SourceReference::whole_file(PathBuf::from(format!(
                "docs/file{i}.md"
            )))]);
        }
        assert!(state.recent_paths().len() <= MAX_RECENT_PATHS);
        assert_eq!(state.recent_paths()[0], "docs/file19.md");
    }

    #[test]
    fn reset_forgets_everything() {
        let mut state = TopicState::new();
        state.observe_question(&terms("Explain the session architecture."), false);
        state.observe_sources(&[SourceReference::whole_file(PathBuf::from(
            "docs/SESSIONS.md",
        ))]);
        state.reset();
        assert!(state.is_empty());
        assert!(state.recent_paths().is_empty());
    }

    #[test]
    fn tracks_project_scope_separately_from_search_terms() {
        let mut state = TopicState::new();
        state.set_projects(&["obc".to_string()]);
        assert_eq!(state.projects(), &["obc".to_string()]);
        assert!(
            !state.context_terms().contains(&"obc".to_string()),
            "a project name would match nearly everything inside it"
        );
    }
}
