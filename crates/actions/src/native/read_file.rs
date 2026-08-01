use std::path::PathBuf;

use orbit_core::{
    ActionDescriptor, ActionInput, ActionOutput, OrbitError, Permission, SourceReference,
};
use orbit_project::{read_allowed_file, read_allowed_file_truncated};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::registry::{Action, ActionContext};

pub const NAME: &str = "project.read_file";

/// Hard ceiling on a single read, independent of any caller-supplied limit,
/// so a huge `max_bytes` value can't be used to blow the model context or
/// exhaust memory.
const MAX_ALLOWED_READ_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_READ_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ReadFileInput {
    path: String,
    #[serde(default)]
    max_bytes: Option<u64>,
    /// Return the first `max_bytes` of an oversized file instead of
    /// failing. Off by default, so existing callers keep the strict
    /// behavior they rely on.
    #[serde(default)]
    truncate: bool,
    /// First line to return, 1-indexed and inclusive.
    ///
    /// A range is applied *after* the same security checks a whole-file
    /// read performs, so it can only ever return less than the caller
    /// was already allowed to see. Retrieval uses it to quote the exact
    /// span of a declaration instead of spending a model's context on
    /// the other six hundred lines of the file that declares it.
    #[serde(default)]
    line_start: Option<usize>,
    /// Last line to return, 1-indexed and inclusive.
    #[serde(default)]
    line_end: Option<usize>,
}

/// Take `[line_start, line_end]` (1-indexed, inclusive) from `content`.
///
/// Out-of-range bounds clamp rather than fail: a span that runs past the
/// end of a truncated read should yield what exists, not an error that
/// costs the caller the evidence entirely.
fn slice_lines(content: &str, start: Option<usize>, end: Option<usize>) -> (String, usize) {
    if start.is_none() && end.is_none() {
        return (content.to_string(), 1);
    }
    let first = start.unwrap_or(1).max(1);
    let last = end.unwrap_or(usize::MAX).max(first);
    let selected: Vec<&str> = content
        .lines()
        .skip(first - 1)
        .take(last - first + 1)
        .collect();
    (selected.join("\n"), first)
}

pub struct ReadFileAction;

#[async_trait::async_trait]
impl Action for ReadFileAction {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            name: NAME.to_string(),
            description: "Read a single allowed project file as UTF-8 text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the project root" },
                    "max_bytes": { "type": "integer", "minimum": 1 },
                    "truncate": {
                        "type": "boolean",
                        "description": "Return the first max_bytes instead of failing when the file is larger."
                    },
                    "line_start": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to return, 1-indexed and inclusive."
                    },
                    "line_end": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line to return, 1-indexed and inclusive."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            default_permission: Permission::Allow,
        }
    }

    fn validate(&self, input: &Value) -> Result<(), OrbitError> {
        serde_json::from_value::<ReadFileInput>(input.clone()).map_err(|e| {
            OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            }
        })?;
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &ActionContext,
        input: ActionInput,
    ) -> Result<ActionOutput, OrbitError> {
        let parsed: ReadFileInput =
            serde_json::from_value(input.0).map_err(|e| OrbitError::InvalidActionInput {
                name: NAME.to_string(),
                reason: e.to_string(),
            })?;

        let requested = parsed
            .max_bytes
            .unwrap_or(DEFAULT_READ_BYTES)
            .min(MAX_ALLOWED_READ_BYTES);
        let ranged = parsed.line_start.is_some() || parsed.line_end.is_some();

        // A byte budget and a line range measure different things. Cutting
        // the file to `max_bytes` *before* slicing silently returns
        // nothing whenever the requested lines sit past that cut — asking
        // for lines 490-549 of a file truncated at line 350 yielded an
        // empty read, and the evidence simply vanished. So a ranged read
        // takes the whole file (still under the hard ceiling) and lets the
        // range do the narrowing; `max_bytes` then bounds the *result*.
        let limit = if ranged {
            MAX_ALLOWED_READ_BYTES
        } else {
            requested
        };

        let relative = PathBuf::from(&parsed.path);
        let (content, truncated) = if parsed.truncate {
            read_allowed_file_truncated(&ctx.root, &ctx.config, &relative, limit)?
        } else {
            (
                read_allowed_file(&ctx.root, &ctx.config, &relative, limit)?,
                false,
            )
        };
        let (mut content, first_line) = slice_lines(&content, parsed.line_start, parsed.line_end);
        let mut truncated = truncated;
        if ranged && content.len() as u64 > requested {
            content.truncate(requested as usize);
            truncated = true;
        }
        let last_line = first_line + content.lines().count().saturating_sub(1);

        // Never log file content -- only that a read happened and its size.
        tracing::debug!(
            path = %parsed.path,
            bytes = content.len(),
            truncated,
            ranged,
            "project.read_file executed"
        );

        // A ranged read cites the lines it actually returned, so the
        // source list points at the declaration rather than at the whole
        // file that happens to contain it.
        let source = if ranged {
            SourceReference::lines(relative, first_line, last_line)
        } else {
            SourceReference::whole_file(relative)
        };

        Ok(ActionOutput::new(json!({
            "path": parsed.path,
            "size": content.len(),
            // Stated explicitly so the model knows it is seeing part of a
            // file rather than all of it.
            "truncated": truncated,
            "line_start": first_line,
            "line_end": last_line,
            "content": content,
        }))
        .with_sources(vec![source]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Action;
    use crate::test_support::default_context;

    #[tokio::test]
    async fn reads_an_allowed_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "hello there").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ReadFileAction
            .execute(&ctx, ActionInput(json!({"path": "README.md"})))
            .await
            .unwrap();
        assert_eq!(out.data["content"], "hello there");
        assert_eq!(out.sources.len(), 1);
    }

    #[tokio::test]
    async fn rejects_traversal_via_the_action() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let err = ReadFileAction
            .execute(&ctx, ActionInput(json!({"path": "../../etc/passwd"})))
            .await
            .unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[test]
    fn validate_rejects_missing_path() {
        let err = ReadFileAction.validate(&json!({})).unwrap_err();
        assert!(matches!(err, OrbitError::InvalidActionInput { .. }));
    }

    #[tokio::test]
    async fn reads_only_the_requested_line_range() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ReadFileAction
            .execute(
                &ctx,
                ActionInput(json!({"path": "a.rs", "line_start": 2, "line_end": 4})),
            )
            .await
            .unwrap();
        assert_eq!(out.data["content"], "two\nthree\nfour");
        assert_eq!(out.data["line_start"], 2);
        assert_eq!(out.data["line_end"], 4);
    }

    /// A ranged read cites the lines it returned, not the whole file, so
    /// the source list points at the declaration itself.
    #[tokio::test]
    async fn a_ranged_read_cites_its_lines() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ReadFileAction
            .execute(
                &ctx,
                ActionInput(json!({"path": "a.rs", "line_start": 2, "line_end": 3})),
            )
            .await
            .unwrap();
        assert_eq!(out.sources[0].line_start, Some(2));
        assert_eq!(out.sources[0].line_end, Some(3));
    }

    /// A range can only ever narrow a read. It is applied after the same
    /// security checks, so it cannot reach a file a whole-file read
    /// could not, and out-of-range bounds clamp instead of failing.
    #[tokio::test]
    async fn a_range_never_widens_access() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let err = ReadFileAction
            .execute(
                &ctx,
                ActionInput(json!({"path": "../secrets.txt", "line_start": 1, "line_end": 9999})),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, OrbitError::PathOutsideProject { .. }));
    }

    #[tokio::test]
    async fn an_out_of_range_span_clamps_to_what_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "one\ntwo\n").unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());
        let out = ReadFileAction
            .execute(
                &ctx,
                ActionInput(json!({"path": "a.rs", "line_start": 2, "line_end": 500})),
            )
            .await
            .unwrap();
        assert_eq!(out.data["content"], "two");
    }

    #[test]
    fn an_absent_range_returns_the_whole_file() {
        let (content, first) = slice_lines("a\nb\nc", None, None);
        assert_eq!(content, "a\nb\nc");
        assert_eq!(first, 1);
    }

    /// A range past a `max_bytes` truncation point must still return the
    /// requested lines. Applying the byte cut first silently returned an
    /// empty read and the evidence disappeared without an error.
    #[tokio::test]
    async fn a_range_beyond_max_bytes_still_returns_its_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (1..=500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(tmp.path().join("big.rs"), &body).unwrap();
        let ctx = default_context(&tmp.path().canonicalize().unwrap());

        let out = ReadFileAction
            .execute(
                &ctx,
                ActionInput(json!({
                    "path": "big.rs",
                    "max_bytes": 100,
                    "truncate": true,
                    "line_start": 490,
                    "line_end": 495
                })),
            )
            .await
            .unwrap();

        let content = out.data["content"].as_str().unwrap();
        assert!(!content.is_empty(), "ranged read returned nothing");
        assert!(content.starts_with("line 490"), "{content}");
        assert_eq!(out.data["line_start"], 490);
    }
}
