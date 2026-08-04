//! Catching an answer that describes code the repository does not have.
//!
//! Retrieval can put the right evidence in front of a model and the model
//! can still fill a gap with a plausible API. Asked how cancellation is
//! checked, Orbit once answered with `SessionRuntime` fields called
//! `session_id` and `permissions` and methods called `start_session()`
//! and `handle_request()` — none of which this repository declares. The
//! answer read as a description of the project and was a description of
//! how such a project is usually written.
//!
//! The check here is deliberately narrow, because a broad one would be
//! worse than none. It flags only identifiers that:
//!
//! - the answer presents *as code* (backticked, or written with call
//!   parentheses), so ordinary prose is never touched;
//! - look like Rust identifiers rather than English words;
//! - the repository does not declare **anywhere**.
//!
//! That last condition is the important one. A symbol that exists but
//! was not retrieved is a retrieval gap, and the model may legitimately
//! know of it from a doc comment in the evidence. A symbol that exists
//! nowhere in the repository cannot have come from the evidence at all,
//! so it was invented — and that is a fact about the text, not a
//! judgment about the model.

use std::collections::HashSet;

/// An identifier the answer presented as this repository's API that the
/// repository does not declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSymbol {
    pub name: String,
    /// How it appeared: backticked, or called with parentheses.
    pub shape: Shape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `` `foo` `` — marked as code by the author.
    Quoted,
    /// `foo()` — written as a call.
    Call,
}

/// Words that look like identifiers but are ordinary English, or belong
/// to the language and its ecosystem rather than to this repository.
///
/// A false positive here is worse than a miss: flagging `Vec` or `Result`
/// as invented would make the warning noise, and noise gets ignored.
const NOT_PROJECT_SYMBOLS: &[&str] = &[
    "Vec", "String", "Option", "Result", "Ok", "Err", "Some", "None", "Box", "Arc", "Rc", "Mutex",
    "RwLock", "HashMap", "HashSet", "BTreeMap", "PathBuf", "Path", "Self", "Default", "Clone",
    "Debug", "Send", "Sync", "Iterator", "Duration", "Instant", "unwrap", "clone", "await",
    "async", "impl", "struct", "enum", "trait", "match", "true", "false",
];

/// Does this look like a Rust identifier rather than a prose word?
///
/// Requires an internal case change, an underscore, or a leading capital
/// followed by a lowercase run — the shapes Rust names actually take.
/// A lowercase single word is left alone, because it is far more likely
/// to be English.
fn looks_like_symbol(word: &str) -> bool {
    if word.len() < 3 || NOT_PROJECT_SYMBOLS.contains(&word) {
        return false;
    }
    if !word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    if word.contains('_') {
        return true;
    }
    let mut seen_lower = false;
    for (index, c) in word.chars().enumerate() {
        if c.is_lowercase() {
            seen_lower = true;
        }
        if c.is_uppercase() && index > 0 && seen_lower {
            return true;
        }
    }
    false
}

/// Extract every backticked span and every `name(` call from `answer`.
fn candidates(answer: &str) -> Vec<(String, Shape)> {
    let mut found: Vec<(String, Shape)> = Vec::new();

    // Backticked spans. A fenced code block is skipped: it is usually a
    // verbatim quotation of the evidence, and its contents are not the
    // model's claims about the API.
    let mut in_fence = false;
    for line in answer.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            let quoted = after[..close]
                .trim()
                .trim_end_matches("()")
                .trim_start_matches('.')
                .trim();
            // Take the last path segment: `SessionRuntime::start_session`
            // is a claim about `start_session`.
            if let Some(name) = quoted.rsplit("::").next()
                && looks_like_symbol(name)
            {
                found.push((name.to_string(), Shape::Quoted));
            }
            rest = &after[close + 1..];
        }
    }

    // `name(` outside code fences, which is how prose writes a call.
    let mut in_fence = false;
    for line in answer.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let bytes: Vec<char> = line.chars().collect();
        let mut start = None;
        for (index, c) in bytes.iter().enumerate() {
            let is_word = c.is_ascii_alphanumeric() || *c == '_';
            match (is_word, start) {
                (true, None) => start = Some(index),
                (false, Some(begin)) => {
                    if *c == '(' {
                        let word: String = bytes[begin..index].iter().collect();
                        if looks_like_symbol(&word) {
                            found.push((word, Shape::Call));
                        }
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }

    found
}

/// Identifiers the answer presents as this repository's API that the
/// repository does not declare anywhere.
///
/// `declared` is every symbol name the project's own index holds.
/// Deterministic and order-stable: results are sorted and deduplicated.
pub fn unknown_symbols(answer: &str, declared: &HashSet<String>) -> Vec<UnknownSymbol> {
    let mut unknown: Vec<UnknownSymbol> = Vec::new();
    for (name, shape) in candidates(answer) {
        if declared.contains(&name) {
            continue;
        }
        if unknown.iter().any(|u| u.name == name) {
            continue;
        }
        unknown.push(UnknownSymbol { name, shape });
    }
    unknown.sort_by(|a, b| a.name.cmp(&b.name));
    unknown
}

/// A note appended to an answer that named symbols the repository does
/// not declare.
///
/// Flagging rather than deleting: the surrounding explanation may still
/// be useful, and silently removing sentences would leave an answer that
/// reads as fully grounded when part of it was not.
pub fn unknown_symbol_notice(unknown: &[UnknownSymbol]) -> Option<String> {
    if unknown.is_empty() {
        return None;
    }
    let names: Vec<String> = unknown.iter().map(|u| format!("`{}`", u.name)).collect();
    Some(format!(
        "\n\n⚠ Not found in this repository: {}. \
         These were not in the retrieved evidence and this project does not declare them, \
         so treat any claim about them as unverified.",
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The reported hallucination, exactly.
    #[test]
    fn invented_methods_are_flagged() {
        let answer = "The runtime exposes `start_session()` and `handle_request()`, \
                      alongside fields `session_id` and `permissions`.";
        let unknown = unknown_symbols(answer, &declared(&["SessionRuntime"]));
        let names: Vec<&str> = unknown.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["handle_request", "session_id", "start_session"]);
        // `permissions` is deliberately *not* flagged: as a bare
        // lowercase word it is indistinguishable from English, and a
        // check that flagged it would also flag "the session stores
        // `permissions`" in an accurate answer. Missing one invented
        // name costs less than a warning nobody trusts.
    }

    #[test]
    fn symbols_the_repository_declares_are_not_flagged() {
        let answer = "`SessionRuntime` holds `current_cancel` and calls `run_turn()`.";
        let unknown = unknown_symbols(
            answer,
            &declared(&["SessionRuntime", "current_cancel", "run_turn"]),
        );
        assert!(unknown.is_empty(), "{unknown:?}");
    }

    /// A broad check would be worse than none: ordinary prose and
    /// standard-library names must never be flagged.
    #[test]
    fn prose_and_standard_types_are_left_alone() {
        let answer = "The session stores state in a `Mutex<SessionState>` and returns \
                      a `Result`. It is cancelled when the user asks.";
        let unknown = unknown_symbols(answer, &declared(&["SessionState"]));
        assert!(unknown.is_empty(), "{unknown:?}");
    }

    /// Quoted code blocks are verbatim evidence, not claims.
    #[test]
    fn fenced_code_is_not_scanned() {
        let answer = "Here is the code:\n```rust\nfn invented_helper() {}\n```\nThat is all.";
        assert!(unknown_symbols(answer, &declared(&[])).is_empty());
    }

    #[test]
    fn a_path_is_judged_on_its_last_segment() {
        let answer = "Call `SessionRuntime::handle_request` to proceed.";
        let unknown = unknown_symbols(answer, &declared(&["SessionRuntime"]));
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].name, "handle_request");
    }

    #[test]
    fn a_lowercase_english_word_is_not_an_identifier() {
        assert!(!looks_like_symbol("cancellation"));
        assert!(!looks_like_symbol("streaming"));
        assert!(looks_like_symbol("start_session"));
        assert!(looks_like_symbol("SessionRuntime"));
    }

    #[test]
    fn the_notice_names_every_flagged_symbol() {
        let unknown = vec![UnknownSymbol {
            name: "start_session".into(),
            shape: Shape::Call,
        }];
        let notice = unknown_symbol_notice(&unknown).unwrap();
        assert!(notice.contains("start_session"));
        assert!(notice.contains("Not found in this repository"));
    }

    #[test]
    fn nothing_flagged_means_no_notice() {
        assert!(unknown_symbol_notice(&[]).is_none());
    }

    #[test]
    fn flagging_is_deterministic() {
        let answer = "`zeta_helper()` and `alpha_helper()` do the work.";
        let first = unknown_symbols(answer, &declared(&[]));
        let second = unknown_symbols(answer, &declared(&[]));
        assert_eq!(first, second);
        assert_eq!(first[0].name, "alpha_helper");
    }
}
