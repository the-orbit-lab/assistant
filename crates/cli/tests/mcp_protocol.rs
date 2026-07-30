//! Real subprocess, real `stdio` MCP protocol tests: this spawns the
//! actual compiled `orbit` binary and speaks newline-delimited JSON-RPC to
//! its stdin/stdout, exactly as Claude Code or any other MCP host would.
//! Nothing here calls a Rust function directly -- if `orbit mcp serve`
//! ever prints a banner, a debug line, or anything else to stdout, every
//! assertion in this file that parses a response line as JSON fails.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};

struct McpProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    /// Every response/notification line received, kept so tests can assert
    /// the whole transcript is clean, not just the lines they inspected.
    raw_lines: Vec<String>,
}

impl McpProcess {
    fn spawn(project_dir: &std::path::Path) -> Self {
        let mut cmd = Command::cargo_bin("orbit").expect("orbit binary must be built");
        cmd.args(["--project", project_dir.to_str().unwrap(), "mcp", "serve"])
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

    /// Reads lines until it finds the response matching `id`. Every line
    /// read along the way -- including this one -- must be valid JSON:
    /// that is the "stdout carries protocol traffic only" guarantee.
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
                panic!(
                    "line on stdout was not valid JSON-RPC (stdout is protocol-only, this would \
                     corrupt every MCP client): {e}\nline: {trimmed:?}"
                )
            });
            if parsed.get("id") == Some(&json!(id)) {
                return parsed;
            }
            // Otherwise it was a notification; keep reading for our response.
        }
    }

    fn initialize(&mut self) -> Value {
        let result = self.send_request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "orbit-protocol-test", "version": "0.0.1"}
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

fn write_validation_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("README.md"),
        "# Orbit Test Project\n\nThis is a local-first AI engineering assistant fixture used to \
         validate the MCP protocol boundary.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("CLAUDE.md"),
        "# Instructions\n\nFollow the engineering assistant guidelines in this repository.\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(
        dir.join("docs/PROJECT_SPEC.md"),
        "# Spec\n\nThe engineering assistant must ground every answer in project sources.\n",
    )
    .unwrap();
    std::fs::write(dir.join(".env"), "SECRET_TOKEN=do-not-leak-me\n").unwrap();
    std::fs::create_dir_all(dir.join(".orbit")).unwrap();
    std::fs::write(
        dir.join(".orbit/project.yaml"),
        "version: 1\n\
         project:\n  name: mcp-validation\n\
         context:\n  include:\n    - \"**/*\"\n\
         permissions:\n  project.information: allow\n  project.list_files: allow\n  \
         project.read_file: allow\n  project.search: allow\n\
         mcp:\n  expose:\n    - project.information\n    - project.list_files\n    - \
         project.read_file\n    - project.search\n",
    )
    .unwrap();
}

#[test]
fn full_protocol_round_trip_over_a_real_subprocess() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().canonicalize().unwrap();
    write_validation_project(&project_dir);

    let mut mcp = McpProcess::spawn(&project_dir);

    // 1. initialize
    let init = mcp.initialize();
    assert!(init.get("result").is_some(), "initialize failed: {init:?}");

    // 2. list exposed tools
    let tools = mcp.list_tools();
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("tools/list must return an array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "project.information",
        "project.list_files",
        "project.read_file",
        "project.search",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing tool {expected}: {names:?}"
        );
    }
    assert_eq!(
        names.len(),
        4,
        "only the exposed actions may be listed: {names:?}"
    );

    // 3. call a read-only tool: project.information
    let info = mcp.call_tool("project.information", json!({}));
    assert_ne!(info["result"]["isError"], json!(true), "{info:?}");

    // 4. project.search must preserve structured source metadata
    let search = mcp.call_tool("project.search", json!({"query": "engineering assistant"}));
    assert_ne!(search["result"]["isError"], json!(true), "{search:?}");
    let text = search["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).expect("tool content must be structured JSON");
    let results = payload["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    assert!(
        results
            .iter()
            .all(|r| r.get("line_start").is_some() && r.get("path").is_some()),
        "line ranges and paths must survive the MCP conversion: {results:?}"
    );

    // 5. project.read_file on an allowed file
    let read = mcp.call_tool("project.read_file", json!({"path": "README.md"}));
    let read_text = read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(read_text.contains("Orbit Test Project"));

    // 6. .env must never be returned, even if asked for by name
    let env_read = mcp.call_tool("project.read_file", json!({"path": ".env"}));
    assert_eq!(env_read["result"]["isError"], json!(true));
    let env_error = env_read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(env_error.contains("excluded"), "{env_error}");
    assert!(!env_error.contains("do-not-leak-me"));

    // 7. path traversal must be rejected, not silently resolved
    let traversal = mcp.call_tool("project.read_file", json!({"path": "../../../etc/passwd"}));
    assert_eq!(traversal["result"]["isError"], json!(true));
    let traversal_error = traversal["result"]["content"][0]["text"].as_str().unwrap();
    assert!(traversal_error.contains("outside"), "{traversal_error}");

    // 8. an unexposed action is rejected with a useful error, not run. It
    // is unroutable (no such tool on this server), so it comes back as a
    // JSON-RPC protocol error rather than a CallToolResult.
    let denied = mcp.call_tool("command.run_configured", json!({"name": "anything"}));
    assert!(denied.get("result").is_none(), "{denied:?}");
    let denied_error = denied["error"]["message"].as_str().unwrap();
    assert!(denied_error.contains("not exposed"), "{denied_error}");

    // 9. an unknown tool name is rejected, not treated as any exposed one
    let unknown = mcp.call_tool("project.does_not_exist", json!({}));
    assert!(unknown.get("result").is_none(), "{unknown:?}");
    let unknown_error = unknown["error"]["message"].as_str().unwrap();
    assert!(unknown_error.contains("not exposed"), "{unknown_error}");

    // Every single line seen on stdout across this whole session -- init,
    // list, six tool calls -- must have been valid JSON-RPC. read_response
    // already asserts this per line; this is the belt-and-suspenders
    // whole-transcript check.
    assert!(!mcp.raw_lines.is_empty());
    for line in &mcp.raw_lines {
        assert!(
            serde_json::from_str::<Value>(line).is_ok(),
            "non-JSON line on stdout: {line:?}"
        );
    }

    mcp.shutdown();
}

#[test]
fn stderr_may_carry_startup_diagnostics_but_stdout_never_does() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().canonicalize().unwrap();
    write_validation_project(&project_dir);

    let mut mcp = McpProcess::spawn(&project_dir);
    let init = mcp.initialize();
    assert!(init.get("result").is_some());
    // The very first bytes on stdout, before any of our own reads, must
    // already have been the initialize response -- nothing (no banner, no
    // "starting..." line) was interleaved ahead of it.
    assert_eq!(mcp.raw_lines.len(), 1);
    assert!(serde_json::from_str::<Value>(&mcp.raw_lines[0]).is_ok());
    mcp.shutdown();
}
