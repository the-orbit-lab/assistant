/**
 * Turning a stream of tokens into utterances.
 *
 * Speaking each delta as it arrives produces stuttering fragments;
 * waiting for the whole answer means speech starts long after the text
 * does. So deltas accumulate until a sentence boundary, and only whole
 * sentences are enqueued.
 *
 * Two rules keep it from sounding wrong:
 *
 * - a boundary inside a decimal, an abbreviation, or an ellipsis is not
 *   a boundary;
 * - a fragment shorter than `minLength` waits for the next one, because
 *   "Yes." on its own is jarring when three more words are 40ms away.
 */

/** Abbreviations whose trailing period does not end a sentence. */
const ABBREVIATIONS = ["e.g.", "i.e.", "etc.", "vs.", "Dr.", "Mr.", "Ms.", "Fig.", "approx."];

export interface SentenceBufferOptions {
  /** Below this many characters, a complete sentence still waits. */
  minLength?: number;
  /** Emit regardless once the buffer reaches this length. */
  maxLength?: number;
}

export class SentenceBuffer {
  private buffer = "";
  /**
   * How much of the message has already been fed in.
   *
   * The UI holds the accumulated message text, not a delta queue, so
   * this records where the last feed stopped and only the new tail is
   * pushed. Without it, every render would re-speak the whole answer.
   */
  spokenLength = 0;
  private readonly minLength: number;
  private readonly maxLength: number;

  constructor(options: SentenceBufferOptions = {}) {
    this.minLength = options.minLength ?? 24;
    this.maxLength = options.maxLength ?? 320;
  }

  /** Add text; return every utterance now ready to speak. */
  push(delta: string): string[] {
    this.buffer += delta;
    const ready: string[] = [];

    for (;;) {
      const cut = this.boundary();
      if (cut === -1) break;
      const sentence = this.buffer.slice(0, cut).trim();
      this.buffer = this.buffer.slice(cut);
      if (sentence) ready.push(sentence);
    }
    return ready;
  }

  /** Everything left, at the end of a response. */
  flush(): string[] {
    const rest = this.buffer.trim();
    this.buffer = "";
    return rest ? [rest] : [];
  }

  /** Drop pending text — cancellation, or a new recording. */
  clear(): void {
    this.buffer = "";
    this.spokenLength = 0;
  }

  get pending(): string {
    return this.buffer;
  }

  /**
   * Index just past the first real sentence end, or -1.
   */
  private boundary(): number {
    if (this.buffer.length >= this.maxLength) {
      // Prefer a space so the cut lands between words.
      const space = this.buffer.lastIndexOf(" ", this.maxLength);
      return space > this.minLength ? space + 1 : this.maxLength;
    }

    for (let i = 0; i < this.buffer.length; i += 1) {
      const char = this.buffer[i];
      if (char !== "." && char !== "!" && char !== "?" && char !== "\n") continue;

      // A newline always ends an utterance: it is a paragraph or a list
      // item, and both are natural pauses.
      if (char === "\n") {
        if (i + 1 >= this.minLength) return i + 1;
        continue;
      }

      // "3.14" and "v1.2" are not sentence ends.
      const before = this.buffer[i - 1];
      const after = this.buffer[i + 1];
      if (before && /\d/.test(before) && after && /\d/.test(after)) continue;

      // "..." ends once, at the last dot.
      if (after === "." ) continue;

      // The next character must be whitespace, or we are mid-token.
      if (after !== undefined && !/\s/.test(after)) continue;

      const head = this.buffer.slice(0, i + 1);
      if (ABBREVIATIONS.some((abbr) => head.endsWith(abbr))) continue;

      if (i + 1 < this.minLength) continue;
      return i + 1;
    }
    return -1;
  }
}
