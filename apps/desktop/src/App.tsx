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
import { MarkdownMessage } from "./components/MarkdownMessage";
import { SourceCitation } from "./components/SourceCitation";
import { macOsTts, unconfiguredStt } from "./features/voice/providers";
import { SentenceBuffer } from "./features/voice/sentences";
import { canStopSpeech, isCapturing, VOICE_LABELS, type VoiceState } from "./features/voice/state";
import { initialState, reduce, sourceKey, type SourceItem } from "./state/session";
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
  const [voice, setVoice] = useState<VoiceState>("idle");
  const [voiceNote, setVoiceNote] = useState<string>();
  const [sttReady, setSttReady] = useState(false);
  const [ttsReady, setTtsReady] = useState(false);
  const [selectedSource, setSelectedSource] = useState<SourceItem | null>(null);
  const transcript = useRef<HTMLDivElement>(null);
  // Deltas become whole sentences before they are spoken, so speech
  // starts early without reading fragments aloud.
  const speechBuffer = useRef(new SentenceBuffer());
  const spokenTurn = useRef<string | undefined>(undefined);

  useEffect(() => {
    void unconfiguredStt.isAvailable().then(setSttReady);
    void macOsTts.isAvailable().then(setTtsReady);
  }, []);

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

  // Speak the assistant's prose as it arrives. Only the newest streaming
  // message is spoken, and only whole sentences.
  const latest = state.messages.at(-1);
  useEffect(() => {
    if (!ttsReady || !latest || latest.role !== "assistant") return;

    if (spokenTurn.current !== latest.id) {
      speechBuffer.current.clear();
      spokenTurn.current = latest.id;
    }
    if (latest.status === "streaming") {
      const already = speechBuffer.current.spokenLength ?? 0;
      const fresh = latest.text.slice(already);
      if (!fresh) return;
      speechBuffer.current.spokenLength = latest.text.length;
      for (const sentence of speechBuffer.current.push(fresh)) {
        setVoice("speaking");
        void macOsTts.speak(sentence).catch(() => setVoice("error"));
      }
    } else if (latest.status === "complete") {
      for (const sentence of speechBuffer.current.flush()) {
        setVoice("speaking");
        void macOsTts.speak(sentence).catch(() => setVoice("error"));
      }
    }
  }, [latest?.id, latest?.text, latest?.status, ttsReady]);

  // The indicator must reflect the synthesizer, not our intent.
  useEffect(() => {
    if (voice !== "speaking") return;
    const timer = window.setInterval(() => {
      void macOsTts.isSpeaking().then((busy) => {
        if (!busy) setVoice("idle");
      });
    }, 400);
    return () => window.clearInterval(timer);
  }, [voice]);

  const stopSpeaking = useCallback(() => {
    speechBuffer.current.clear();
    void macOsTts.stop();
    setVoice("stopped");
    window.setTimeout(() => setVoice("idle"), 900);
  }, []);

  /** Push-to-talk. No provider is configured, so this explains itself. */
  const startListening = useCallback(async () => {
    setVoiceNote(undefined);
    if (!sttReady) {
      setVoice("error");
      setVoiceNote(unconfiguredStt.setupHint);
      return;
    }
    setVoice("requesting_permission");
    try {
      await unconfiguredStt.start();
      setVoice("listening");
    } catch (error) {
      setVoice("error");
      setVoiceNote(error instanceof Error ? error.message : String(error));
    }
  }, [sttReady]);

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
    // Cancelling the task silences the answer being read aloud too.
    speechBuffer.current.clear();
    void macOsTts.stop();
    setVoice("idle");
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
                    {message.role === "assistant" ? (
                      <MarkdownMessage text={message.text} />
                    ) : (
                      message.text
                    )}
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
                <div className="voice">
                  <button
                    className={`mic ${isCapturing(voice) ? "capturing" : ""}`}
                    onPointerDown={startListening}
                    onPointerUp={() => isCapturing(voice) && setVoice("transcribing")}
                    title={sttReady ? "Hold to talk" : unconfiguredStt.setupHint}
                    aria-label="Push to talk"
                  >
                    <span className="mic-glyph" />
                    {isCapturing(voice) && <span className="wave" aria-hidden />}
                  </button>
                  <span className={`voice-state state-${voice}`}>
                    {voice === "idle" && !sttReady ? "Voice output only" : VOICE_LABELS[voice]}
                  </span>
                  {canStopSpeech(voice) && (
                    <button className="link stop-voice" onClick={stopSpeaking}>
                      Stop voice
                    </button>
                  )}
                </div>
                <div className="send-actions">
                  {state.busy && (
                    <button className="danger" onClick={cancel}>
                      Cancel task
                    </button>
                  )}
                  <button className="primary" onClick={send} disabled={!canSend || !draft.trim()}>
                    Send
                  </button>
                </div>
              </div>
              {voiceNote && (
                <p className="voice-note" onClick={() => setVoiceNote(undefined)}>
                  {voiceNote}
                </p>
              )}
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
                  <SourceCitation
                    key={sourceKey(source)}
                    source={source}
                    onSelect={setSelectedSource}
                  />
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

      {selectedSource && (
        <div className="sheet-backdrop" onClick={() => setSelectedSource(null)}>
          <div className="sheet" onClick={(e) => e.stopPropagation()}>
            <h2>Source</h2>
            <dl>
              {selectedSource.project && (
                <>
                  <dt>Project</dt>
                  <dd>{selectedSource.project}</dd>
                </>
              )}
              <dt>Path</dt>
              {/* Project-relative, as Orbit reports it: an absolute path
                  would put a private directory layout on screen. */}
              <dd><code>{selectedSource.path}</code></dd>
              {selectedSource.lineStart !== undefined && (
                <>
                  <dt>Lines</dt>
                  <dd>
                    {selectedSource.lineStart}
                    {selectedSource.lineEnd !== undefined &&
                    selectedSource.lineEnd !== selectedSource.lineStart
                      ? `–${selectedSource.lineEnd}`
                      : ""}
                  </dd>
                </>
              )}
              {selectedSource.section && (
                <>
                  <dt>Section</dt>
                  <dd>{selectedSource.section}</dd>
                </>
              )}
            </dl>
            <div className="sheet-actions">
              <button onClick={() => setSelectedSource(null)}>Close</button>
            </div>
          </div>
        </div>
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
