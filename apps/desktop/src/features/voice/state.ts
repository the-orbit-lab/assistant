/**
 * What the voice UI is doing.
 *
 * The microphone must never appear active unless capture really is, so
 * these are the states the UI renders, and nothing renders a state that
 * is not one of them.
 */

export type VoiceState =
  | "idle"
  | "requesting_permission"
  | "listening"
  | "transcribing"
  | "sending"
  | "thinking"
  | "speaking"
  | "stopped"
  | "error";

export const VOICE_LABELS: Record<VoiceState, string> = {
  idle: "Voice ready",
  requesting_permission: "Requesting microphone…",
  listening: "Listening",
  transcribing: "Transcribing…",
  sending: "Sending…",
  thinking: "Orbit is working",
  speaking: "Speaking",
  stopped: "Stopped",
  error: "Voice error",
};

/** Is audio capture actually running in this state? */
export function isCapturing(state: VoiceState): boolean {
  return state === "listening";
}

/** Should the stop-speaking control be offered? */
export function canStopSpeech(state: VoiceState): boolean {
  return state === "speaking";
}
