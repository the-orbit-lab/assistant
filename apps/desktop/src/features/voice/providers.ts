/**
 * Speech providers.
 *
 * The conversation never talks to a synthesizer or a recognizer
 * directly, so swapping either one is a change in this file and nowhere
 * else.
 *
 * Only text-to-speech has a real implementation today. Speech-to-text
 * reports itself unavailable and carries setup instructions, because a
 * microphone button that quietly did something else — or nothing —
 * would undercut the property this app exists to demonstrate.
 */

import { invoke } from "@tauri-apps/api/core";

export interface Transcript {
  text: string;
  confidence?: number;
}

export interface SpeechToTextProvider {
  readonly name: string;
  isAvailable(): Promise<boolean>;
  /** Why it is unavailable, phrased as something the user can act on. */
  readonly setupHint: string;
  start(): Promise<void>;
  stopAndTranscribe(): Promise<Transcript>;
  cancel(): Promise<void>;
}

export interface TextToSpeechProvider {
  readonly name: string;
  isAvailable(): Promise<boolean>;
  speak(text: string): Promise<void>;
  stop(): Promise<void>;
  isSpeaking(): Promise<boolean>;
}

/** The macOS synthesizer, driven from Rust with a fixed argument list. */
export const macOsTts: TextToSpeechProvider = {
  name: "macOS",
  isAvailable: () => invoke<boolean>("speech_available"),
  speak: (text) => invoke("speak", { text }),
  stop: () => invoke("stop_speaking"),
  isSpeaking: () => invoke<boolean>("is_speaking"),
};

/**
 * The absence of a speech recognizer, made explicit.
 *
 * Reporting unavailability is the honest state: it keeps the button on
 * screen so the feature is discoverable, and says exactly what would
 * make it work, rather than pretending or hiding.
 */
export const unconfiguredStt: SpeechToTextProvider = {
  name: "none",
  isAvailable: async () => false,
  setupHint:
    "No speech-to-text provider is configured. Orbit will not send audio to a cloud service, " +
    "so voice input needs a local recognizer — see docs/VOICE.md.",
  start: async () => {
    throw new Error("no speech-to-text provider is configured");
  },
  stopAndTranscribe: async () => {
    throw new Error("no speech-to-text provider is configured");
  },
  cancel: async () => {},
};
