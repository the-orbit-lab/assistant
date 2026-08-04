//! Spoken responses.
//!
//! macOS ships a speech synthesizer reachable through `/usr/bin/say`.
//! This module drives it as a **fixed argument list** — the text arrives
//! on the process's stdin, never interpolated into a command string — so
//! there is no shell to escape and nothing a model's output could do
//! beyond being read aloud.
//!
//! Speech-to-text is deliberately absent. A microphone button that
//! silently routed audio somewhere would undercut the property Orbit is
//! built for, so the UI reports that no provider is configured instead.
//! See `docs/VOICE.md`.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;

/// The running utterance, if any. One at a time: speech is a queue in
/// the UI, and two synthesizers talking over each other is never what
/// anyone wants.
#[derive(Default)]
pub struct SpeechState {
    current: Mutex<Option<Child>>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpeechError {
    /// No synthesizer on this platform.
    Unavailable { detail: String },
    /// The synthesizer could not be started or written to.
    Failed { detail: String },
}

impl std::fmt::Display for SpeechError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { detail } => write!(f, "speech is unavailable: {detail}"),
            Self::Failed { detail } => write!(f, "speech failed: {detail}"),
        }
    }
}

/// Is a synthesizer present?
pub fn available() -> bool {
    cfg!(target_os = "macos") && std::path::Path::new("/usr/bin/say").is_file()
}

/// Text that is safe to hand a synthesizer.
///
/// Speech should carry the answer's prose and nothing else. Code blocks,
/// tables, and link targets are unpleasant read aloud and are usually
/// the parts a listener is reading rather than hearing, so they are
/// removed here rather than in the UI — the same rule then applies to
/// every future provider.
pub fn speakable(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Table rows and horizontal rules read as noise.
        if trimmed.starts_with('|') || trimmed.starts_with("---") || trimmed.starts_with("***") {
            continue;
        }

        let mut text = trimmed.trim_start_matches('#').trim_start().to_string();
        text = text
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .to_string();
        text = text.trim_start_matches("> ").to_string();
        // Inline code, emphasis, and link syntax are markup, not words.
        text = text.replace('`', "");
        text = text.replace("**", "").replace('*', "").replace('_', " ");
        text = strip_link_targets(&text);

        if !text.trim().is_empty() {
            out.push_str(text.trim());
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// `[label](url)` becomes `label`: the target is for clicking, not
/// listening.
fn strip_link_targets(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](") else {
            break;
        };
        let Some(end) = rest[open + close..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..open + close]);
        rest = &rest[open + close + end + 1..];
    }
    out.push_str(rest);
    out
}

impl SpeechState {
    /// Speak one utterance, replacing anything already speaking.
    pub fn speak(&self, text: &str) -> Result<(), SpeechError> {
        if !available() {
            return Err(SpeechError::Unavailable {
                detail: "no /usr/bin/say on this system".to_string(),
            });
        }
        let spoken = speakable(text);
        if spoken.trim().is_empty() {
            return Ok(());
        }

        self.stop();

        // `-f -` reads the utterance from stdin. The text never becomes
        // part of the argument list, so nothing in it is interpreted.
        let mut child = Command::new("/usr/bin/say")
            .arg("-f")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| SpeechError::Failed {
                detail: e.to_string(),
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(spoken.as_bytes())
                .map_err(|e| SpeechError::Failed {
                    detail: e.to_string(),
                })?;
        }

        *self.current.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Stop immediately. Called on Stop voice, on cancellation, and
    /// before a new recording starts.
    pub fn stop(&self) {
        if let Some(mut child) = self.current.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Has the current utterance finished?
    pub fn speaking(&self) -> bool {
        let mut guard = self.current.lock().unwrap();
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    *guard = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            },
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fences_are_never_spoken() {
        let text = "Here is how:\n```rust\nfn main() { println!(\"hi\"); }\n```\nThat is all.";
        let spoken = speakable(text);
        assert!(spoken.contains("Here is how"));
        assert!(spoken.contains("That is all"));
        assert!(!spoken.contains("println"), "{spoken}");
        assert!(!spoken.contains("fn main"), "{spoken}");
    }

    #[test]
    fn headings_lose_their_markers_but_keep_their_words() {
        assert_eq!(speakable("## Session state"), "Session state");
    }

    #[test]
    fn emphasis_and_inline_code_become_plain_words() {
        let spoken = speakable("The **state** field is a `Mutex`.");
        assert_eq!(spoken, "The state field is a Mutex.");
    }

    #[test]
    fn list_markers_are_dropped() {
        assert_eq!(speakable("- first\n- second"), "first\nsecond");
    }

    #[test]
    fn a_link_is_read_as_its_label() {
        assert_eq!(
            speakable("See [the sessions doc](https://example.com/x) for more."),
            "See the sessions doc for more."
        );
    }

    #[test]
    fn tables_and_rules_are_skipped() {
        let spoken = speakable("Intro\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nOutro");
        assert_eq!(spoken, "Intro\nOutro");
    }

    #[test]
    fn blockquotes_are_spoken_without_their_marker() {
        assert_eq!(speakable("> quoted line"), "quoted line");
    }

    #[test]
    fn a_message_that_is_only_code_speaks_nothing() {
        assert_eq!(speakable("```\nfn x() {}\n```"), "");
    }

    #[test]
    fn stopping_when_silent_is_harmless() {
        SpeechState::default().stop();
        assert!(!SpeechState::default().speaking());
    }
}
