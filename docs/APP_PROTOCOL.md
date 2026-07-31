# Application protocol (JSON Lines)

```bash
orbit app serve --jsonl
```

drives Orbit [sessions](SESSIONS.md) over stdin/stdout as newline-delimited
JSON: commands in, [events](EVENTS.md) out. It is the interface a desktop
or voice front end uses instead of reimplementing agent logic.

- **stdin**: one JSON command object per line.
- **stdout**: one JSON frame per line — nothing else, ever. A client can
  parse every stdout line without filtering.
- **stderr**: all logging and diagnostics.

`--jsonl` is required. It is the only transport today, and requiring it
keeps `orbit app serve` from silently meaning something else once another
one exists.

## Versioning

The first frame is always:

```json
{"type":"ready","protocol_version":1}
```

The same version appears in every `session_started`. Adding an event type
or an optional field is backwards-compatible; renaming or removing one
increments the version. A client should check it at startup and refuse a
version it does not understand.

## Commands

### `session.start`

```json
{"type":"session.start","workspace":"/Users/me/orbit-lab"}
{"type":"session.start","project":"/Users/me/orbit-lab/obc"}
{"type":"session.start","project":"obc","streaming":true,"permissions":"external"}
```

| Field | Default | Meaning |
|---|---|---|
| `workspace` | — | Workspace directory. |
| `project` | — | Project path, or a registered name/alias. |
| `streaming` | `true` | Deliver assistant text as `response_delta` events. |
| `permissions` | `"external"` | `external`, `allow_all`, or `deny_all`. |

With neither `workspace` nor `project`, Orbit discovers one from the
working directory using the ordinary precedence rules (see
[WORKSPACES.md](WORKSPACES.md#discovery-precedence)).

Replies with a `session_started` **event** carrying the new `session_id` —
the client reads its id from the event stream, not from a separate ack.

### `message.send`

```json
{"type":"message.send","session_id":"sess-…","text":"Why was STM32 selected for the OBC?"}
```

Runs one turn. The turn is processed on its own task, so
`execution.cancel` and `permission.resolve` remain responsive while it
runs.

### `permission.resolve`

```json
{"type":"permission.resolve","request_id":"perm-…","decision":"allow_once"}
```

`decision` is `allow_once` or `deny_once`. The `request_id` comes from a
`permission_required` event. There is deliberately no "always allow" — a
persistent change belongs in `.orbit/project.yaml`.

### `execution.cancel`

```json
{"type":"execution.cancel","session_id":"sess-…"}
{"type":"execution.cancel","session_id":"sess-…","execution_id":"exec-7"}
```

Cancels the turn currently running. `execution_id` is accepted for
symmetry with the events but is not required: a session runs one turn at a
time. Replies `nothing_to_cancel` when idle.

### `projects.set` / `projects.list`

```json
{"type":"projects.set","session_id":"sess-…","projects":["docs","obc"]}
{"type":"projects.list","session_id":"sess-…"}
```

`projects.set` replaces the active set (workspace sessions only) and emits
`active_projects_changed`. An unknown or unavailable name is an error and
changes nothing.

### `session.status`

```json
{"type":"session.status","session_id":"sess-…"}
```

### `session.end`

```json
{"type":"session.end","session_id":"sess-…"}
```

Shuts down the session's external MCP servers and emits `session_ended`.
Closing stdin ends every live session the same way.

## Non-event frames

Alongside [event frames](EVENTS.md#event-catalogue), stdout carries:

```json
{"type":"ready","protocol_version":1}
{"type":"ack","request":"execution.cancel","session_id":"sess-…"}
{"type":"error","code":"unknown_session","message":"no live session `sess-x`"}
{"type":"status","session_id":"sess-…","mode":"workspace","workspace":"Orbit Lab","active_projects":["docs","obc"],"turns":2,"message_count":9,"source_count":5,"action_count":4,"command_run_count":0,"running":false,"pending_permissions":[],"streaming":true}
{"type":"projects","session_id":"sess-…","projects":[{"name":"obc","available":true,"aliases":["flight-computer"]}]}
```

These names are deliberately distinct from every event name, so one stream
carries both.

### Error codes

| Code | Meaning |
|---|---|
| `malformed_json` | The line was not valid JSON. |
| `unknown_request` | Valid JSON, but not a known request (or a missing/unknown field). |
| `unknown_session` | `session_id` does not name a live session. |
| `session_start_failed` | No project/workspace found, or its config is invalid. |
| `request_failed` | Valid request that could not be carried out. |
| `nothing_to_cancel` | No turn was running. |
| `unknown_permission_request` | The `request_id` is not pending. |

**A bad message never ends the process.** Malformed input, unknown
requests, and unknown sessions all produce an `error` frame and the bridge
keeps serving. Unknown *fields* are rejected rather than ignored, so a
typo surfaces instead of silently doing nothing.

## A worked example

Comparing the documented STM32 decision with the OBC implementation, then
cancelling a follow-up. `→` is stdin, `←` is stdout (abridged).

```json
← {"type":"ready","protocol_version":1}
→ {"type":"session.start","workspace":"/Users/me/orbit-lab","permissions":"external"}
← {"type":"session_started","session_id":"sess-19a2","timestamp_ms":1738245100000,"protocol_version":1,"mode":"workspace","workspace":"Orbit Lab","projects":[]}

→ {"type":"projects.set","session_id":"sess-19a2","projects":["docs","obc"]}
← {"type":"active_projects_changed","session_id":"sess-19a2","timestamp_ms":1738245100100,"projects":["docs","obc"]}
← {"type":"ack","request":"projects.set","session_id":"sess-19a2"}

→ {"type":"message.send","session_id":"sess-19a2","text":"Compare the documented STM32 decision with the OBC implementation."}
← {"type":"user_message_received","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100200,"text":"Compare the documented STM32 decision with the OBC implementation."}
← {"type":"retrieval_started","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100201,"scope":["docs","obc"]}
← {"type":"action_requested","session_id":"sess-19a2","turn_id":"turn-1","execution_id":"exec-1","timestamp_ms":1738245100202,"action":"workspace.search","arguments":"projects=[2 item(s)], query=STM32"}
← {"type":"action_started","session_id":"sess-19a2","turn_id":"turn-1","execution_id":"exec-1","timestamp_ms":1738245100203,"action":"workspace.search"}
← {"type":"source_found","session_id":"sess-19a2","turn_id":"turn-1","execution_id":"exec-1","timestamp_ms":1738245100240,"project":"docs","path":"obc/ADR-0004.md","line_start":18,"line_end":41}
← {"type":"source_found","session_id":"sess-19a2","turn_id":"turn-1","execution_id":"exec-1","timestamp_ms":1738245100241,"project":"obc","path":"src/watchdog.rs","line_start":2,"line_end":2}
← {"type":"action_completed","session_id":"sess-19a2","turn_id":"turn-1","execution_id":"exec-1","timestamp_ms":1738245100242,"action":"workspace.search","duration_ms":39,"source_count":2}
← {"type":"retrieval_completed","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100243,"scope":["docs","obc"],"action_count":3,"source_count":2}
← {"type":"model_response_started","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100250,"model":"qwen2.5:latest","streaming":true}
← {"type":"response_delta","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100600,"text":"The documentation indicates"}
← {"type":"response_delta","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100640,"text":" STM32 was chosen for low power draw."}
← {"type":"model_response_completed","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100900,"text":"The documentation indicates STM32 was chosen for low power draw."}
← {"type":"turn_completed","session_id":"sess-19a2","turn_id":"turn-1","timestamp_ms":1738245100901,"source_count":2,"action_count":3}
```

A configured command needs confirmation:

```json
→ {"type":"message.send","session_id":"sess-19a2","text":"Run the obc tests."}
← {"type":"action_requested","session_id":"sess-19a2","turn_id":"turn-2","execution_id":"exec-4","timestamp_ms":1738245200000,"project":"obc","action":"command.run_configured","arguments":"name=test"}
← {"type":"permission_required","session_id":"sess-19a2","turn_id":"turn-2","execution_id":"exec-4","timestamp_ms":1738245200001,"project":"obc","request_id":"perm-6f2a","action":"command.run_configured","description":"Run a command configured in this project.","arguments":"name=test"}
→ {"type":"permission.resolve","request_id":"perm-6f2a","decision":"allow_once"}
← {"type":"permission_resolved","session_id":"sess-19a2","turn_id":"turn-2","execution_id":"exec-4","timestamp_ms":1738245200500,"project":"obc","request_id":"perm-6f2a","decision":"allow_once"}
← {"type":"action_started","session_id":"sess-19a2","turn_id":"turn-2","execution_id":"exec-4","timestamp_ms":1738245200501,"project":"obc","action":"command.run_configured"}
← {"type":"action_completed","session_id":"sess-19a2","turn_id":"turn-2","execution_id":"exec-4","timestamp_ms":1738245201900,"project":"obc","action":"command.run_configured","duration_ms":1398,"source_count":0}
```

Cancelling a long answer about brownout recovery:

```json
→ {"type":"message.send","session_id":"sess-19a2","text":"Explain brownout recovery in exhaustive detail."}
← {"type":"response_delta","session_id":"sess-19a2","turn_id":"turn-3","timestamp_ms":1738245300400,"text":"Brownout recovery on the OBC"}
→ {"type":"execution.cancel","session_id":"sess-19a2"}
← {"type":"ack","request":"execution.cancel","session_id":"sess-19a2"}
← {"type":"execution_cancelled","session_id":"sess-19a2","turn_id":"turn-3","timestamp_ms":1738245300450,"reason":"cancelled by user"}
→ {"type":"session.end","session_id":"sess-19a2"}
← {"type":"session_ended","session_id":"sess-19a2","timestamp_ms":1738245300500,"reason":"client requested"}
```

Note that the cancelled turn has **no** `turn_completed`, and the two
sources found in turn 1 remain part of the session.

## Notes for a SwiftUI client

The protocol is designed so a native app is a renderer, not a
reimplementation:

- **Launch** `orbit app serve --jsonl` as a `Process` with piped stdio.
  Read stdout line-by-line and decode each line into an event; send
  commands as lines on stdin. Keep stderr for a log pane — never parse it.
- **Model the conversation from events, not from the answer text.**
  `user_message_received` opens a bubble; `response_delta` appends to the
  assistant bubble as it arrives; `model_response_completed` finalizes it.
  Because deltas always reconstruct the final text exactly, the streamed
  view never has to be reconciled with a different final value.
- **Group by `turn_id`,** and attach action rows by `execution_id`. An
  `action_started` without its `action_completed` yet is a spinner; the
  matching completion or failure resolves it.
- **Render `source_found` as citations** keyed by `(project, path,
  line_start)`. Never derive a citation from the answer text — Orbit
  guarantees these events come only from actions that really ran, and that
  guarantee is the client's too if it uses them.
- **Permissions are a sheet.** On `permission_required`, present `action`,
  `project`, `description`, and `arguments` (already redacted and safe to
  display), then send `permission.resolve` with the `request_id`. The turn
  is genuinely paused until you do; there is no timeout to race.
- **Cancellation is a button,** not a process kill: send
  `execution.cancel` and keep the session. Killing the process would
  discard the conversation, which lives only in Orbit's memory.
- **Project switching** is `projects.set`; reflect the resulting
  `active_projects_changed` rather than assuming it succeeded, because an
  invalid selection is rejected and changes nothing.
- **Check `protocol_version`** from `ready` before doing anything else.

Speech input and output are out of scope for this version: a voice front
end would sit on exactly this protocol, turning transcribed text into
`message.send` and `response_delta` into spoken output, with no change to
Orbit.

## Testing

`crates/cli/tests/app_protocol.rs` spawns the real compiled binary and
speaks this protocol to its actual stdin/stdout — session lifecycle,
project selection, action and source events, malformed input handling, and
the stdout-is-frames-only guarantee (every line read is asserted to parse
as JSON, so a stray `println!` anywhere in Orbit fails the test). It needs
no model: Ollama is pointed at a closed port, which still exercises
retrieval, actions, and sources before reporting a clean failure.
