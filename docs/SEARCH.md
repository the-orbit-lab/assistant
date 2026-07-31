# Search and retrieval

How Orbit finds the repository evidence an answer is built on. Everything
here is deterministic and local — no embeddings, no vector database, no
model involvement in deciding what to retrieve.

## The problem this solves

Orbit previously required a query to appear in a file as **one exact
substring**. That made ordinary questions unanswerable. Asked to "Explain
the session architecture" about a repository containing `SessionRuntime`,
`docs/SESSIONS.md`, and "session lifecycle", search returned nothing,
because no single line contains the literal string `session architecture`.
Retrieval then reported no evidence, and the model answered from general
knowledge instead — confidently, and about something else.

Three separate defects produced that:

1. **Substring-only matching.** A multi-word query could only match
   verbatim.
2. **Conversational filler in the query.** "Now explain how cancellation
   works" was searched almost verbatim, so `now`, `explain`, and `works`
   competed with `cancellation`.
3. **No conversational context.** A follow-up naming only "cancellation"
   had no way to know it meant cancellation *in the session runtime*.

All three are addressed below.

## Pipeline

```text
question
   ↓  query analysis          (orbit-project::query)
terms + phrase + context
   ↓  lexical search          (orbit-project::search)
ranked lines and files
   ↓  progressive reads       (retrieval)
full text of the strongest matches
   ↓  confidence              (orbit-core::RetrievalConfidence)
grounded answer, or an explicit "not enough evidence"
```

## Tokenization (`orbit-project::lexical`)

A query and the text it should match are reduced to the same normalized
tokens:

| Input | Tokens |
|---|---|
| `SessionRuntime` | `sessionruntime`, `session`, `runtime` |
| `session_state`, `session-state` | `session`, `state` |
| `HTTPServer` | `http`, `server` |
| `crates/session/src/runtime.rs` | `crates`, `session`, `src`, `runtime`, `rs` |
| `STM32` | `stm32` |

- Unicode-aware: splits on any non-alphanumeric character, so accented
  words survive intact.
- Case-boundary splitting handles camelCase and acronym runs. A compound
  identifier also yields its joined form, so both `SessionRuntime` and
  `session` match the same line.
- **Letters are never split from digits**, so `STM32` stays findable as
  written.

### Stemming

Conservative English suffix stripping, so a word family shares one term:

```text
cancel · cancels · cancelled · cancelling · cancellation  →  cancel
document · documents · documentation                       →  document
require · requires · required · requiring                  →  requir
```

Two rules exist specifically to stop the stemmer from *preventing*
matches: a doubled final consonant is collapsed (`cancell` → `cancel`),
and a silent final `e` is dropped from stripped and unstripped words
alike (otherwise `require` and `required` would diverge). `-ment` is
deliberately **not** stripped, because it would map `document` to `docu`
while `documentation` maps to `document`, splitting the family it was
meant to join. Words of four characters or fewer are left untouched.

Portuguese is handled by stopword filtering rather than stemming.

## Query analysis (`orbit-project::query`)

A conversational sentence is not a search query.

```text
"Explain the session architecture."      → [session, architecture]
"Now explain how cancellation works."    → [cancel]
"What does the OBC project do?"          → [obc]
"Agora explique como funciona o cancelamento" → [cancelamento]
```

Filtered as filler: English and Portuguese function words, plus verbs of
asking (`explain`, `show`, `tell`, `explique`, `mostre`) and **container
words** (`project`, `repository`, `codebase`, `projeto`) that occur in
nearly every file and would swamp the real subject. A question genuinely
about project *configuration* still retrieves on `configuration`.

The analyzer also reports:

- **`phrase`** — the longest run of consecutive non-filler words,
  preserved verbatim, used for the exact-phrase bonus.
- **`has_reference`** — the question contains `that`, `it`, `this`,
  `isso`, … and so refers back to something already discussed.
- **`needs_context`** — the question has one term or none, and cannot be
  retrieved on by itself.

## Ranking (`orbit-project::search`)

BM25 over normalized tokens, scored **per line** so every result keeps a
precise source location, with inverse document frequency computed over
the project's own files. A term occurring in every file contributes
little; a distinctive one contributes a lot.

On top of the lexical score:

| Signal | Effect |
|---|---|
| Filename term match | Large bonus, IDF-weighted; also emits a whole-file result |
| Path component match | Smaller bonus; also emits a whole-file result |
| Markdown heading | Multiplier — a heading names its section |
| Rust definition (`fn`, `struct`, `enum`, `trait`, …) | Bonus, so the line that *declares* a symbol outranks lines that merely use it |
| Exact phrase present | Bonus — a *bonus*, never a requirement |
| Term coverage | Rewards lines matching more distinct query terms |

Results are truncated per file before global ranking, so one large
document cannot fill the result set. Ordering is fully deterministic:
scores are scaled to integers, and ties break on path then line number.

## Conversational context (`orbit-session::topic`)

The session keeps a compact, structured record of what the conversation is
about — deliberately not the whole transcript, which would drown each new
question in earlier terms:

- **subject terms**, refreshed when mentioned and expiring after three
  turns without a mention;
- **selected projects** (tracked separately, and *never* used as search
  terms — a project name matches nearly everything inside it);
- **entities** derived from the file names of sources actually retrieved;
- **recent source paths**;
- whether the last question contained an unresolved reference.

Context is merged into a query only when the question needs it — it refers
back, or has too little subject of its own. A self-contained question
starts a fresh topic rather than inheriting the previous one.

```text
Turn 1: "Explain the session architecture."   → [session, architecture]
Turn 2: "Now explain how cancellation works." → [cancel] + [session, architecture]
```

Only sources from real retrieval update the topic, so the model cannot
steer future searches by mentioning a path in prose.

## Progressive retrieval

For every question, before the model is consulted:

1. `project.information` / `workspace.project_information` — project
   identity.
2. **Lexical search.** Filename, path, and heading bonuses mean
   documentation and likely source modules surface without any special
   case for `docs/`.
3. **Read the strongest matches in full** (up to three distinct files). A
   one-line excerpt rarely answers "explain X"; the surrounding prose
   does. The user is never asked which file to inspect, because the
   ranking already knows.
4. **Fallback:** if nothing matched, read whatever looks like an overview
   (README, CLAUDE.md, a spec under `docs/`).

Retrieval reads are **truncating**: an oversized file yields its
beginning, clearly marked `truncated`, rather than failing. This matters
because the most relevant document for a question is often the longest
one, and refusing it outright is how a well-documented subject ends up
answered from general knowledge. `project.read_file` keeps its strict
behavior by default; truncation is opt-in per call and enforces the
identical security boundary (root containment, symlink rejection,
include/exclude rules).

## Grounding policy

Confidence is judged on **distinct files** cited, so several matches
inside one document do not look like corroboration:

| Distinct files | Confidence |
|---|---|
| 0 | `None` |
| 1 | `Low` |
| 2+ | `High` |

Below `High`, a trusted system instruction is appended before the model
answers:

```text
Do not answer from general knowledge as if it describes this project.
Say plainly that the repository search did not return enough evidence…
Do not invent file paths, types, functions, or behavior…
Do not include unrelated code examples or generic tutorials…
```

Saying "no supporting source was found" is a correct answer. A confident
generic explanation presented as a description of the repository is not.

## Debugging a search

```bash
orbit --verbose search "session architecture"
```

prints, on stderr (so stdout stays machine-readable):

```text
query:            session architecture
normalized query: session architectur
extracted tokens: ["session", "architectur"]
results:          20
docs/ARCHITECTURE.md:1 (Architecture)
    # Architecture
      score=10265 lexical=1.14 coverage=0.50 filename=2.27 path=0.00 heading=1 symbol=0.00 phrase=0.00 matched=["architectur"]
```

Structural information only — the terms searched for and why each result
ranked where it did. No file content appears beyond the excerpts search
already returns, and nothing here can reach an excluded file.

## Known limitations

- **Lexical, not semantic.** A question must share vocabulary with the
  content. "How do I stop a running request?" does not reach
  "cancellation" unless the word appears somewhere relevant.
- **No cross-language matching.** A Portuguese question finds English
  content only through terms the two share — typically identifiers and
  technical nouns, which is how mixed-language repositories are actually
  written. Translating `cancelamento` to `cancellation` would need a
  bilingual lexicon and is not implemented.
- **Stemming is English-only** and conservative; it will not connect
  `architecture` to `architectural`.
- **No embeddings or vector database**, by design.
- Container words (`project`, `repository`) are always filtered, so a
  question about literally those words retrieves on its other terms.
- The topic state carries at most a handful of terms for three turns; a
  subject reintroduced after a long digression must be named again.
