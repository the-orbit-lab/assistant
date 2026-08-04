# Sessions

A session is a stateful, multi-turn conversation with Orbit. It is the
layer every front end sits on: `orbit chat` renders one to a terminal, the
[JSONL bridge](APP_PROTOCOL.md) exposes one over a protocol, and a future
desktop or voice interface would consume the same thing.

Implementation: [`orbit-session`](../crates/session/). Observation:
[EVENTS.md](EVENTS.md).

## Lifecycle

```text
SessionRuntime::single_project(...)  ─┐
                                      ├─→ session_started
SessionRuntime::workspace(...)       ─┘
        ↓
  send_message()  ×N        → one turn each
  set_active_projects()     → active_projects_changed
  cancel_current_turn()     → execution_cancelled
  resolve_permission()      → permission_resolved
  clear()                   → forget the conversation, keep the session
        ↓
  end()                     → session_ended
```

Opening a session emits `session_started` carrying the protocol version,
the mode, the workspace name (in workspace mode), and the initially active
projects. Ending one shuts down any external MCP servers the session
started and emits `session_ended`.

## What a session remembers

For the lifetime of the process:

- session id, mode (single-project or workspace), and workspace identity;
- the active project or projects;
- the full conversation (system prompt, user messages, assistant
  messages, tool calls, and tool results);
- every action execution record (name, permission outcome, timing,
  success);
- every project-scoped source reference collected so far;
- configured-command executions specifically, so a front end can show
  what was run;
- pending permission requests;
- cancellation state and whether a turn is currently running.

**Nothing is written to disk.** Session state lives in process memory and
is discarded when the process exits. There is no transcript file, no
history database, and no cache — a conversation is never silently
persisted. Persistent memory is deliberately not implemented (see
[ARCHITECTURE.md](ARCHITECTURE.md#what-is-not-built-yet)).

## Turns

One `send_message` is one turn. Turns are serial: a session runs at most
one at a time, and the runtime holds its state lock for the duration.
Cancelling and resolving permissions deliberately do **not** take that
lock, so they work while a turn is in flight.

A turn:

1. emits `user_message_received`;
2. resolves scope (workspace mode — see below);
3. runs deterministic retrieval, if applicable;
4. runs the agent loop: model request → tool calls → model request → …;
5. emits `turn_completed`.

Ordering guarantees are specified in [EVENTS.md](EVENTS.md#ordering-guarantees).

## Project selection

In single-project mode the project is fixed at construction and cannot be
changed; `set_active_projects` returns an error.

In workspace mode a session starts with **no** active project — Orbit
never silently points a session at a repository. From there:

- **Explicit selection** (`/use obc`, or `projects.set` over the bridge)
  replaces the active set. Names are resolved through the workspace's
  deterministic `ProjectRegistry` (exact name → exact alias → normalized
  name → normalized alias, never fuzzy). An unknown or unavailable name is
  an error that leaves the previous selection untouched.
- **Naming a project in a message** adds it to the active set, so a
  follow-up broadens the conversation instead of being ignored:

  ```text
  > /use obc
  Active project(s): obc
  > Why was STM32 selected?
  • active project(s): obc
  > Now compare that with docs
  • active project(s): obc, docs
  ```

  This is the same deterministic exact-name/alias scan used everywhere
  else. The model never chooses which project a request touches.
- **Nothing selected and nothing named** falls back to the workspace's
  `defaults.project` for that turn only. It is reported (`used_default`
  in the turn outcome; "using the workspace default project" in the CLI)
  and does **not** become sticky — an overview question never silently
  pins the session to a repository.

## Permissions

An `ask` permission genuinely pauses the turn. The Action Runtime emits
`permission_required` and then awaits a decision; nothing proceeds until
one arrives. Three modes:

| Mode | Behavior | Used by |
|---|---|---|
| `AutoAllow` | Approve without asking | `orbit chat --yes`, bridge `"permissions":"allow_all"` |
| `AutoDeny` | Refuse without asking | non-interactive contexts, bridge `"permissions":"deny_all"` |
| `External` | Wait for `resolve_permission` | interactive `orbit chat`, bridge `"permissions":"external"` (default) |

Only `allow once` and `deny once` exist. There is no "always allow": a
persistent permission change belongs in `.orbit/project.yaml`, not in a
conversation.

Nothing times out. An unanswered request holds the turn until the client
answers or cancels — silently choosing for the user is exactly what an
`ask` permission exists to prevent.

Because `permission_required` is emitted *before* the runtime awaits the
decision, a fast client can answer before the wait begins; the session
keeps such early decisions rather than dropping them.

## Cancellation

`cancel_current_turn()` (CLI: `/cancel` or Ctrl-C; bridge:
`execution.cancel`) cancels the turn in flight. It:

- stops further model generation, including mid-stream — the provider's
  delta handler reports the cancellation and the stream is abandoned;
- stops further tool calls before they start;
- releases any pending permission request as cancelled;
- emits `execution_cancelled`;
- leaves the session usable for the next message.

It returns `false` when nothing is running, so a client can tell
"cancelled" from "nothing to cancel" instead of being told a no-op
succeeded.

**Completed work is preserved and never disowned.** Messages, sources, and
action records from before the cancellation stay in the session. Orbit
does not pretend that a file already written or a command already executed
was undone — it only stops what had not started yet. A cancelled turn's
answer contains exactly the text that was actually streamed, never more.

## Streaming

When the provider supports it and the session enables it, assistant text
arrives as `response_delta` events. Concatenating every delta of one model
response yields exactly that response's final text.

A provider that cannot stream still satisfies this contract: the default
`chat_streaming` implementation calls `chat` and reports the whole answer
as one delta. Front ends therefore need only one rendering path, and
`model_response_started.streaming` tells them whether text will arrive
incrementally.

Tool calling is unaffected by streaming.

## Session commands

Parsed by `orbit_session::parse`, in the session layer rather than in any
renderer — so every front end shares the vocabulary, and, more
importantly, **a command is never sent to the model**:

```text
/projects        list the projects registered in this workspace
/use <a[,b]>     set the active project(s)
/status          show session id, mode, active projects, counters
/sources         re-print the sources collected in this session
/cancel          cancel the turn currently running
/clear           forget the conversation, keep the session
/help            show this list
/exit            end the session
```

`exit` and `quit` without a slash also end the session. A mistyped command
(`/usse obc`) is reported as unknown — it is never quietly forwarded to
the model as if it were a question. A message that merely contains a slash
(`what is in docs/obc/architecture.md?`) is ordinary text.

## Concurrency

A session runs one turn at a time. `execution_id` exists in the event
model so that concurrent read-only actions can be correlated later without
a protocol change, but Orbit does not run actions concurrently today, and
this version deliberately does not introduce uncontrolled concurrency.

The bridge does run each turn on its own task, so `execution.cancel` and
`permission.resolve` can be processed while a turn is in flight.

## Topic state

A session tracks what the conversation is about, so a follow-up that does
not name its subject can still be retrieved on. See
[SEARCH.md](SEARCH.md#conversational-context-orbit-sessiontopic).

## Known limitations

- In-memory only: closing the process discards the conversation.
- One turn at a time per session.
- `set_active_projects` is workspace-only.
- Deterministic project routing is exact name/alias matching, not
  semantic — a message must actually contain a registered name or alias.
- `orbit ask` remains a one-shot command and does not create a session.
