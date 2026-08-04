# Orbit desktop

A Tauri 2 + React desktop client for Orbit. It is a **renderer**, not a
second implementation: every answer, source, and action comes from the
real Orbit backend over the JSON Lines protocol in
[`docs/APP_PROTOCOL.md`](../../docs/APP_PROTOCOL.md).

## Status

This is the first vertical slice, and it is real end to end — no mock
data, no simulated states.

**Working:** workspace selection, backend startup with protocol-version
validation, session start, sending a message, streamed `response_delta`
rendering, a live activity timeline grouped by `execution_id`,
deduplicated sources, the permission sheet, and cancellation that keeps
the session alive.

**Not built yet:** voice input and spoken responses. The composer has no
microphone button because there is no speech provider behind it; adding
one that did nothing would be worse than its absence. See
[`docs/VOICE.md`](../../docs/VOICE.md) for the intended shape.

## Running it

```bash
# 1. Build the backend the app drives.
cargo build --release

# 2. Install frontend dependencies.
cd apps/desktop && pnpm install

# 3. Run the app.
pnpm tauri dev
```

Then choose a workspace directory — one containing
`.orbit/workspace.yaml`, e.g. `/Users/you/orbit-lab` — and press
**Connect**.

The binary is discovered automatically: next to the app executable in a
packaged build, or at `target/release/orbit` in this repository during
development.

## Checks

```bash
pnpm typecheck && pnpm test && pnpm build   # frontend
cd src-tauri && cargo fmt --check && cargo clippy --all-targets && cargo test
```

`cargo test` in `src-tauri` includes `tests/real_backend.rs`, which
spawns the actual Orbit binary and asserts the handshake. It skips
cleanly when no release build exists.

## Layout

```text
src/
├── protocol/frames.ts   wire types, kept in sync with APP_PROTOCOL.md
├── state/session.ts     the reducer: frames in, conversation out
├── services/orbit.ts    the only path to the backend (Tauri invoke)
├── styles/app.css
└── App.tsx              two-column shell
src-tauri/
├── src/sidecar.rs       process manager: spawn, handshake, frame routing
└── src/lib.rs           the four commands the webview may call
```

## Security

The webview has no filesystem, no shell, and no HTTP capability. Its
Tauri capability set is `core:default`, `opener:default`, and
`dialog:allow-open` — a directory picker and nothing else.

It cannot name a program: `start_backend` takes a workspace directory,
and the argument list (`app serve --jsonl`) is fixed in Rust. It cannot
send an arbitrary protocol message either — `send_command` allow-lists
the eight request types this app uses, so `command.run_configured` and
anything else are refused before reaching stdin.

Project access happens only through Orbit's own permission-checked
actions.
