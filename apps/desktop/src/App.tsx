/**
 * Orbit desktop.
 *
 * Two columns: the conversation, and an inspector showing what Orbit is
 * actually doing. Everything rendered here comes from a protocol frame —
 * there are no simulated "thinking" states, because a state nobody can
 * verify is worse than no state at all.
 */

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { describeError, orbit } from "./services/orbit";
import { initialState, reduce, sourceKey } from "./state/session";
import type { Frame } from "../src/protocol/frames";
import "./styles/app.css";

type Connection =
  | { status: "idle" }
  | { status: "starting" }
  | { status: "ready"; binary: string; version: number }
  | { status: "error"; message: string };

export default function App() {
  const [state, dispatch] = useReducer(reduce, initialState);
  const [connection, setConnection] = useState<Connection>({ status: "idle" });
  const [workspace, setWorkspace] = useState(
    () => localStorage.getItem("orbit.workspace") ?? "",
  );
  const [draft, setDraft] = useState("");
  const [diagnostics, setDiagnostics] = useState<string[]>([]);
  const [showActivity, setShowActivity] = useState(true);
  const transcript = useRef<HTMLDivElement>(null);

  // Frames, diagnostics, and exits arrive on three separate channels so
  // a log line can never be mistaken for protocol.
  useEffect(() => {
    const unlisten: Promise<() => void>[] = [
      orbit.onFrame((frame: Frame) => dispatch(frame)),
      orbit.onDiagnostic((line) => setDiagnostics((d) => [...d.slice(-200), line])),
      orbit.onExit((reason) => setConnection({ status: "error", message: reason })),
    ];
    return () => {
      unlisten.forEach((p) => p.then((off) => off()));
    };
  }, []);

  useEffect(() => {
    transcript.current?.scrollTo({ top: transcript.current.scrollHeight, behavior: "smooth" });
  }, [state.messages]);

  const chooseWorkspace = useCallback(async () => {
    const picked = await open({ directory: true, title: "Select an Orbit workspace" });
    if (typeof picked === "string") {
      setWorkspace(picked);
      localStorage.setItem("orbit.workspace", picked);
    }
  }, []);

  /** Start the backend and open a session. Validation is Orbit's, not ours. */
  const connect = useCallback(async () => {
    if (!workspace) return;
    setConnection({ status: "starting" });
    try {
      const started = await orbit.start(workspace);
      setConnection({
        status: "ready",
        binary: started.binary_path,
        version: started.protocol_version,
      });
      await orbit.send({ type: "session.start", workspace, permissions: "external" });
    } catch (error) {
      setConnection({ status: "error", message: describeError(error) });
    }
  }, [workspace]);

  const send = useCallback(async () => {
    const text = draft.trim();
    if (!text || !state.sessionId || state.busy) return;
    setDraft("");
    try {
      await orbit.send({ type: "message.send", session_id: state.sessionId, text });
    } catch (error) {
      setConnection({ status: "error", message: describeError(error) });
    }
  }, [draft, state.sessionId, state.busy]);

  /** Cancellation is a protocol message; the session stays alive. */
  const cancel = useCallback(async () => {
    if (!state.sessionId) return;
    await orbit.send({ type: "execution.cancel", session_id: state.sessionId }).catch(() => {});
  }, [state.sessionId]);

  const resolvePermission = useCallback(
    async (decision: "allow_once" | "deny_once") => {
      const pending = state.pendingPermission;
      if (!pending) return;
      await orbit.send({ type: "permission.resolve", request_id: pending.requestId, decision });
    },
    [state.pendingPermission],
  );

  const restart = useCallback(async () => {
    await orbit.stop();
    setConnection({ status: "idle" });
    await connect();
  }, [connect]);

  const visibleActivity = useMemo(
    () => (showActivity ? state.activity : state.activity.filter((a) => a.status !== "completed")),
    [state.activity, showActivity],
  );

  const canSend = Boolean(state.sessionId) && !state.busy;

  return (
    <div className="orbit">
      <header className="bar">
        <div className="brand">
          <span className="node" data-busy={state.busy} />
          <strong>Orbit</strong>
        </div>
        <div className="bar-meta">
          <span className={`chip status-${connection.status}`}>
            {connection.status === "ready"
              ? `protocol v${connection.version}`
              : connection.status}
          </span>
          {state.workspace && <span className="chip">{state.workspace}</span>}
          {state.activeProjects.length > 0 && (
            <span className="chip accent">{state.activeProjects.join(", ")}</span>
          )}
          {state.model && <span className="chip subtle">{state.model}</span>}
        </div>
        <div className="bar-actions">
          <button onClick={chooseWorkspace}>Workspace…</button>
          <button onClick={restart} disabled={connection.status === "starting"}>
            Restart
          </button>
        </div>
      </header>

      {connection.status !== "ready" && (
        <section className="setup">
          <h1>Connect to a workspace</h1>
          <p className="path">{workspace || "No workspace selected."}</p>
          {connection.status === "error" && (
            <pre className="failure" onClick={() => navigator.clipboard.writeText(connection.message)}>
              {connection.message}
              <span className="copy-hint">click to copy</span>
            </pre>
          )}
          <div className="setup-actions">
            <button onClick={chooseWorkspace}>Choose folder</button>
            <button className="primary" onClick={connect} disabled={!workspace}>
              {connection.status === "starting" ? "Starting…" : "Connect"}
            </button>
          </div>
        </section>
      )}

      {connection.status === "ready" && (
        <main className="columns">
          <section className="conversation">
            <div className="transcript" ref={transcript}>
              {state.messages.length === 0 && (
                <p className="empty">
                  Ask about this workspace. Every answer is grounded in files Orbit really read.
                </p>
              )}
              {state.messages.map((message) => (
                <article key={message.id} className={`turn ${message.role}`} data-status={message.status}>
                  <div className="who">{message.role === "user" ? "You" : "Orbit"}</div>
                  <div className="body">
                    {message.text}
                    {message.status === "streaming" && <span className="caret" />}
                  </div>
                  {message.status === "cancelled" && <div className="note">cancelled</div>}
                  {message.status === "failed" && <div className="note error">failed</div>}
                </article>
              ))}
            </div>

            <div className="composer">
              <textarea
                value={draft}
                placeholder={canSend ? "Ask Orbit…" : "Waiting for Orbit…"}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    void send();
                  }
                  if (e.key === "Escape" && state.busy) void cancel();
                }}
                rows={3}
              />
              <div className="composer-actions">
                {state.busy ? (
                  <button className="danger" onClick={cancel}>
                    Cancel task
                  </button>
                ) : (
                  <button className="primary" onClick={send} disabled={!canSend || !draft.trim()}>
                    Send
                  </button>
                )}
              </div>
            </div>
          </section>

          <aside className="inspector">
            <div className="panel">
              <h2>
                Activity
                <button className="link" onClick={() => setShowActivity((v) => !v)}>
                  {showActivity ? "collapse" : "expand"}
                </button>
              </h2>
              {visibleActivity.length === 0 && <p className="empty">Idle.</p>}
              <ul className="timeline">
                {visibleActivity.map((item) => (
                  <li key={item.id} data-status={item.status}>
                    <span className="dot" />
                    <div>
                      <div className="label">{item.label}</div>
                      {item.detail && <div className="detail">{item.detail}</div>}
                    </div>
                    {item.durationMs !== undefined && (
                      <span className="ms">{item.durationMs}ms</span>
                    )}
                  </li>
                ))}
              </ul>
            </div>

            <div className="panel">
              <h2>Sources <span className="count">{state.sources.length}</span></h2>
              {state.sources.length === 0 && <p className="empty">No sources yet.</p>}
              <ul className="sources">
                {state.sources.map((source) => (
                  <li key={sourceKey(source)}>
                    {source.project && <span className="proj">{source.project}</span>}
                    <span className="path">{source.path}</span>
                    {source.lineStart !== undefined && (
                      <span className="lines">
                        :{source.lineStart}
                        {source.lineEnd !== undefined && source.lineEnd !== source.lineStart
                          ? `-${source.lineEnd}`
                          : ""}
                      </span>
                    )}
                    {source.section && <span className="section">{source.section}</span>}
                  </li>
                ))}
              </ul>
            </div>

            {state.errors.length > 0 && (
              <div className="panel">
                <h2>Warnings</h2>
                <ul className="warnings">
                  {state.errors.slice(-5).map((error, index) => (
                    <li key={index}>{error}</li>
                  ))}
                </ul>
              </div>
            )}

            <details className="panel diagnostics">
              <summary>Backend log ({diagnostics.length})</summary>
              <pre onClick={() => navigator.clipboard.writeText(diagnostics.join("\n"))}>
                {diagnostics.slice(-40).join("\n") || "—"}
              </pre>
            </details>
          </aside>
        </main>
      )}

      {state.pendingPermission && (
        <div className="sheet-backdrop">
          <div className="sheet">
            <h2>Orbit needs permission</h2>
            <dl>
              <dt>Action</dt>
              <dd>{state.pendingPermission.action}</dd>
              {state.pendingPermission.project && (
                <>
                  <dt>Project</dt>
                  <dd>{state.pendingPermission.project}</dd>
                </>
              )}
              {state.pendingPermission.description && (
                <>
                  <dt>Description</dt>
                  <dd>{state.pendingPermission.description}</dd>
                </>
              )}
              {state.pendingPermission.arguments && (
                <>
                  <dt>Arguments</dt>
                  <dd><code>{state.pendingPermission.arguments}</code></dd>
                </>
              )}
              <dt>Request</dt>
              <dd className="subtle">{state.pendingPermission.requestId}</dd>
            </dl>
            {/* Dismissing is never approval: there is no close button. */}
            <div className="sheet-actions">
              <button onClick={() => resolvePermission("deny_once")}>Deny once</button>
              <button className="primary" onClick={() => resolvePermission("allow_once")}>
                Allow once
              </button>
              <button className="danger" onClick={cancel}>Cancel task</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
