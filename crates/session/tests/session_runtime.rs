//! Session lifecycle, multi-turn state, project switching, event ordering,
//! streaming, permissions, and cancellation. Uses mock providers and temp
//! projects/workspaces; nothing here requires Ollama.

mod support;

use std::sync::Arc;

use orbit_core::{
    CollectingSink, EventPayload, PermissionDecision, PermissionRequestId, SessionMode,
};
use orbit_providers::MockProvider;
use orbit_session::{ConfirmationMode, SessionRuntime};
use support::*;

fn sink() -> Arc<CollectingSink> {
    Arc::new(CollectingSink::new())
}

async fn single_project_session(
    responses: Vec<orbit_core::ModelResponse>,
    sink: Arc<CollectingSink>,
    mode: ConfirmationMode,
) -> (tempfile::TempDir, SessionRuntime) {
    let (tmp, ctx) = single_project_fixture();
    let runtime = SessionRuntime::single_project(
        ctx,
        Arc::new(MockProvider::new(responses)),
        sink,
        mode,
        false,
    )
    .await
    .unwrap();
    (tmp, runtime)
}

// --- lifecycle -----------------------------------------------------------

#[tokio::test]
async fn a_session_starts_and_ends_with_matching_events() {
    let sink = sink();
    let (_tmp, runtime) =
        single_project_session(vec![], sink.clone(), ConfirmationMode::AutoDeny).await;
    runtime.end("client requested").await;

    let names = sink.type_names();
    assert_eq!(names.first(), Some(&"session_started"));
    assert_eq!(names.last(), Some(&"session_ended"));

    match &sink.events()[0].payload {
        EventPayload::SessionStarted {
            mode,
            projects,
            protocol_version,
            ..
        } => {
            assert_eq!(*mode, SessionMode::SingleProject);
            assert_eq!(projects, &vec!["obc".to_string()]);
            assert_eq!(*protocol_version, orbit_core::EVENT_PROTOCOL_VERSION);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn every_session_gets_a_unique_id() {
    let (_a, one) = single_project_session(vec![], sink(), ConfirmationMode::AutoDeny).await;
    let (_b, two) = single_project_session(vec![], sink(), ConfirmationMode::AutoDeny).await;
    assert_ne!(one.id(), two.id());
}

#[tokio::test]
async fn conversation_context_is_preserved_across_turns() {
    let sink = sink();
    let (_tmp, runtime) = single_project_session(
        vec![
            answer("The watchdog resets the system."),
            answer("It fires after a brownout."),
        ],
        sink.clone(),
        ConfirmationMode::AutoDeny,
    )
    .await;

    runtime
        .send_message("what does the watchdog do?")
        .await
        .unwrap();
    let after_first = runtime.status().await.message_count;
    let second = runtime.send_message("when does it fire?").await.unwrap();

    assert_eq!(second.answer, "It fires after a brownout.");
    let status = runtime.status().await;
    assert_eq!(status.turns, 2);
    assert!(
        status.message_count > after_first,
        "the second turn must build on the first, not replace it"
    );
}

#[tokio::test]
async fn clear_forgets_the_conversation_but_keeps_the_session() {
    let sink = sink();
    let (_tmp, runtime) =
        single_project_session(vec![answer("hi")], sink.clone(), ConfirmationMode::AutoDeny).await;
    runtime.send_message("hello").await.unwrap();
    let id_before = runtime.id().clone();

    runtime.clear().await;

    let status = runtime.status().await;
    assert_eq!(status.message_count, 0);
    assert_eq!(status.source_count, 0);
    assert_eq!(
        runtime.id(),
        &id_before,
        "the session itself survives /clear"
    );
}

// --- event ordering ------------------------------------------------------

#[tokio::test]
async fn a_normal_turn_follows_the_documented_event_order() {
    let sink = sink();
    let (_tmp, runtime) = single_project_session(
        vec![
            tool_call("project.search", serde_json::json!({"query": "watchdog"})),
            answer("The watchdog resets the system."),
        ],
        sink.clone(),
        ConfirmationMode::AutoDeny,
    )
    .await;
    runtime
        .send_message("what does the watchdog do?")
        .await
        .unwrap();

    let names = sink.type_names();
    let index = |needle: &str| names.iter().position(|n| *n == needle).unwrap();

    assert!(index("session_started") < index("user_message_received"));
    assert!(index("user_message_received") < index("action_requested"));

    // The per-execution guarantee, checked for every execution in the
    // turn rather than only the first occurrence of each name:
    // requested → started → sources → completed, sharing one execution
    // id. Deterministic retrieval runs its own actions on every turn, so
    // a global "first action_started before first source_found" check
    // would compare events from unrelated executions.
    let mut by_execution: std::collections::BTreeMap<String, Vec<&str>> = Default::default();
    for event in sink.events() {
        if let Some(id) = &event.execution_id {
            by_execution
                .entry(id.0.clone())
                .or_default()
                .push(event.type_name());
        }
    }
    assert!(!by_execution.is_empty());
    for (id, sequence) in by_execution {
        let position = |needle: &str| sequence.iter().position(|n| *n == needle);
        let requested = position("action_requested")
            .unwrap_or_else(|| panic!("{id} has no action_requested: {sequence:?}"));
        if let Some(started) = position("action_started") {
            assert!(requested < started, "{id}: {sequence:?}");
            let finished = position("action_completed")
                .or_else(|| position("action_failed"))
                .unwrap_or_else(|| panic!("{id} never finished: {sequence:?}"));
            assert!(started < finished, "{id}: {sequence:?}");
            for (at, name) in sequence.iter().enumerate() {
                if *name == "source_found" {
                    assert!(
                        started < at && at < finished,
                        "{id}: a source must fall inside its own action: {sequence:?}"
                    );
                }
            }
        }
    }
    assert!(
        names
            .iter()
            .rposition(|n| *n == "model_response_completed")
            .unwrap()
            < index("turn_completed"),
        "the final model response must precede turn_completed: {names:?}"
    );
    assert_eq!(names.last(), Some(&"turn_completed"));
}

#[tokio::test]
async fn action_completed_always_follows_its_action_started() {
    let sink = sink();
    let (_tmp, runtime) = single_project_session(
        vec![
            tool_call("project.search", serde_json::json!({"query": "brownout"})),
            answer("done"),
        ],
        sink.clone(),
        ConfirmationMode::AutoDeny,
    )
    .await;
    runtime.send_message("brownout?").await.unwrap();

    let names = sink.type_names();
    let started = names.iter().position(|n| *n == "action_started").unwrap();
    let completed = names.iter().position(|n| *n == "action_completed").unwrap();
    assert!(started < completed);
}

#[tokio::test]
async fn a_failing_action_reports_action_failed_and_the_turn_still_completes() {
    let sink = sink();
    let (_tmp, runtime) = single_project_session(
        vec![
            tool_call(
                "project.read_file",
                serde_json::json!({"path": "does-not-exist.md"}),
            ),
            answer("I could not read that file."),
        ],
        sink.clone(),
        ConfirmationMode::AutoDeny,
    )
    .await;
    let outcome = runtime.send_message("read the missing file").await.unwrap();

    let read_events = events_for_action(&sink, "project.read_file");
    assert!(
        read_events.contains(&"action_failed"),
        "the missing file must be reported as a failure: {read_events:?}"
    );
    assert!(!outcome.cancelled);
    assert_eq!(sink.type_names().last(), Some(&"turn_completed"));
}

// --- streaming -----------------------------------------------------------

#[tokio::test]
async fn streamed_deltas_reconstruct_the_answer_exactly() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = SessionRuntime::single_project(
        ctx,
        Arc::new(MockProvider::streaming(vec![answer(
            "STM32 was selected for low power draw.",
        )])),
        sink.clone(),
        ConfirmationMode::AutoDeny,
        true,
    )
    .await
    .unwrap();

    let outcome = runtime.send_message("why STM32?").await.unwrap();
    drop(tmp);

    let deltas: String = sink
        .events()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ResponseDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, outcome.answer);
}

// --- permissions ---------------------------------------------------------

#[tokio::test]
async fn an_ask_permission_pauses_until_it_is_approved() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = Arc::new(
        SessionRuntime::single_project(
            ctx,
            Arc::new(MockProvider::new(vec![
                tool_call(
                    "command.run_configured",
                    serde_json::json!({"name": "test"}),
                ),
                answer("The tests ran."),
            ])),
            sink.clone(),
            ConfirmationMode::External,
            false,
        )
        .await
        .unwrap(),
    );

    let turn = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.send_message("run the tests").await })
    };

    // Wait for the request, proving the action actually paused.
    let request_id = wait_for_permission(&sink).await;
    assert!(
        !events_for_action(&sink, "command.run_configured").contains(&"action_started"),
        "the gated action must not have started while the request is pending"
    );

    runtime.resolve_permission(&request_id, PermissionDecision::AllowOnce);
    let outcome = turn.await.unwrap().unwrap();
    drop(tmp);

    assert_eq!(outcome.answer, "The tests ran.");
    let names = sink.type_names();
    assert!(names.contains(&"permission_required"));
    assert!(names.contains(&"permission_resolved"));
    assert!(names.contains(&"action_started"));
    assert!(names.contains(&"action_completed"));
}

#[tokio::test]
async fn a_denied_permission_never_starts_the_action() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = Arc::new(
        SessionRuntime::single_project(
            ctx,
            Arc::new(MockProvider::new(vec![
                tool_call(
                    "command.run_configured",
                    serde_json::json!({"name": "test"}),
                ),
                answer("That command was not allowed."),
            ])),
            sink.clone(),
            ConfirmationMode::External,
            false,
        )
        .await
        .unwrap(),
    );

    let turn = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.send_message("run the tests").await })
    };

    let request_id = wait_for_permission(&sink).await;
    runtime.resolve_permission(&request_id, PermissionDecision::DenyOnce);
    turn.await.unwrap().unwrap();
    drop(tmp);

    assert!(sink.type_names().contains(&"permission_resolved"));
    let gated = events_for_action(&sink, "command.run_configured");
    assert!(
        !gated.contains(&"action_started"),
        "a denied action must never start: {gated:?}"
    );
    assert!(gated.contains(&"action_failed"), "{gated:?}");
}

/// The permission event must carry a safe, redacted argument summary.
#[tokio::test]
async fn a_permission_request_carries_a_safe_argument_summary() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = Arc::new(
        SessionRuntime::single_project(
            ctx,
            Arc::new(MockProvider::new(vec![
                tool_call(
                    "command.run_configured",
                    serde_json::json!({"name": "test"}),
                ),
                answer("ok"),
            ])),
            sink.clone(),
            ConfirmationMode::External,
            false,
        )
        .await
        .unwrap(),
    );
    let turn = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.send_message("run the tests").await })
    };
    let request_id = wait_for_permission(&sink).await;

    let event = sink
        .events()
        .into_iter()
        .find(|e| matches!(e.payload, EventPayload::PermissionRequired { .. }))
        .unwrap();
    match event.payload {
        EventPayload::PermissionRequired {
            action, arguments, ..
        } => {
            assert_eq!(action, "command.run_configured");
            assert_eq!(arguments, "name=test");
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(event.project.as_deref(), Some("obc"));

    runtime.resolve_permission(&request_id, PermissionDecision::DenyOnce);
    turn.await.unwrap().unwrap();
    drop(tmp);
}

// --- cancellation --------------------------------------------------------

#[tokio::test]
async fn cancelling_a_pending_permission_ends_the_turn_without_running_it() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = Arc::new(
        SessionRuntime::single_project(
            ctx,
            Arc::new(MockProvider::new(vec![
                tool_call(
                    "command.run_configured",
                    serde_json::json!({"name": "test"}),
                ),
                answer("unused"),
            ])),
            sink.clone(),
            ConfirmationMode::External,
            false,
        )
        .await
        .unwrap(),
    );

    let turn = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.send_message("run the tests").await })
    };
    wait_for_permission(&sink).await;

    assert!(runtime.cancel_current_turn());
    turn.await.unwrap().unwrap();
    drop(tmp);

    let names = sink.type_names();
    assert!(
        !events_for_action(&sink, "command.run_configured").contains(&"action_started"),
        "the gated action must never have started"
    );
    assert!(names.contains(&"execution_cancelled"));
    assert!(
        !names.contains(&"turn_completed"),
        "a cancelled turn must not also report completion: {names:?}"
    );
}

#[tokio::test]
async fn cancelling_when_nothing_is_running_reports_that_plainly() {
    let (_tmp, runtime) = single_project_session(vec![], sink(), ConfirmationMode::AutoDeny).await;
    assert!(!runtime.cancel_current_turn());
}

#[tokio::test]
async fn a_session_stays_usable_after_a_cancelled_turn() {
    let (tmp, ctx) = single_project_fixture();
    let sink = sink();
    let runtime = Arc::new(
        SessionRuntime::single_project(
            ctx,
            // The cancelled turn consumes only the tool-call response: it
            // stops at the top of the next iteration rather than asking
            // the model again, so the following answer belongs to turn 2.
            Arc::new(MockProvider::new(vec![
                tool_call(
                    "command.run_configured",
                    serde_json::json!({"name": "test"}),
                ),
                answer("Second turn worked."),
            ])),
            sink.clone(),
            ConfirmationMode::External,
            false,
        )
        .await
        .unwrap(),
    );

    let turn = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.send_message("run the tests").await })
    };
    wait_for_permission(&sink).await;
    runtime.cancel_current_turn();
    let cancelled = turn.await.unwrap().unwrap();
    assert!(cancelled.cancelled);

    // The next turn must work normally.
    let next = runtime.send_message("anything else?").await.unwrap();
    drop(tmp);
    assert!(!next.cancelled);
    assert_eq!(next.answer, "Second turn worked.");
    assert_eq!(runtime.status().await.turns, 2);
}

/// Event type names concerning one specific action, in order.
///
/// Deterministic retrieval runs its own actions on every turn, so
/// assertions about "the action" under test must name it — otherwise they
/// accidentally match retrieval's `project.information` or
/// `project.search`.
fn events_for_action(sink: &CollectingSink, action_name: &str) -> Vec<&'static str> {
    sink.events()
        .iter()
        .filter(|e| match &e.payload {
            EventPayload::ActionRequested { action, .. }
            | EventPayload::ActionStarted { action }
            | EventPayload::ActionCompleted { action, .. }
            | EventPayload::ActionFailed { action, .. } => action == action_name,
            EventPayload::PermissionRequired { action, .. } => action == action_name,
            _ => false,
        })
        .map(|e| e.type_name())
        .collect()
}

async fn wait_for_permission(sink: &Arc<CollectingSink>) -> PermissionRequestId {
    for _ in 0..2_000 {
        if let Some(id) = sink.events().iter().find_map(|e| match &e.payload {
            EventPayload::PermissionRequired { request_id, .. } => Some(request_id.clone()),
            _ => None,
        }) {
            return id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("no permission request was ever emitted");
}
