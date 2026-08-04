# Voice

**Output works. Input does not.**

Orbit speaks its answers using the macOS synthesizer. It does not listen:
no speech-to-text provider is configured, and the microphone button says
so rather than pretending. The point of Orbit is that you can tell where
an answer came from, and that applies to where your audio goes too — so
a button that quietly shipped recordings to a cloud service would
undercut the whole thing.

## What works: spoken responses

`/usr/bin/say`, driven from Rust with a **fixed argument list**. The text
arrives on the process's stdin rather than being interpolated into a
command string, so there is no shell to escape and nothing in a model's
output can do more than be read aloud.

Only prose is spoken. `speech::speakable` strips code fences, tables,
horizontal rules, list markers, heading hashes, emphasis, inline code,
and link targets before anything reaches the synthesizer — in Rust, so
every provider added later inherits the same rule.

Deltas are buffered into sentences (`src/features/voice/sentences.ts`)
so speech starts while text is still arriving without reading fragments
aloud. A boundary inside a decimal, an abbreviation, or an ellipsis is
not a boundary, and a sentence shorter than 24 characters waits for the
next one. The queue is dropped immediately on **Stop voice** and on task
cancellation.

## What does not: voice input

The microphone button is present and reports `Voice output only`.
Pressing it explains what would make it work. `isCapturing` gates the
active indicator, so the microphone can never appear live while nothing
is being recorded.

## Intended shape for input

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

Push-to-talk, never always-listening, and never a wake word. The first
provider should be a locally installed `whisper.cpp` executable, invoked
from Rust with a fixed argument list and a configurable model path — not
an arbitrary shell string. Captured audio is deleted after transcription
unless a debug flag is set explicitly.

**States** rendered honestly: `idle`, `requesting_permission`,
`listening`, `transcribing`, `sending`, `thinking`, `speaking`,
`stopped`, `error`. The microphone indicator must never be shown as
active unless capture really is.

A voice failure never makes text chat unusable: speech runs beside the
conversation, and every failure path leaves the composer working.

`NSMicrophoneUsageDescription` will be required in the macOS bundle
before any capture code ships.
