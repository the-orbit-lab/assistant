/**
 * Conversation state, derived from protocol frames only.
 *
 * The rule from `docs/APP_PROTOCOL.md`: model the conversation from
 * events, not from the answer text. Deltas reconstruct the final text
 * exactly, so the streamed view never has to be reconciled with a
 * different final value, and a citation is never derived from prose —
 * only from a `source_found` an action really produced.
 *
 * This is a pure reducer so it can be tested without a backend, a
 * webview, or a model.
 */

import type { Frame } from "../protocol/frames";

export type TurnStatus = "streaming" | "complete" | "cancelled" | "failed";

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  text: string;
  status: TurnStatus;
}

export type ActivityStatus =
  | "queued"
  | "active"
  | "completed"
  | "failed"
  | "cancelled"
  | "waiting_for_permission";

export interface ActivityItem {
  /** `execution_id` where one exists; a synthetic id for phase rows. */
  id: string;
  turnId?: string;
  label: string;
  detail?: string;
  project?: string;
  status: ActivityStatus;
  durationMs?: number;
}

export interface SourceItem {
  project?: string;
  path: string;
  lineStart?: number;
  lineEnd?: number;
  section?: string;
}

export interface PermissionRequest {
  requestId: string;
  executionId: string;
  action: string;
  project?: string;
  description?: string;
  arguments?: string;
}

export interface SessionState {
  sessionId?: string;
  mode?: "single_project" | "workspace";
  workspace?: string;
  activeProjects: string[];
  availableProjects: { name: string; available: boolean }[];
  messages: ChatMessage[];
  activity: ActivityItem[];
  sources: SourceItem[];
  pendingPermission?: PermissionRequest;
  /** A turn is running: cancellation is meaningful, sending is not. */
  busy: boolean;
  model?: string;
  errors: string[];
  /** Frames this client does not model, for diagnostics. */
  unknownFrames: number;
}

export const initialState: SessionState = {
  activeProjects: [],
  availableProjects: [],
  messages: [],
  activity: [],
  sources: [],
  busy: false,
  errors: [],
  unknownFrames: 0,
};

/**
 * Identity of a citation.
 *
 * `project + path + line_start + line_end + section`, exactly as the
 * backend's own deduplication defines it, so the two agree.
 */
export function sourceKey(source: SourceItem): string {
  return [
    source.project ?? "",
    source.path,
    source.lineStart ?? "",
    source.lineEnd ?? "",
    source.section ?? "",
  ].join("|");
}

/**
 * Add a source, dropping duplicates and path-only references that a
 * precise one supersedes.
 *
 * A whole-file citation next to the exact lines that were quoted is
 * noise: the precise one is what the answer rests on.
 */
export function mergeSource(sources: SourceItem[], incoming: SourceItem): SourceItem[] {
  if (sources.some((s) => sourceKey(s) === sourceKey(incoming))) {
    return sources;
  }
  const samePath = (s: SourceItem) => s.path === incoming.path && s.project === incoming.project;

  if (incoming.lineStart === undefined) {
    // A path-only citation is redundant once precise lines exist.
    if (sources.some((s) => samePath(s) && s.lineStart !== undefined)) {
      return sources;
    }
    return [...sources, incoming];
  }
  // A precise citation supersedes the path-only one for the same file.
  return [...sources.filter((s) => !(samePath(s) && s.lineStart === undefined)), incoming];
}

function upsertActivity(activity: ActivityItem[], item: ActivityItem): ActivityItem[] {
  const index = activity.findIndex((a) => a.id === item.id);
  if (index === -1) return [...activity, item];
  const next = [...activity];
  next[index] = { ...next[index], ...item };
  return next;
}

/** A short, human label for an action name. */
function actionLabel(action: string, project?: string | null): string {
  const scope = project ? `${project}: ` : "";
  if (action.endsWith("search")) return `${scope}Searching`;
  if (action.endsWith("read_file")) return `${scope}Reading`;
  if (action.endsWith("list_files")) return `${scope}Listing files`;
  if (action.endsWith("information")) return `${scope}Reading project info`;
  if (action.endsWith("run_configured")) return `${scope}Running command`;
  return `${scope}${action}`;
}

/**
 * Apply one frame.
 *
 * Unrecognized frames increment a counter and change nothing else: the
 * protocol says adding an event is backwards-compatible, so a client
 * that crashed on one would break on a backend upgrade.
 */
export function reduce(state: SessionState, frame: Frame): SessionState {
  switch (frame.type) {
    case "session_started": {
      const f = frame as import("../protocol/frames").SessionStartedFrame;
      return {
        ...state,
        sessionId: f.session_id,
        mode: f.mode,
        workspace: f.workspace ?? undefined,
        activeProjects: f.projects ?? [],
      };
    }

    case "active_projects_changed":
      return { ...state, activeProjects: (frame as any).projects ?? [] };

    case "projects":
      return {
        ...state,
        availableProjects: ((frame as any).projects ?? []).map((p: any) => ({
          name: p.name,
          available: p.available,
        })),
      };

    case "user_message_received": {
      const f = frame as import("../protocol/frames").UserMessageReceivedFrame;
      return {
        ...state,
        busy: true,
        activity: [],
        messages: [
          ...state.messages,
          { id: `${f.turn_id}-user`, role: "user", text: f.text, status: "complete" },
        ],
      };
    }

    case "retrieval_started":
      return {
        ...state,
        activity: upsertActivity(state.activity, {
          id: `${frame.turn_id}-retrieval`,
          turnId: frame.turn_id,
          label: "Gathering context",
          detail: ((frame as any).scope ?? []).join(", "),
          status: "active",
        }),
      };

    case "retrieval_completed":
      return {
        ...state,
        activity: upsertActivity(state.activity, {
          id: `${frame.turn_id}-retrieval`,
          turnId: frame.turn_id,
          label: "Gathered context",
          detail: `${(frame as any).action_count} actions, ${(frame as any).source_count} sources`,
          status: "completed",
        }),
      };

    case "action_requested":
    case "action_started": {
      const f = frame as import("../protocol/frames").ActionStartedFrame;
      return {
        ...state,
        activity: upsertActivity(state.activity, {
          id: f.execution_id,
          turnId: f.turn_id,
          label: actionLabel(f.action, f.project),
          project: f.project ?? undefined,
          detail: (frame as any).arguments ?? undefined,
          status: frame.type === "action_started" ? "active" : "queued",
        }),
      };
    }

    case "action_completed": {
      const f = frame as import("../protocol/frames").ActionCompletedFrame;
      return {
        ...state,
        activity: upsertActivity(state.activity, {
          id: f.execution_id,
          label: actionLabel(f.action, f.project),
          status: "completed",
          durationMs: f.duration_ms,
        }),
      };
    }

    case "action_failed": {
      const f = frame as import("../protocol/frames").ActionFailedFrame;
      return {
        ...state,
        activity: upsertActivity(state.activity, {
          id: f.execution_id,
          label: actionLabel(f.action, f.project),
          detail: f.error,
          status: "failed",
        }),
      };
    }

    case "permission_required": {
      const f = frame as import("../protocol/frames").PermissionRequiredFrame;
      return {
        ...state,
        pendingPermission: {
          requestId: f.request_id,
          executionId: f.execution_id,
          action: f.action,
          project: f.project ?? undefined,
          description: f.description ?? undefined,
          arguments: f.arguments ?? undefined,
        },
        activity: upsertActivity(state.activity, {
          id: f.execution_id,
          label: actionLabel(f.action, f.project),
          status: "waiting_for_permission",
        }),
      };
    }

    case "permission_resolved":
      return { ...state, pendingPermission: undefined };

    case "source_found": {
      const f = frame as import("../protocol/frames").SourceFoundFrame;
      return {
        ...state,
        sources: mergeSource(state.sources, {
          project: f.project ?? undefined,
          path: f.path,
          lineStart: f.line_start ?? undefined,
          lineEnd: f.line_end ?? undefined,
          section: f.section ?? undefined,
        }),
      };
    }

    case "model_response_started": {
      const f = frame as import("../protocol/frames").ModelResponseStartedFrame;
      return {
        ...state,
        model: f.model,
        messages: [
          ...state.messages,
          { id: `${f.turn_id}-assistant`, role: "assistant", text: "", status: "streaming" },
        ],
      };
    }

    case "response_delta": {
      const f = frame as import("../protocol/frames").ResponseDeltaFrame;
      const id = `${f.turn_id}-assistant`;
      // A delta may arrive before `model_response_started` was seen;
      // opening the bubble here keeps text from being lost.
      if (!state.messages.some((m) => m.id === id)) {
        return {
          ...state,
          messages: [
            ...state.messages,
            { id, role: "assistant", text: f.text, status: "streaming" },
          ],
        };
      }
      return {
        ...state,
        messages: state.messages.map((m) =>
          m.id === id ? { ...m, text: m.text + f.text } : m,
        ),
      };
    }

    case "model_response_completed": {
      const f = frame as import("../protocol/frames").ModelResponseCompletedFrame;
      const id = `${f.turn_id}-assistant`;
      return {
        ...state,
        messages: state.messages.map((m) =>
          m.id === id ? { ...m, text: f.text, status: "complete" } : m,
        ),
      };
    }

    case "execution_cancelled":
      // Whatever text already arrived is kept: cancellation stops the
      // work, it does not claim the work never happened.
      return {
        ...state,
        busy: false,
        pendingPermission: undefined,
        messages: state.messages.map((m) =>
          m.status === "streaming" ? { ...m, status: "cancelled" } : m,
        ),
        activity: state.activity.map((a) =>
          a.status === "active" || a.status === "waiting_for_permission"
            ? { ...a, status: "cancelled" }
            : a,
        ),
      };

    case "turn_completed":
      return { ...state, busy: false };

    case "failure":
      return {
        ...state,
        busy: false,
        errors: [...state.errors, (frame as any).message],
        messages: state.messages.map((m) =>
          m.status === "streaming" ? { ...m, status: "failed" } : m,
        ),
      };

    case "warning":
      return { ...state, errors: [...state.errors, (frame as any).message] };

    case "error":
      return { ...state, errors: [...state.errors, `${(frame as any).code}: ${(frame as any).message}`] };

    case "session_ended":
      return { ...state, busy: false, sessionId: undefined };

    // Acks and the ready frame carry no conversation state.
    case "ack":
    case "ready":
      return state;

    default:
      return { ...state, unknownFrames: state.unknownFrames + 1 };
  }
}
