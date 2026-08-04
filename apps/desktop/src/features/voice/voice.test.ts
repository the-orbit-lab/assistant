import { describe, expect, it } from "vitest";
import { SentenceBuffer } from "./sentences";
import { canStopSpeech, isCapturing, VOICE_LABELS, type VoiceState } from "./state";
import { unconfiguredStt, macOsTts } from "./providers";

describe("sentence buffering", () => {
  it("holds a fragment until the sentence ends", () => {
    const buffer = new SentenceBuffer();
    expect(buffer.push("SessionRuntime owns the ")).toEqual([]);
    expect(buffer.push("conversation state for one session.")).toEqual([
      "SessionRuntime owns the conversation state for one session.",
    ]);
  });

  it("emits several sentences from one delta", () => {
    const buffer = new SentenceBuffer();
    const ready = buffer.push(
      "The first field holds the id. The second holds the mode. The third is a mutex.",
    );
    expect(ready).toEqual([
      "The first field holds the id.",
      "The second holds the mode.",
    ]);
    // The last sentence is under minLength, so it waits for more text
    // rather than being spoken as a fragment -- and `flush` releases it
    // when the response ends.
    expect(buffer.flush()).toEqual(["The third is a mutex."]);
  });

  /// Speaking "Yes." alone is jarring when more text is milliseconds away.
  it("does not speak a very short fragment on its own", () => {
    const buffer = new SentenceBuffer({ minLength: 24 });
    expect(buffer.push("Yes. ")).toEqual([]);
    const ready = buffer.push("The runtime stores it in a mutex.");
    expect(ready[0]).toContain("Yes.");
  });

  it("does not break inside a decimal", () => {
    const buffer = new SentenceBuffer();
    expect(buffer.push("The value is 3.14159 exactly and nothing more")).toEqual([]);
  });

  it("does not break on a common abbreviation", () => {
    const buffer = new SentenceBuffer();
    expect(buffer.push("Some actions, e.g. reads, are permitted here")).toEqual([]);
  });

  it("treats an ellipsis as one boundary", () => {
    const buffer = new SentenceBuffer();
    const ready = buffer.push("It waits for a decision... then the turn resumes normally. ");
    expect(ready).toHaveLength(2);
    expect(ready[0]).toBe("It waits for a decision...");
  });

  it("breaks on a paragraph boundary", () => {
    const buffer = new SentenceBuffer();
    const ready = buffer.push("A heading line that is long enough\nnext paragraph");
    expect(ready[0]).toBe("A heading line that is long enough");
  });

  it("emits eventually even with no punctuation at all", () => {
    const buffer = new SentenceBuffer({ maxLength: 60 });
    const ready = buffer.push("word ".repeat(30));
    expect(ready.length).toBeGreaterThan(0);
  });

  it("flushes the remainder when the response completes", () => {
    const buffer = new SentenceBuffer();
    buffer.push("A trailing clause with no terminator");
    expect(buffer.flush()).toEqual(["A trailing clause with no terminator"]);
    expect(buffer.flush()).toEqual([]);
  });

  it("drops pending text when cleared", () => {
    const buffer = new SentenceBuffer();
    buffer.push("half a sentence");
    buffer.clear();
    expect(buffer.pending).toBe("");
    expect(buffer.flush()).toEqual([]);
  });

  it("reconstructs the full text across sentences", () => {
    const buffer = new SentenceBuffer();
    const source =
      "Orbit reads the declaration first. Then it reads the documentation. Finally it answers.";
    const spoken: string[] = [];
    for (const char of source) spoken.push(...buffer.push(char));
    spoken.push(...buffer.flush());
    expect(spoken.join(" ")).toBe(source);
  });
});

describe("voice state", () => {
  it("shows the microphone as active only while capturing", () => {
    expect(isCapturing("listening")).toBe(true);
    for (const state of ["idle", "transcribing", "thinking", "speaking", "error"] as VoiceState[]) {
      expect(isCapturing(state)).toBe(false);
    }
  });

  it("offers stop-speaking only while speaking", () => {
    expect(canStopSpeech("speaking")).toBe(true);
    expect(canStopSpeech("listening")).toBe(false);
    expect(canStopSpeech("idle")).toBe(false);
  });

  it("labels every state", () => {
    const states: VoiceState[] = [
      "idle", "requesting_permission", "listening", "transcribing",
      "sending", "thinking", "speaking", "stopped", "error",
    ];
    for (const state of states) expect(VOICE_LABELS[state]).toBeTruthy();
  });
});

describe("providers", () => {
  it("reports speech-to-text as unavailable with an actionable hint", async () => {
    expect(await unconfiguredStt.isAvailable()).toBe(false);
    expect(unconfiguredStt.setupHint).toMatch(/local recognizer|docs\/VOICE/);
  });

  it("refuses to start rather than pretending to listen", async () => {
    await expect(unconfiguredStt.start()).rejects.toThrow(/no speech-to-text/);
  });

  it("names the text-to-speech provider", () => {
    expect(macOsTts.name).toBe("macOS");
  });
});
