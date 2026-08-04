# Agent Event Stream

Everything Orbit does inside a session is reported as a structured event.
The interactive CLI, the JSON Lines bridge, and a future SwiftUI client
all observe the *same* stream — no front end re-derives what happened by
parsing printed text, and no agent logic lives inside a renderer or a wire
protocol.

```text
User input
    ↓
Session Runtime            (orbit-session)
    ↓
Agent + Workspace + Actions + Provider
    ↓
Agent Event Bus            (orbit_core::EventSink)
    ├── CLI renderer                 (crates/cli/src/commands/chat.rs)
    ├── JSONL application bridge     (crates/cli/src/app/)
    └── future SwiftUI client
```

Types live in [`orbit_core::event`](../crates/core/src/event.rs).

## Two rules

1. **Events describe real work.** Every event corresponds to a request
   actually made, an action actually executed, or bytes actually received
   from a provider. There are no decorative "thinking" events, and no
   event is synthesized to make the stream look busier.
2. **Events are safe to display and log.** Action arguments are passed
   through `summarize_arguments`, which redacts secret-shaped keys
   (`*token*`, `*secret*`, `*password*`, `*api_key*`, `*auth*`, …),
   shortens absolute paths to their last two components, and collapses
   long strings and containers. The action itself always receives the
   original, unmodified input — the summary is for display only.

## Envelope

Every frame carries correlation fields alongside its payload:

| Field | Meaning |
|---|---|
| `type` | The event name (the payload tag). |
| `session_id` | The session this belongs to. Always present. |
| `turn_id` | The turn, when the event belongs to one. |
| `execution_id` | One action execution, when the event belongs to one. |
| `timestamp_ms` | Unix milliseconds. |
| `project` | The project concerned, when exactly one is. |

`turn_id`, `execution_id`, and `project` are omitted when they do not
apply, rather than being sent as `null`.

```json
{"type":"source_found","session_id":"sess-…","turn_id":"turn-1","execution_id":"exec-3","timestamp_ms":1738245123456,"project":"docs","path":"obc/ADR-0004.md","line_start":18,"line_end":41}
```

### Project identity

`project` is the *single* project an event concerns. A workspace-scoped
action (`workspace.search` across `docs` and `obc`) has no single project,
so its `action_*` events omit `project` entirely rather than reporting the
workspace's own name as if it were a project. Per-project identity for
that work arrives on the `source_found` events, each of which names the
project its source really came from.

## Event catalogue

### Session

| Event | Payload |
|---|---|
| `session_started` | `protocol_version`, `mode` (`single_project`/`workspace`), `workspace?`, `projects` |
| `session_ended` | `reason` |
| `user_message_received` | `text` |
| `active_projects_changed` | `projects` |
| `turn_completed` | `source_count`, `action_count` |

### Deterministic retrieval

| Event | Payload |
|---|---|
| `retrieval_started` | `scope` (empty = workspace-level) |
| `retrieval_completed` | `scope`, `action_count`, `source_count` |

### Actions

| Event | Payload |
|---|---|
| `action_requested` | `action`, `arguments` (redacted summary) |
| `permission_required` | `request_id`, `action`, `description`, `arguments` |
| `permission_resolved` | `request_id`, `decision` |
| `action_started` | `action` |
| `action_progress` | `action`, `message` |
| `action_completed` | `action`, `duration_ms`, `source_count` |
| `action_failed` | `action`, `error` |
| `source_found` | `path`, `line_start?`, `line_end?`, `section?` |

`action_progress` is part of the wire contract for future consumers but is
**not emitted today**, because no current action produces intermediate
progress. It is documented rather than faked.

### Model

| Event | Payload |
|---|---|
| `model_response_started` | `model`, `streaming` |
| `response_delta` | `text` |
| `model_response_completed` | `text` |

### Control

| Event | Payload |
|---|---|
| `execution_cancelled` | `reason` |
| `warning` | `message` |
| `failure` | `message` |

## Ordering guarantees

These are enforced by construction and covered by tests.

**A normal turn:**

```text
user_message_received
  → [retrieval_started → action events → retrieval_completed]
  → model_response_started → response_delta* → model_response_completed
  → [action events → model_response_started → … ]   (repeats per tool round)
  → turn_completed
```

- `user_message_received` is the first event of a turn; `turn_completed`
  is the last.
- **One model response per agent iteration.** A turn that calls tools
  produces several `model_response_started`/`model_response_completed`
  pairs; the *last* one carries the final answer.
- `action_completed` or `action_failed` always follows the matching
  `action_started`, with the same `execution_id`.
- **`action_started` means the action really ran.** Invalid input, a
  `deny` permission, or a refused confirmation goes straight to
  `action_failed` without an `action_started`.
- `permission_required` is always followed by exactly one
  `permission_resolved` with the same `request_id`, and both precede
  `action_started`.
- `source_found` events always sit between an `action_started` and its
  `action_completed` — a source can only come from an action that ran.
- Concatenating every `response_delta` of one model response yields
  exactly that response's `model_response_completed` text. This holds for
  non-streaming providers too, which emit a single delta.

**A cancelled turn** ends with `execution_cancelled` and **no**
`turn_completed`. Events for work that had already finished are not
retracted; cancellation stops future work and never claims completed work
was undone.

**A failed turn** emits `failure` and no `turn_completed`.

## Sources are never invented

A `source_found` event is emitted only from `ActionOutput::sources` —
what an executed action actually returned. The model's answer text cannot
produce one, no matter what paths it mentions. This is enforced in the
Action Runtime (`ActionRegistry::execute_observed`), not in any renderer,
and is covered by
`model_prose_cannot_produce_a_source_event`.

## Implementing a sink

```rust
use std::sync::Arc;
use orbit_core::{AgentEvent, EventSink};

struct MySink;

impl EventSink for MySink {
    fn emit(&self, event: AgentEvent) {
        println!("{}", serde_json::to_string(&event).unwrap());
    }
}
```

`emit` must be cheap and must not block: it is called from the turn's own
task. A front end that needs to block (the CLI's confirmation prompt reads
stdin) should hand the work to another thread and resolve asynchronously —
see `TerminalRenderer` in `crates/cli/src/commands/chat.rs`.

`orbit_core` ships two sinks for convenience: `NullSink` (discards
everything; reports `is_enabled() == false` so emitters can skip building
payloads) and `CollectingSink` (records events in order, used by tests).

## Versioning

`EVENT_PROTOCOL_VERSION` (currently `1`) covers event names and payload
fields. It is reported in the `ready` frame of the JSONL bridge and in
every `session_started`. Adding a new event type or an optional field is
backwards-compatible; renaming or removing one is not and increments the
version.
