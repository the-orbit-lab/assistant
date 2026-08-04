//! Workspace-scoped sessions: project switching, deterministic routing,
//! and project-scoped source preservation.

mod support;

use std::sync::Arc;

use orbit_core::{CollectingSink, EventPayload, SessionMode};
use orbit_providers::MockProvider;
use orbit_session::{ConfirmationMode, SessionRuntime};
use support::*;

fn workspace_session(
    responses: Vec<orbit_core::ModelResponse>,
) -> (tempfile::TempDir, Arc<SessionRuntime>, Arc<CollectingSink>) {
    let (tmp, projects) = workspace_fixture();
    let sink = Arc::new(CollectingSink::new());
    let runtime = SessionRuntime::workspace(
        projects,
        Arc::new(MockProvider::new(responses)),
        sink.clone(),
        ConfirmationMode::AutoDeny,
        false,
    )
    .unwrap();
    (tmp, Arc::new(runtime), sink)
}

#[tokio::test]
async fn a_workspace_session_starts_with_no_active_project() {
    let (_tmp, runtime, sink) = workspace_session(vec![]);
    assert!(runtime.active_projects().await.is_empty());
    match &sink.events()[0].payload {
        EventPayload::SessionStarted {
            mode,
            workspace,
            projects,
            ..
        } => {
            assert_eq!(*mode, SessionMode::Workspace);
            assert_eq!(workspace.as_deref(), Some("Orbit Lab"));
            assert!(projects.is_empty(), "no repository is selected implicitly");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn switching_projects_emits_active_projects_changed() {
    let (_tmp, runtime, sink) = workspace_session(vec![]);

    let names = runtime
        .set_active_projects(&["obc".to_string()])
        .await
        .unwrap();
    assert_eq!(names, vec!["obc".to_string()]);

    let changed: Vec<_> = sink
        .events()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ActiveProjectsChanged { projects } => Some(projects),
            _ => None,
        })
        .collect();
    assert_eq!(changed, vec![vec!["obc".to_string()]]);
}

#[tokio::test]
async fn an_alias_resolves_to_its_registered_project() {
    let (_tmp, runtime, _sink) = workspace_session(vec![]);
    let names = runtime
        .set_active_projects(&["flight-computer".to_string()])
        .await
        .unwrap();
    assert_eq!(names, vec!["obc".to_string()]);
}

#[tokio::test]
async fn several_projects_can_be_active_at_once() {
    let (_tmp, runtime, _sink) = workspace_session(vec![]);
    let names = runtime
        .set_active_projects(&["docs".to_string(), "obc".to_string()])
        .await
        .unwrap();
    assert_eq!(names, vec!["docs".to_string(), "obc".to_string()]);
}

/// An invalid switch must leave the previous selection intact rather than
/// half-applying it.
#[tokio::test]
async fn an_unknown_project_is_rejected_and_changes_nothing() {
    let (_tmp, runtime, sink) = workspace_session(vec![]);
    runtime
        .set_active_projects(&["obc".to_string()])
        .await
        .unwrap();
    sink.clear();

    let err = runtime
        .set_active_projects(&["docs".to_string(), "nonexistent".to_string()])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nonexistent"), "{err}");

    assert_eq!(runtime.active_projects().await, vec!["obc".to_string()]);
    assert!(
        sink.type_names().is_empty(),
        "a rejected switch must not announce a change"
    );
}

#[tokio::test]
async fn an_unavailable_project_cannot_be_selected() {
    let (_tmp, runtime, _sink) = workspace_session(vec![]);
    let err = runtime
        .set_active_projects(&["mission-tools".to_string()])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mission-tools"), "{err}");
    assert!(runtime.active_projects().await.is_empty());
}

/// Deterministic routing: naming a project in the question scopes the turn
/// to it and preserves project-scoped sources.
#[tokio::test]
async fn naming_a_project_scopes_the_turn_and_keeps_project_scoped_sources() {
    let (_tmp, runtime, sink) =
        workspace_session(vec![answer("STM32 was chosen for low power draw.")]);

    // Phrased so the extracted query is the literal token `STM32`:
    // `orbit-project`'s search is substring-based, not tokenized, so a
    // multi-word query only matches a literal phrase (see
    // docs/WORKSPACES.md "Known limitations").
    let outcome = runtime.send_message("In docs, why STM32?").await.unwrap();

    assert!(outcome.active_projects.contains(&"docs".to_string()));
    assert!(!outcome.used_default_project);

    let source_events: Vec<_> = sink
        .events()
        .into_iter()
        .filter(|e| matches!(e.payload, EventPayload::SourceFound { .. }))
        .collect();
    assert!(!source_events.is_empty(), "the turn must be grounded");
    assert!(
        source_events
            .iter()
            .all(|e| e.project.as_deref() == Some("docs")),
        "sources must stay attributed to their own project"
    );

    // The session keeps them for the rest of the conversation.
    let kept = runtime.sources().await;
    assert!(
        kept.iter()
            .any(|s| s.path.to_string_lossy().starts_with("docs:"))
    );
}

#[tokio::test]
async fn retrieval_events_bracket_the_deterministic_step() {
    let (_tmp, runtime, sink) = workspace_session(vec![answer("ok")]);
    runtime
        .send_message("In obc, what is the watchdog?")
        .await
        .unwrap();

    let names = sink.type_names();
    let started = names
        .iter()
        .position(|n| *n == "retrieval_started")
        .unwrap();
    let completed = names
        .iter()
        .position(|n| *n == "retrieval_completed")
        .unwrap();
    assert!(started < completed);
    assert!(
        names
            .iter()
            .position(|n| *n == "user_message_received")
            .unwrap()
            < started,
        "retrieval belongs to the turn it serves: {names:?}"
    );
}

/// An explicit selection wins over scanning the question text.
#[tokio::test]
async fn an_explicit_selection_is_used_even_when_the_question_names_nothing() {
    let (_tmp, runtime, sink) = workspace_session(vec![answer("ok")]);
    runtime
        .set_active_projects(&["obc".to_string()])
        .await
        .unwrap();

    let outcome = runtime
        .send_message("what changed recently?")
        .await
        .unwrap();
    assert_eq!(outcome.active_projects, vec!["obc".to_string()]);

    let scopes: Vec<_> = sink
        .events()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::RetrievalStarted { scope } => Some(scope),
            _ => None,
        })
        .collect();
    assert_eq!(scopes, vec![vec!["obc".to_string()]]);
}

/// A follow-up that names another project broadens the active set, matching
/// "Now include the docs project."
#[tokio::test]
async fn naming_another_project_in_a_follow_up_broadens_the_active_set() {
    let (_tmp, runtime, _sink) = workspace_session(vec![answer("first"), answer("second")]);
    runtime
        .set_active_projects(&["obc".to_string()])
        .await
        .unwrap();
    runtime.send_message("what is the watchdog?").await.unwrap();

    let outcome = runtime
        .send_message("compare that with docs")
        .await
        .unwrap();
    assert!(outcome.active_projects.contains(&"obc".to_string()));
    assert!(outcome.active_projects.contains(&"docs".to_string()));
}

/// Falling back to `defaults.project` must be visible and must not pin the
/// session to that project.
#[tokio::test]
async fn the_default_project_is_reported_and_does_not_become_sticky() {
    let (_tmp, runtime, _sink) = workspace_session(vec![answer("overview")]);
    let outcome = runtime.send_message("what is this?").await.unwrap();

    assert!(outcome.used_default_project);
    assert_eq!(outcome.active_projects, Vec::<String>::new());
    assert!(
        runtime.active_projects().await.is_empty(),
        "an overview question must not silently pin the session to a project"
    );
}

#[tokio::test]
async fn single_project_sessions_cannot_switch_projects() {
    let (_tmp, ctx) = single_project_fixture();
    let runtime = SessionRuntime::single_project(
        ctx,
        Arc::new(MockProvider::new(vec![])),
        Arc::new(CollectingSink::new()),
        ConfirmationMode::AutoDeny,
        false,
    )
    .await
    .unwrap();

    assert!(
        runtime
            .set_active_projects(&["docs".to_string()])
            .await
            .is_err()
    );
}

/// Regression: retrieved sources were once recorded twice — once when
/// retrieval returned them and again when the turn outcome (which already
/// merged them) was folded in — so `/sources` listed every source twice.
#[tokio::test]
async fn session_sources_are_recorded_once_per_source() {
    let (_tmp, runtime, _sink) = workspace_session(vec![answer("ok")]);
    let outcome = runtime.send_message("In docs, why STM32?").await.unwrap();
    assert!(!outcome.sources.is_empty(), "the turn must be grounded");

    let kept = runtime.sources().await;
    let mut unique = kept.clone();
    unique.dedup();
    assert_eq!(
        kept.len(),
        unique.len(),
        "each source must appear once in the session: {kept:?}"
    );
    assert_eq!(kept.len(), outcome.sources.len());
}

/// The multi-project conversation from the report: a follow-up that
/// refers back ("that") must keep the earlier subject rather than
/// retrieving on the word "compare".
#[tokio::test]
async fn a_referring_follow_up_keeps_the_previous_subject() {
    let (_tmp, runtime, sink) = workspace_session(vec![answer("first"), answer("second")]);

    runtime
        .send_message("Why was STM32 selected?")
        .await
        .unwrap();
    sink.clear();

    let outcome = runtime
        .send_message("Now compare that with docs")
        .await
        .unwrap();

    assert!(
        outcome.active_projects.contains(&"docs".to_string()),
        "{:?}",
        outcome.active_projects
    );
    let sources: Vec<String> = sink
        .events()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SourceFound { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !sources.is_empty(),
        "a referring follow-up must still be grounded: {sources:?}"
    );
    assert!(
        sources.iter().any(|p| p.contains("ADR-0004")),
        "the STM32 subject must survive the follow-up: {sources:?}"
    );
}
