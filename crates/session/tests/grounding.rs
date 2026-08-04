//! Regression tests for the grounding failure reported from a live
//! session:
//!
//! ```text
//! /use assistant
//! Explain the session architecture.
//! Now explain how cancellation works.
//! ```
//!
//! Orbit searched, concluded the repository contained nothing about
//! sessions or cancellation, and produced a generic explanation with an
//! unrelated Tokio example instead.
//!
//! Every test here uses a **mock provider that never calls a tool**, so
//! whatever reaches the model must have come from deterministic
//! retrieval alone. If retrieval regresses, these fail — a model that
//! happens to ask for the right file cannot paper over it.

mod support;

use std::sync::Arc;

use orbit_core::{CollectingSink, EventPayload, FinishReason, Message, ModelResponse};
use orbit_providers::MockProvider;
use orbit_session::{ConfirmationMode, SessionRuntime};
use support::write_project;

/// A repository shaped like the one the failure was reported against.
fn orbit_like_repository() -> (tempfile::TempDir, orbit_actions::ActionContext) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    write_project(
        &root,
        "assistant",
        &[
            (
                "docs/SESSIONS.md",
                "# Sessions\n\n\
                 A session is a stateful, multi-turn conversation with Orbit.\n\n\
                 ## What a session remembers\n\n\
                 Session state lives in process memory: the conversation, the active\n\
                 projects, action records, and collected sources.\n\n\
                 ## Cancellation\n\n\
                 Cancelling a turn stops further model generation and further tool\n\
                 calls, releases any pending permission request, and leaves the\n\
                 session usable for the next message. Completed work is preserved.\n",
            ),
            (
                "docs/EVENTS.md",
                "# Agent Event Stream\n\n\
                 Everything a session does is reported as a structured event.\n\n\
                 ## Ordering guarantees\n\n\
                 A cancelled turn ends with execution_cancelled and no turn_completed.\n",
            ),
            (
                "docs/APP_PROTOCOL.md",
                "# Application protocol\n\n\
                 Sessions are driven over stdin/stdout as JSON Lines.\n\n\
                 ## execution.cancel\n\n\
                 Cancels the turn currently running in a session.\n",
            ),
            (
                "crates/session/src/runtime.rs",
                "//! The session runtime.\n\
                 pub struct SessionRuntime {\n    state: SessionState,\n}\n\n\
                 impl SessionRuntime {\n    \
                 pub fn cancel_current_turn(&self) -> bool { true }\n}\n",
            ),
            (
                "crates/core/src/event.rs",
                "//! Agent event model.\n\
                 pub struct CancellationToken;\n\n\
                 impl CancellationToken {\n    pub fn cancel(&self) {}\n}\n",
            ),
            (
                // Genuinely unrelated: it must contain none of the terms
                // under test, or matching it would be correct behavior.
                "docs/UNRELATED.md",
                "# Colour palette\n\nSwatches, hex codes, and print profiles.\n",
            ),
        ],
    );

    let config_path = root.join(".orbit/project.yaml");
    let config = orbit_project::ProjectConfig::load(&config_path).unwrap();
    (
        tmp,
        orbit_actions::ActionContext {
            root,
            config_path,
            config,
        },
    )
}

/// A provider that answers immediately and never requests a tool, and
/// which records everything it was sent.
fn never_calls_tools(answers: &[&str]) -> Arc<MockProvider> {
    Arc::new(MockProvider::new(
        answers
            .iter()
            .map(|a| ModelResponse {
                message: Message::assistant(*a),
                finish_reason: FinishReason::Stop,
            })
            .collect(),
    ))
}

/// Everything the provider was shown, across all requests.
fn text_sent_to_model(provider: &MockProvider) -> String {
    provider
        .recorded_requests()
        .iter()
        .flat_map(|r| r.messages.iter())
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn source_paths(sink: &CollectingSink) -> Vec<String> {
    sink.events()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SourceFound { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

async fn session(
    provider: Arc<MockProvider>,
    sink: Arc<CollectingSink>,
) -> (tempfile::TempDir, SessionRuntime) {
    let (tmp, ctx) = orbit_like_repository();
    let runtime =
        SessionRuntime::single_project(ctx, provider, sink, ConfirmationMode::AutoDeny, false)
            .await
            .unwrap();
    (tmp, runtime)
}

// --- the reported failure ------------------------------------------------

#[tokio::test]
async fn explain_the_session_architecture_is_grounded_in_the_repository() {
    let provider = never_calls_tools(&["(answer)"]);
    let sink = Arc::new(CollectingSink::new());
    let (_tmp, runtime) = session(provider.clone(), sink.clone()).await;

    let outcome = runtime
        .send_message("Explain the session architecture.")
        .await
        .unwrap();

    assert!(
        !outcome.sources.is_empty(),
        "the turn must be grounded; this is the exact reported failure"
    );

    let paths = source_paths(&sink);
    assert!(
        paths.iter().any(|p| p.contains("SESSIONS.md")),
        "session documentation must be found: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("runtime.rs")),
        "session implementation must be found: {paths:?}"
    );

    // The content itself must have reached the model, not just a citation.
    let sent = text_sent_to_model(&provider);
    assert!(
        sent.contains("stateful, multi-turn conversation"),
        "documentation content did not reach the model"
    );
    assert!(
        sent.contains("SessionRuntime"),
        "implementation content did not reach the model"
    );
}

/// The second turn of the reported conversation. "cancellation" alone is
/// not a self-contained subject; it must resolve against the session
/// topic established by the first turn.
#[tokio::test]
async fn a_follow_up_about_cancellation_resolves_against_the_session_topic() {
    let provider = never_calls_tools(&["(first)", "(second)"]);
    let sink = Arc::new(CollectingSink::new());
    let (_tmp, runtime) = session(provider.clone(), sink.clone()).await;

    runtime
        .send_message("Explain the session architecture.")
        .await
        .unwrap();
    sink.clear();

    let outcome = runtime
        .send_message("Now explain how cancellation works.")
        .await
        .unwrap();

    assert!(
        !outcome.sources.is_empty(),
        "the follow-up must be grounded"
    );

    let paths = source_paths(&sink);
    assert!(
        paths
            .iter()
            .any(|p| p.contains("SESSIONS.md") || p.contains("runtime.rs")),
        "cancellation must be resolved in the session context, not generically: {paths:?}"
    );

    // Cancellation content specifically must have reached the model.
    let sent = text_sent_to_model(&provider);
    assert!(
        sent.contains("stops further model generation") || sent.contains("cancel_current_turn"),
        "cancellation evidence did not reach the model"
    );
    assert!(
        !paths.iter().any(|p| p.contains("UNRELATED")),
        "unrelated files must not be retrieved: {paths:?}"
    );
}

/// Sources must come only from real retrieval, so a grounded answer can
/// always be traced back to a file that was actually read.
#[tokio::test]
async fn every_source_is_a_real_repository_path() {
    let provider = never_calls_tools(&["See /etc/passwd and imaginary.md for details."]);
    let sink = Arc::new(CollectingSink::new());
    let (tmp, runtime) = session(provider, sink.clone()).await;

    runtime
        .send_message("Explain the session architecture.")
        .await
        .unwrap();

    let root = tmp.path().canonicalize().unwrap();
    for path in source_paths(&sink) {
        assert!(
            root.join(&path).exists(),
            "`{path}` is not a real file in the repository"
        );
    }
}

// --- grounding policy ----------------------------------------------------

/// When the repository genuinely has nothing, the model must be told not
/// to answer as though its general knowledge described this project.
#[tokio::test]
async fn a_question_with_no_evidence_gets_an_explicit_grounding_instruction() {
    let provider = never_calls_tools(&["(answer)"]);
    let sink = Arc::new(CollectingSink::new());
    let (_tmp, runtime) = session(provider.clone(), sink).await;

    runtime
        .send_message("Explain the hydraulic landing gear telemetry calibration.")
        .await
        .unwrap();

    let sent = text_sent_to_model(&provider);
    assert!(
        sent.contains("Do not answer from general knowledge as if it describes this project"),
        "a weakly-grounded turn must carry the grounding instruction"
    );
}

/// ...and a well-grounded question must not, or the model would hedge on
/// answers it can actually support.
#[tokio::test]
async fn a_well_grounded_question_carries_no_grounding_warning() {
    let provider = never_calls_tools(&["(answer)"]);
    let sink = Arc::new(CollectingSink::new());
    let (_tmp, runtime) = session(provider.clone(), sink).await;

    runtime
        .send_message("Explain the session architecture.")
        .await
        .unwrap();

    let sent = text_sent_to_model(&provider);
    assert!(
        !sent.contains("Do not answer from general knowledge"),
        "a well-grounded turn must not be told its evidence is weak"
    );
}

// --- the other conversations named in the report -------------------------

#[tokio::test]
async fn find_the_watchdog_implementation_then_ask_about_the_requirement() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_project(
        &root,
        "obc",
        &[
            (
                "src/watchdog.rs",
                "//! Watchdog implementation.\npub fn feed_watchdog() {}\n",
            ),
            (
                "docs/REQUIREMENTS.md",
                "# Requirements\n\n\
                 REQ-12: the watchdog requirement is that it resets the system within\n\
                 two seconds of a stalled main loop.\n",
            ),
        ],
    );
    let config_path = root.join(".orbit/project.yaml");
    let config = orbit_project::ProjectConfig::load(&config_path).unwrap();
    let ctx = orbit_actions::ActionContext {
        root,
        config_path,
        config,
    };

    let provider = never_calls_tools(&["(first)", "(second)"]);
    let sink = Arc::new(CollectingSink::new());
    let runtime = SessionRuntime::single_project(
        ctx,
        provider.clone(),
        sink.clone(),
        ConfirmationMode::AutoDeny,
        false,
    )
    .await
    .unwrap();

    runtime
        .send_message("Find the watchdog implementation.")
        .await
        .unwrap();
    sink.clear();
    runtime
        .send_message("Does it satisfy the requirement?")
        .await
        .unwrap();

    let paths = source_paths(&sink);
    assert!(
        paths.iter().any(|p| p.contains("REQUIREMENTS.md")),
        "the follow-up must connect implementation to requirement: {paths:?}"
    );
    assert!(
        text_sent_to_model(&provider).contains("REQ-12"),
        "the requirement text must reach the model"
    );
}

/// Portuguese filler must not defeat retrieval.
///
/// Matching is lexical, so a Portuguese question finds English content
/// through the technical terms the two share — which is how engineers
/// actually write ("explique como funciona o SessionRuntime"). Translating
/// `cancelamento` into `cancellation` would need a bilingual lexicon and
/// is not implemented; see the limitations section of docs/SEARCH.md.
#[tokio::test]
async fn a_portuguese_question_is_grounded_through_shared_technical_terms() {
    let provider = never_calls_tools(&["(resposta)"]);
    let sink = Arc::new(CollectingSink::new());
    let (_tmp, runtime) = session(provider.clone(), sink.clone()).await;

    let outcome = runtime
        .send_message("Agora explique como funciona o SessionRuntime")
        .await
        .unwrap();

    assert!(
        !outcome.sources.is_empty(),
        "Portuguese filler must not defeat retrieval"
    );
    let paths = source_paths(&sink);
    assert!(
        paths
            .iter()
            .any(|p| p.contains("SESSIONS.md") || p.contains("runtime.rs")),
        "{paths:?}"
    );
    // The filler words themselves must not have driven the search.
    let terms = orbit_project::analyze("Agora explique como funciona o SessionRuntime").terms;
    for filler in ["agora", "explique", "como", "funciona"] {
        assert!(!terms.contains(&filler.to_string()), "{terms:?}");
    }
}
