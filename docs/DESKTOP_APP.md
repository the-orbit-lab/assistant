# Desktop application

`apps/desktop` is a Tauri 2 + React client for the JSON Lines protocol.
It renders Orbit; it does not reimplement it.

```text
React UI
   ↓ invoke / listen
Tauri commands  (src-tauri/src/lib.rs)
   ↓
Process manager (src-tauri/src/sidecar.rs)
   ↓ stdin / stdout
orbit app serve --jsonl
   ↓
SessionRuntime → Agent → Actions → Ollama
```

## Why the process manager is in Rust

Three properties are only enforceable on that side of the boundary:

- **stdout is frames, nothing else.** One JSON object per line. A line
  that does not parse is forwarded as a *diagnostic* rather than dropped
  — Orbit guarantees stdout is frames only, so a malformed line means
  something is wrong and hiding it would hide the problem.
- **stderr never enters protocol parsing.** It is read on its own thread
  and emitted as a separate event, so no amount of logging can be
  mistaken for a frame.
- **The webview cannot name a program.** It supplies a workspace
  directory; the binary and the argument list are chosen in Rust.

## Protocol version

`SUPPORTED_PROTOCOL_VERSION` in `sidecar.rs` and
`SUPPORTED_PROTOCOL_VERSION` in `src/protocol/frames.ts` must agree with
the backend's `ready` frame. A mismatch is an explicit upgrade error, not
a silent failure. `src-tauri/tests/real_backend.rs` asserts the real
binary still announces the version the app expects.

An **unrecognized frame is ignored**, because the protocol says adding an
event type is backwards-compatible. A client that crashed on one would
break on every backend upgrade.

## State

`src/state/session.ts` is a pure reducer: frames in, conversation out.
The rules it encodes come from the protocol doc —

- deltas accumulate into the assistant message for their own `turn_id`;
- `model_response_completed` replaces the accumulated text with the
  authoritative final text;
- action rows group by `execution_id`, so `action_started` without a
  completion is a spinner and the matching frame resolves it;
- citations come only from `source_found`, never from answer prose;
- cancellation keeps whatever text already arrived and leaves the session
  usable.

Because it is pure, all of that is tested without a backend, a webview,
or a model.

## Source deduplication

By `project + path + line_start + line_end + section`, matching the
backend's own rule so the two agree. A path-only citation is dropped once
precise lines for the same file exist — the precise one is what the
answer rests on.

## Limitations

- Voice is not implemented. See [VOICE.md](VOICE.md).
- Answers render as plain text; Markdown rendering and code-block copy
  are not built yet.
- The workspace path is remembered in `localStorage`; there is no
  settings screen for the binary path.
- macOS is the only platform exercised.
