import { describe, expect, it } from "vitest";
import { initialState, mergeSource, reduce, type SessionState } from "./session";
import type { Frame } from "../protocol/frames";

const apply = (frames: Frame[], from: SessionState = initialState) =>
  frames.reduce(reduce, from);

const started: Frame = {
  type: "session_started",
  session_id: "sess-1",
  protocol_version: 1,
  mode: "workspace",
  workspace: "Orbit Lab",
  projects: [],
};

describe("session reducer", () => {
  it("takes the session id and mode from session_started", () => {
    const s = apply([started]);
    expect(s.sessionId).toBe("sess-1");
    expect(s.mode).toBe("workspace");
    expect(s.workspace).toBe("Orbit Lab");
  });

  it("tracks active projects from the backend, not from the click", () => {
    const s = apply([started, { type: "active_projects_changed", projects: ["assistant"] } as Frame]);
    expect(s.activeProjects).toEqual(["assistant"]);
  });

  it("accumulates response deltas into one assistant message", () => {
    const s = apply([
      started,
      { type: "user_message_received", turn_id: "turn-1", text: "hi" } as Frame,
      { type: "model_response_started", turn_id: "turn-1", model: "qwen", streaming: true } as Frame,
      { type: "response_delta", turn_id: "turn-1", text: "Session" } as Frame,
      { type: "response_delta", turn_id: "turn-1", text: "Runtime" } as Frame,
    ]);
    const assistant = s.messages.find((m) => m.role === "assistant")!;
    expect(assistant.text).toBe("SessionRuntime");
    expect(assistant.status).toBe("streaming");
  });

  it("associates deltas with their own turn", () => {
    const s = apply([
      started,
      { type: "model_response_started", turn_id: "turn-1", model: "m", streaming: true } as Frame,
      { type: "response_delta", turn_id: "turn-1", text: "one" } as Frame,
      { type: "model_response_started", turn_id: "turn-2", model: "m", streaming: true } as Frame,
      { type: "response_delta", turn_id: "turn-2", text: "two" } as Frame,
    ]);
    expect(s.messages.map((m) => m.text)).toEqual(["one", "two"]);
  });

  it("finalizes on model_response_completed", () => {
    const s = apply([
      started,
      { type: "model_response_started", turn_id: "turn-1", model: "m", streaming: true } as Frame,
      { type: "response_delta", turn_id: "turn-1", text: "partial" } as Frame,
      { type: "model_response_completed", turn_id: "turn-1", text: "the whole answer" } as Frame,
    ]);
    const assistant = s.messages.at(-1)!;
    expect(assistant.text).toBe("the whole answer");
    expect(assistant.status).toBe("complete");
  });

  it("groups action lifecycle by execution_id", () => {
    const s = apply([
      started,
      { type: "action_started", turn_id: "t1", execution_id: "exec-1", action: "workspace.search" } as Frame,
      { type: "action_completed", turn_id: "t1", execution_id: "exec-1", action: "workspace.search", duration_ms: 39, source_count: 2 } as Frame,
    ]);
    expect(s.activity).toHaveLength(1);
    expect(s.activity[0].status).toBe("completed");
    expect(s.activity[0].durationMs).toBe(39);
  });

  it("marks a failed action rather than leaving it spinning", () => {
    const s = apply([
      started,
      { type: "action_started", turn_id: "t1", execution_id: "exec-1", action: "project.read_file" } as Frame,
      { type: "action_failed", turn_id: "t1", execution_id: "exec-1", action: "project.read_file", error: "denied" } as Frame,
    ]);
    expect(s.activity[0].status).toBe("failed");
    expect(s.activity[0].detail).toBe("denied");
  });

  it("opens and clears a permission request", () => {
    const asked = apply([
      started,
      { type: "permission_required", turn_id: "t1", execution_id: "exec-4", request_id: "perm-1", action: "command.run_configured", project: "obc", arguments: "name=test" } as Frame,
    ]);
    expect(asked.pendingPermission?.requestId).toBe("perm-1");
    expect(asked.activity[0].status).toBe("waiting_for_permission");

    const resolved = reduce(asked, { type: "permission_resolved", turn_id: "t1", execution_id: "exec-4", request_id: "perm-1", decision: "deny_once" } as Frame);
    expect(resolved.pendingPermission).toBeUndefined();
  });

  it("keeps the text already streamed when a turn is cancelled", () => {
    const s = apply([
      started,
      { type: "model_response_started", turn_id: "turn-1", model: "m", streaming: true } as Frame,
      { type: "response_delta", turn_id: "turn-1", text: "Brownout recovery" } as Frame,
      { type: "execution_cancelled", turn_id: "turn-1", reason: "cancelled by user" } as Frame,
    ]);
    const assistant = s.messages.at(-1)!;
    expect(assistant.text).toBe("Brownout recovery");
    expect(assistant.status).toBe("cancelled");
    expect(s.busy).toBe(false);
    // The session survives: cancellation is a message, not a kill.
    expect(s.sessionId).toBe("sess-1");
  });

  it("is busy from the user message until the turn completes", () => {
    const sending = apply([started, { type: "user_message_received", turn_id: "t1", text: "hi" } as Frame]);
    expect(sending.busy).toBe(true);
    const done = reduce(sending, { type: "turn_completed", turn_id: "t1", source_count: 0, action_count: 0 } as Frame);
    expect(done.busy).toBe(false);
  });

  it("ignores a frame it does not model instead of crashing", () => {
    const s = reduce(initialState, { type: "some_future_event", detail: 1 } as Frame);
    expect(s.unknownFrames).toBe(1);
    expect(s.messages).toEqual([]);
  });
});

describe("source deduplication", () => {
  const precise = { project: "docs", path: "a.md", lineStart: 1, lineEnd: 9 };

  it("drops an exact duplicate", () => {
    expect(mergeSource([precise], { ...precise })).toHaveLength(1);
  });

  it("keeps distinct line ranges of one file", () => {
    expect(mergeSource([precise], { ...precise, lineStart: 20, lineEnd: 30 })).toHaveLength(2);
  });

  it("distinguishes the same path in different projects", () => {
    expect(mergeSource([precise], { ...precise, project: "obc" })).toHaveLength(2);
  });

  it("hides a path-only citation once precise lines exist", () => {
    const merged = mergeSource([precise], { project: "docs", path: "a.md" });
    expect(merged).toHaveLength(1);
    expect(merged[0].lineStart).toBe(1);
  });

  it("replaces a path-only citation when precise lines arrive", () => {
    const merged = mergeSource([{ project: "docs", path: "a.md" }], precise);
    expect(merged).toHaveLength(1);
    expect(merged[0].lineStart).toBe(1);
  });

  it("distinguishes sections", () => {
    expect(mergeSource([precise], { ...precise, section: "Cancellation" })).toHaveLength(2);
  });

  it("collects sources only from source_found frames", () => {
    const s = apply([
      started,
      { type: "source_found", turn_id: "t1", path: "docs/SESSIONS.md", project: "assistant", line_start: 32, line_end: 53 } as Frame,
      { type: "source_found", turn_id: "t1", path: "docs/SESSIONS.md", project: "assistant" } as Frame,
    ]);
    expect(s.sources).toHaveLength(1);
    expect(s.sources[0].lineStart).toBe(32);
  });
});
