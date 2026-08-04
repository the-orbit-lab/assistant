# Voice

**Not implemented.** This document records the intended design so the
next change has a shape to fill, and so nobody mistakes the absence for
an oversight.

Orbit's desktop app has no microphone button today. Adding one that did
nothing, or that only worked through the browser's `SpeechRecognition`
(which is cloud-backed in most builds), would be worse than its absence:
the point of Orbit is that you can tell where an answer came from, and
that applies to where your audio goes too.

## Intended shape

Both directions sit behind a provider interface, so the conversation
logic never depends on one implementation:

```ts
interface SpeechToTextProvider {
  isAvailable(): Promise<boolean>;
  start(): Promise<void>;
  stopAndTranscribe(): Promise<Transcript>;
  cancel(): Promise<void>;
}

interface TextToSpeechProvider {
  isAvailable(): Promise<boolean>;
  speak(text: string): Promise<void>;
  stop(): Promise<void>;
}
```

**Input** is push-to-talk, never always-listening, and never a wake word.
The first provider should be a locally installed `whisper.cpp`
executable, invoked from Rust with a fixed argument list and a
configurable model path — not an arbitrary shell string. Captured audio
is deleted after transcription unless a debug flag is set explicitly.

**Output** speaks only the assistant's prose: not code blocks, source
lists, action logs, JSON, or permission arguments. Deltas are buffered
into sentences before being enqueued, so speech starts early without
reading fragments aloud, and the queue is dropped immediately on stop,
cancellation, or a new recording.

**States** to render honestly: `idle`, `requesting_permission`,
`listening`, `transcribing`, `sending`, `thinking`, `speaking`,
`stopped`, `error`. The microphone indicator must never be shown as
active unless capture really is.

A voice failure must never make text chat unusable.

`NSMicrophoneUsageDescription` will be required in the macOS bundle
before any capture code ships.
