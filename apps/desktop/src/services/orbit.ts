/**
 * The only path from the UI to Orbit.
 *
 * Everything here is a Tauri `invoke` into the Rust layer, which owns
 * the process, the binary path, and the argument list. The webview has
 * no shell, no filesystem, and no way to name a program.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Command, Frame } from "../protocol/frames";

export interface StartedBackend {
  protocol_version: number;
  binary_path: string;
  workspace: string;
}

/** A structured failure from the Rust layer, ready to show a person. */
export interface SidecarError {
  kind: string;
  path?: string;
  detail?: string;
  expected?: number;
  actual?: number;
}

export function describeError(error: unknown): string {
  const e = error as SidecarError;
  switch (e?.kind) {
    case "binary_not_found":
      return `Orbit binary not found. Looked in: ${e.path}. Build it with \`cargo build --release\`, or set a path in settings.`;
    case "protocol_mismatch":
      return `This app speaks Orbit protocol v${e.expected}; the backend speaks v${e.actual}. Update whichever is older.`;
    case "no_ready_frame":
      return `The backend started but never announced itself: ${e.detail}`;
    case "spawn_failed":
      return `Could not start Orbit: ${e.detail}`;
    case "not_running":
      return "The Orbit backend is not running.";
    case "write_failed":
      return `Could not send to Orbit: ${e.detail}`;
    default:
      return typeof error === "string" ? error : JSON.stringify(error);
  }
}

export const orbit = {
  start(workspace: string, binaryPath?: string): Promise<StartedBackend> {
    return invoke("start_backend", { workspace, binaryPath: binaryPath || null });
  },
  send(command: Command): Promise<void> {
    return invoke("send_command", { command });
  },
  stop(): Promise<void> {
    return invoke("stop_backend");
  },
  running(): Promise<boolean> {
    return invoke("backend_running");
  },
  onFrame(handler: (frame: Frame) => void): Promise<UnlistenFn> {
    return listen<Frame>("orbit://frame", (e) => handler(e.payload));
  },
  onDiagnostic(handler: (line: string) => void): Promise<UnlistenFn> {
    return listen<string>("orbit://diagnostic", (e) => handler(e.payload));
  },
  onExit(handler: (reason: string) => void): Promise<UnlistenFn> {
    return listen<string>("orbit://exit", (e) => handler(e.payload));
  },
};
