/**
 * The wire types of `orbit app serve --jsonl`.
 *
 * Kept in explicit sync with `docs/APP_PROTOCOL.md` and
 * `crates/cli/src/app/protocol.rs`. Field names are the protocol's, never
 * invented: a renamed field is a protocol version bump, and this file is
 * where that shows up.
 *
 * Frames are modelled as a discriminated union on `type` with an
 * `UnknownFrame` fallback, because the protocol says adding an event is
 * backwards-compatible — an unrecognized frame must be ignorable, not a
 * crash.
 */

/** Incremented when a field is renamed or removed. See APP_PROTOCOL.md. */
export const SUPPORTED_PROTOCOL_VERSION = 1;

export interface FrameBase {
  type: string;
  session_id?: string;
  turn_id?: string;
  execution_id?: string;
  timestamp_ms?: number;
}

export interface ReadyFrame extends FrameBase {
  type: "ready";
  protocol_version: number;
}

export interface SessionStartedFrame extends FrameBase {
  type: "session_started";
  session_id: string;
  protocol_version: number;
  mode: "single_project" | "workspace";
  workspace?: string | null;
  projects: string[];
}

export interface UserMessageReceivedFrame extends FrameBase {
  type: "user_message_received";
  turn_id: string;
  text: string;
}

export interface ActiveProjectsChangedFrame extends FrameBase {
  type: "active_projects_changed";
  projects: string[];
}

export interface RetrievalStartedFrame extends FrameBase {
  type: "retrieval_started";
  scope: string[];
}

export interface RetrievalCompletedFrame extends FrameBase {
  type: "retrieval_completed";
  scope: string[];
  action_count: number;
  source_count: number;
}

export interface ActionRequestedFrame extends FrameBase {
  type: "action_requested";
  execution_id: string;
  action: string;
  project?: string | null;
  arguments?: string | null;
}

export interface ActionStartedFrame extends FrameBase {
  type: "action_started";
  execution_id: string;
  action: string;
  project?: string | null;
}

export interface ActionCompletedFrame extends FrameBase {
  type: "action_completed";
  execution_id: string;
  action: string;
  project?: string | null;
  duration_ms: number;
  source_count: number;
}

export interface ActionFailedFrame extends FrameBase {
  type: "action_failed";
  execution_id: string;
  action: string;
  project?: string | null;
  error: string;
}

export interface PermissionRequiredFrame extends FrameBase {
  type: "permission_required";
  execution_id: string;
  request_id: string;
  action: string;
  project?: string | null;
  description?: string | null;
  arguments?: string | null;
}

export interface PermissionResolvedFrame extends FrameBase {
  type: "permission_resolved";
  execution_id: string;
  request_id: string;
  decision: "allow_once" | "deny_once";
  project?: string | null;
}

export interface SourceFoundFrame extends FrameBase {
  type: "source_found";
  path: string;
  project?: string | null;
  line_start?: number | null;
  line_end?: number | null;
  section?: string | null;
}

export interface ModelResponseStartedFrame extends FrameBase {
  type: "model_response_started";
  turn_id: string;
  model: string;
  streaming: boolean;
}

export interface ResponseDeltaFrame extends FrameBase {
  type: "response_delta";
  turn_id: string;
  text: string;
}

export interface ModelResponseCompletedFrame extends FrameBase {
  type: "model_response_completed";
  turn_id: string;
  text: string;
}

export interface ExecutionCancelledFrame extends FrameBase {
  type: "execution_cancelled";
  reason: string;
}

export interface TurnCompletedFrame extends FrameBase {
  type: "turn_completed";
  turn_id: string;
  source_count: number;
  action_count: number;
}

export interface SessionEndedFrame extends FrameBase {
  type: "session_ended";
  reason: string;
}

export interface WarningFrame extends FrameBase {
  type: "warning";
  message: string;
}

export interface FailureFrame extends FrameBase {
  type: "failure";
  message: string;
}

/** Non-event frames: acks, errors, and query replies. */
export interface AckFrame extends FrameBase {
  type: "ack";
  request: string;
}

export interface ErrorFrame extends FrameBase {
  type: "error";
  code: string;
  message: string;
}

export interface ProjectsFrame extends FrameBase {
  type: "projects";
  projects: { name: string; available: boolean; aliases: string[] }[];
}

/** Any frame this client does not model. Ignored, never fatal. */
export interface UnknownFrame extends FrameBase {
  [key: string]: unknown;
}

export type Frame =
  | ReadyFrame
  | SessionStartedFrame
  | UserMessageReceivedFrame
  | ActiveProjectsChangedFrame
  | RetrievalStartedFrame
  | RetrievalCompletedFrame
  | ActionRequestedFrame
  | ActionStartedFrame
  | ActionCompletedFrame
  | ActionFailedFrame
  | PermissionRequiredFrame
  | PermissionResolvedFrame
  | SourceFoundFrame
  | ModelResponseStartedFrame
  | ResponseDeltaFrame
  | ModelResponseCompletedFrame
  | ExecutionCancelledFrame
  | TurnCompletedFrame
  | SessionEndedFrame
  | WarningFrame
  | FailureFrame
  | AckFrame
  | ErrorFrame
  | ProjectsFrame
  | UnknownFrame;

/** Commands this client sends. The Rust layer allow-lists these too. */
export type Command =
  | { type: "session.start"; workspace?: string; project?: string; streaming?: boolean; permissions?: "external" | "allow_all" | "deny_all" }
  | { type: "message.send"; session_id: string; text: string }
  | { type: "permission.resolve"; request_id: string; decision: "allow_once" | "deny_once" }
  | { type: "execution.cancel"; session_id: string; execution_id?: string }
  | { type: "projects.set"; session_id: string; projects: string[] }
  | { type: "projects.list"; session_id: string }
  | { type: "session.status"; session_id: string }
  | { type: "session.end"; session_id: string };
