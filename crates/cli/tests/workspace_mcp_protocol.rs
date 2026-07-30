//! Real subprocess, real `stdio` MCP protocol test for **workspace** mode
//! (`orbit --workspace <dir> mcp serve`), mirroring `tests/mcp_protocol.rs`.
//! This exists to prove, at the protocol level and not just via direct
//! Rust calls, that workspace mode exposes exactly the six `workspace.*`
//! actions -- never one dynamically generated tool per repository -- and
//! that project identity and permission isolation survive the MCP
//! boundary.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    raw_lines: Vec<String>,
}

impl McpProcess {
    fn spawn(workspace_dir: &std::path::Path) -> Self {
        let mut cmd = Command::cargo_bin("orbit").expect("orbit binary must be built");
        cmd.args([
            "--workspace",
            workspace_dir.to_str().unwrap(),
            "mcp",
            "serve",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn orbit mcp serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            raw_lines: Vec::new(),
        }
    }

    fn send_request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&message);
        self.read_response(id)
    }

    fn send_notification(&mut self, method: &str, params: Value) {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_line(&message);
    }

    fn write_line(&mut self, message: &Value) {
        let line = serde_json::to_string(message).unwrap();
        writeln!(self.stdin, "{line}").expect("failed to write to orbit stdin");
        self.stdin.flush().unwrap();
    }

    fn read_response(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            let bytes_read = self
                .stdout
                .read_line(&mut line)
                .expect("failed to read orbit stdout");
            assert!(bytes_read > 0, "orbit closed stdout before responding");
            let trimmed = line.trim_end().to_string();
            self.raw_lines.push(trimmed.clone());
            let parsed: Value = serde_json::from_str(&trimmed).unwrap_or_else(|e| {
                panic!("line on stdout was not valid JSON-RPC: {e}\nline: {trimmed:?}")
            });
            if parsed.get("id") == Some(&json!(id)) {
                return parsed;
            }
        }
    }

    fn initialize(&mut self) -> Value {
        let result = self.send_request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "orbit-workspace-protocol-test", "version": "0.0.1"}
            }),
        );
        self.send_notification("notifications/initialized", json!({}));
        result
    }

    fn list_tools(&mut self) -> Value {
        self.send_request("tools/list", json!({}))
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.send_request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn write_project(root: &std::path::Path, name: &str, extra_files: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(
        root.join(".orbit/project.yaml"),
        format!(
            "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n\
             permissions:\n  project.information: allow\n  project.list_files: allow\n  \
             project.read_file: allow\n  project.search: allow\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join("README.md"), format!("# {name}\n")).unwrap();
    for (path, content) in extra_files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
}

fn write_validation_workspace(root: &std::path::Path) {
    write_project(
        &root.join("docs"),
        "docs",
        &[(
            "adr/decision.md",
            "STM32 was chosen for its low power draw.\n",
        )],
    );
    write_project(
        &root.join("obc"),
        "obc",
        &[(
            "src/watchdog.rs",
            "// resets the system\nfn watchdog() {}\n",
        )],
    );
    std::fs::write(root.join("docs/.env"), "SECRET_TOKEN=do-not-leak-me\n").unwrap();

    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(
        root.join(".orbit/workspace.yaml"),
        "version: 1\n\
         workspace:\n  name: Orbit Lab\n  description: MCP validation workspace\n\
         projects:\n\
         \x20\x20docs:\n    path: ./docs\n\
         \x20\x20obc:\n    path: ./obc\n\
         defaults:\n  project: docs\n",
    )
    .unwrap();
}

#[test]
fn workspace_mode_exposes_exactly_the_six_workspace_actions() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace_dir = tmp.path().canonicalize().unwrap();
    write_validation_workspace(&workspace_dir);

    let mut mcp = McpProcess::spawn(&workspace_dir);

    let init = mcp.initialize();
    assert!(init.get("result").is_some(), "initialize failed: {init:?}");

    let tools = mcp.list_tools();
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("tools/list must return an array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    let expected = [
        "workspace.information",
        "workspace.list_projects",
        "workspace.project_information",
        "workspace.search",
        "workspace.read_file",
        "workspace.list_project_files",
    ];
    for name in expected {
        assert!(
            names.contains(&name.to_string()),
            "missing tool {name}: {names:?}"
        );
    }
    assert_eq!(
        names.len(),
        expected.len(),
        "workspace mode must expose exactly the six workspace.* actions, never one tool per \
         registered repository: {names:?}"
    );

    mcp.shutdown();
}

#[test]
fn workspace_search_results_carry_project_identity_and_stay_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace_dir = tmp.path().canonicalize().unwrap();
    write_validation_workspace(&workspace_dir);

    let mut mcp = McpProcess::spawn(&workspace_dir);
    mcp.initialize();

    let search = mcp.call_tool(
        "workspace.search",
        json!({"query": "STM32", "projects": ["docs", "obc"]}),
    );
    assert_ne!(search["result"]["isError"], json!(true), "{search:?}");
    let text = search["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).expect("tool content must be structured JSON");
    let results = payload["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    assert!(
        results.iter().all(|r| r.get("project").is_some()),
        "every workspace search result must carry its owning project: {results:?}"
    );
    assert!(
        results.iter().all(|r| r["project"] == "docs"),
        "STM32 only appears in docs; a workspace search must never attribute a match to the \
         wrong project: {results:?}"
    );

    // read_file must stay inside the named project's own root.
    let read = mcp.call_tool(
        "workspace.read_file",
        json!({"project": "obc", "path": "src/watchdog.rs"}),
    );
    assert_ne!(read["result"]["isError"], json!(true), "{read:?}");
    let read_text = read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(read_text.contains("resets the system"));

    // A path that reaches into a sibling project must be rejected, not
    // silently resolved to the sibling's file.
    let cross_project = mcp.call_tool(
        "workspace.read_file",
        json!({"project": "obc", "path": "../docs/adr/decision.md"}),
    );
    assert_eq!(
        cross_project["result"]["isError"],
        json!(true),
        "{cross_project:?}"
    );

    // .env must never be returned even inside a workspace-scoped read.
    let env_read = mcp.call_tool(
        "workspace.read_file",
        json!({"project": "docs", "path": ".env"}),
    );
    assert_eq!(env_read["result"]["isError"], json!(true));
    let env_error = env_read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!env_error.contains("do-not-leak-me"));

    // An unregistered project name is rejected, not guessed at.
    let unknown_project = mcp.call_tool(
        "workspace.search",
        json!({"query": "STM32", "projects": ["nonexistent"]}),
    );
    assert_eq!(unknown_project["result"]["isError"], json!(true));

    for line in &mcp.raw_lines {
        assert!(
            serde_json::from_str::<Value>(line).is_ok(),
            "non-JSON line on stdout: {line:?}"
        );
    }

    mcp.shutdown();
}
