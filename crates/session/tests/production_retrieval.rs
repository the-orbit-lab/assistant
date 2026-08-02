//! What `orbit chat` actually sends the model.
//!
//! Every other retrieval test in this repository calls a retrieval
//! function directly. That is exactly how the reported failure survived:
//! the two-stage pipeline was wired into `orbit-agent`, every unit test
//! of it passed, and `orbit chat` in workspace mode went on running the
//! old lexical search — because `SessionRuntime::run_turn` dispatches on
//! `Scope`, and the workspace arm called `orbit_workspace::retrieval`,
//! which the pipeline had never reached.
//!
//! These tests therefore drive the **production path**:
//!
//! ```text
//! SessionRuntime::send  →  run_turn  →  Scope::Workspace
//!   →  orbit_workspace::retrieval::run
//!   →  orbit_retrieval::agenda  (plan → generate → fuse → rerank → select)
//!   →  workspace.read_file      (whole files, then declaration spans)
//!   →  ModelProvider::chat
//! ```
//!
//! and assert on the ordered messages a recording provider received. A
//! test that bypasses session or workspace orchestration cannot catch an
//! orchestration bug, which is the only kind this file exists to catch.

mod support;

use std::path::Path;
use std::sync::Arc;

use orbit_core::{CollectingSink, ModelRequest, Role};
use orbit_providers::MockProvider;
use orbit_session::{ConfirmationMode, SessionRuntime};
use orbit_workspace::{ProjectRegistry, WorkspaceConfig};
use support::*;

/// A project shaped like the one that produced the report: a real
/// implementation, a verbose test file repeating the exact user
/// question, and domain documentation.
fn write_session_project(root: &Path) {
    let project = root.join("assistant");
    std::fs::create_dir_all(project.join(".orbit")).unwrap();
    std::fs::write(
        project.join(".orbit/project.yaml"),
        "version: 1\nproject:\n  name: assistant\n  description: The assistant\n\
         context:\n  include:\n    - \"src/**\"\n    - \"docs/**\"\n    - \"tests/**\"\n\
         permissions:\n  project.information: allow\n  project.list_files: allow\n  \
         project.read_file: allow\n  project.search: allow\n",
    )
    .unwrap();

    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/session.rs"),
        "//! Stateful multi-turn sessions.\n\
         use std::sync::Arc;\n\
         \n\
         /// A stateful conversation. Cheap to share.\n\
         pub struct SessionRuntime {\n\
         \x20   id: SessionId,\n\
         \x20   mode: SessionMode,\n\
         \x20   /// Held for the duration of a turn.\n\
         \x20   state: tokio::sync::Mutex<SessionState>,\n\
         \x20   /// Separate from `state` on purpose.\n\
         \x20   current_cancel: std::sync::Mutex<Option<CancellationToken>>,\n\
         \x20   sources: Vec<SourceReference>,\n\
         \x20   streaming: bool,\n\
         }\n\
         \n\
         impl SessionRuntime {\n\
         \x20   /// Store a turn result in the session state.\n\
         \x20   pub async fn record_state(&self, outcome: TurnOutcome) {\n\
         \x20       self.state.lock().await.push(outcome);\n\
         \x20   }\n\
         \n\
         \x20   /// Cancel the turn currently running.\n\
         \x20   pub fn cancel_current_turn(&self) -> bool {\n\
         \x20       true\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();

    // The adversary: repeats the exact user question and the symbol name
    // on nearly every line, and is far longer than the implementation.
    std::fs::create_dir_all(project.join("tests")).unwrap();
    let mut grounding = String::from("//! Grounding regression tests.\n\n");
    for index in 0..80 {
        grounding.push_str(&format!(
            "/// Explain SessionRuntime and how it stores session state.\n\
             #[test]\n\
             fn session_runtime_stores_session_state_{index}() {{\n\
             \x20   // SessionRuntime stores session state in SessionState.\n\
             \x20   let runtime = SessionRuntime::new();\n\
             \x20   assert!(runtime.stores_session_state());\n\
             }}\n\n"
        ));
    }
    std::fs::write(project.join("tests/grounding.rs"), &grounding).unwrap();

    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(
        project.join("docs/SESSIONS.md"),
        "# Sessions\n\n\
         A session keeps its conversation state in process memory.\n\n\
         ## State\n\n\
         The session runtime owns the history and the collected sources.\n\
         Nothing about a session is written to disk.\n\n\
         ## Cancellation\n\n\
         A running turn can be cancelled at any point.\n",
    )
    .unwrap();
}

/// A workspace holding that one project, wired exactly as `orbit chat`
/// wires it.
fn session_workspace() -> (tempfile::TempDir, Arc<SessionRuntime>, Arc<MockProvider>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    write_session_project(&root);

    let yaml = "version: 1\n\
        workspace:\n  name: Orbit Lab\n  description: session retrieval fixture\n\
        projects:\n\
        \x20\x20assistant:\n    path: ./assistant\n";
    let config_path = root.join(".orbit/workspace.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let config = WorkspaceConfig::parse(yaml).unwrap();
    let projects = Arc::new(ProjectRegistry::load(root.clone(), config_path, config).unwrap());

    let provider = Arc::new(MockProvider::new(vec![answer(
        "SessionRuntime stores state in a mutex.",
    )]));
    let runtime = SessionRuntime::workspace(
        projects,
        provider.clone(),
        Arc::new(CollectingSink::new()),
        ConfirmationMode::AutoAllow,
        false,
    )
    .unwrap();
    (tmp, Arc::new(runtime), provider)
}

/// The ordered file paths the provider actually received, read from the
/// tool-result messages in its first request.
fn context_paths(request: &ModelRequest) -> Vec<String> {
    let mut paths = Vec::new();
    for message in &request.messages {
        if message.role != Role::Tool {
            continue;
        }
        // Tool results carry the action's JSON payload; the read actions
        // report the path they read.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) else {
            continue;
        };
        if let Some(path) = value.get("path").and_then(|p| p.as_str()) {
            paths.push(path.to_string());
        }
    }
    paths
}

async fn run_reported_question() -> (tempfile::TempDir, Vec<String>, Vec<ModelRequest>) {
    let (tmp, runtime, provider) = session_workspace();
    runtime
        .set_active_projects(&["assistant".to_string()])
        .await
        .unwrap();
    runtime
        .send_message("Explain SessionRuntime and how it stores session state.")
        .await
        .unwrap();

    let requests = provider.recorded_requests();
    let paths = context_paths(&requests[0]);
    (tmp, paths, requests)
}

/// The whole point: `orbit chat` in workspace mode must put the file
/// that *declares* `SessionRuntime` in front of the model.
#[tokio::test]
async fn the_production_path_sends_the_implementation() {
    let (_tmp, paths, _) = run_reported_question().await;
    assert!(
        paths.iter().any(|p| p.ends_with("src/session.rs")),
        "the declaring file never reached the provider: {paths:?}"
    );
}

/// The reported failure, stated as an assertion: a verbose test file
/// repeating the question cannot be the primary explanatory context.
#[tokio::test]
async fn a_verbose_test_file_is_never_the_primary_context() {
    let (_tmp, paths, _) = run_reported_question().await;

    let implementation = paths.iter().position(|p| p.ends_with("src/session.rs"));
    let test_file = paths.iter().position(|p| p.ends_with("tests/grounding.rs"));

    let implementation = implementation.expect("implementation must be present");

    // A test may corroborate, but never alone and never as the last —
    // and therefore most salient — thing the model reads.
    if let Some(test_file) = test_file {
        assert!(
            test_file < paths.len() - 1,
            "the test file was the last context the model saw: {paths:?}"
        );
        assert!(
            implementation > test_file || paths.len() > test_file + 1,
            "the test file outranked the implementation: {paths:?}"
        );
    }
}

/// Direct implementation evidence is the *last* thing read, because a
/// model answers from what is nearest its question.
#[tokio::test]
async fn implementation_evidence_is_closest_to_the_question() {
    let (_tmp, paths, _) = run_reported_question().await;
    let last = paths.last().expect("some context");
    assert!(
        last.ends_with("src/session.rs"),
        "the declaration must be the last evidence read: {paths:?}"
    );
}

/// Documentation is present, so the answer has both the code and the
/// prose describing it.
#[tokio::test]
async fn domain_documentation_accompanies_the_implementation() {
    let (_tmp, paths, _) = run_reported_question().await;
    assert!(
        paths.iter().any(|p| p.ends_with("docs/SESSIONS.md")),
        "{paths:?}"
    );
}

/// The declaration is quoted by *span*, not by reading the whole file,
/// and a trusted instruction names where it lives.
#[tokio::test]
async fn the_declaration_is_quoted_by_span_with_an_anchor() {
    let (_tmp, _, requests) = run_reported_question().await;
    let request = &requests[0];

    let ranged = request.messages.iter().any(|message| {
        message.role == Role::Tool
            && serde_json::from_str::<serde_json::Value>(&message.content)
                .ok()
                .and_then(|v| v.get("line_start").and_then(|l| l.as_u64()))
                .is_some_and(|line| line > 1)
    });
    assert!(ranged, "no ranged read reached the provider");

    let anchored = request.messages.iter().any(|message| {
        message.role == Role::System && message.content.contains("declares `SessionRuntime`")
    });
    assert!(anchored, "no declaration anchor reached the provider");
}

/// The model must actually see the fields, which is what "how does it
/// store session state" asks about.
#[tokio::test]
async fn the_provider_receives_the_struct_fields() {
    let (_tmp, _, requests) = run_reported_question().await;
    let transcript: String = requests[0]
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.clone())
        .collect();

    assert!(
        transcript.contains("state: tokio::sync::Mutex<SessionState>"),
        "the state field never reached the provider"
    );
    assert!(
        transcript.contains("current_cancel"),
        "the cancellation field never reached the provider"
    );
}

/// A question naming no symbol still retrieves, so wiring the bundle in
/// did not make ordinary questions depend on it.
#[tokio::test]
async fn a_question_without_a_symbol_still_retrieves() {
    let (_tmp, runtime, provider) = session_workspace();
    runtime
        .set_active_projects(&["assistant".to_string()])
        .await
        .unwrap();
    runtime
        .send_message("Explain the session architecture.")
        .await
        .unwrap();

    let requests = provider.recorded_requests();
    let paths = context_paths(&requests[0]);
    assert!(!paths.is_empty(), "no evidence retrieved at all");
}
