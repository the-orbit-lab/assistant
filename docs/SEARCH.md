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

## Two stages, not one ranking

Lexical ranking answers *"which lines mention these words"*. An
explanation question asks something else: *"which file defines this
thing"*. Those are different questions, and a single scoring function
cannot serve both — a long document that repeats the query terms will
out-score the twenty-line struct that declares them, every time.

That is not hypothetical. Asked to "Explain SessionRuntime and how it
stores session state", Orbit once answered out of `docs/SEARCH.md` and
described the retrieval pipeline. The lexical engine had produced good
candidates; nothing downstream could tell a file that *defines* a
concept from a verbose file that merely mentions it.

So retrieval is split into layers that each do one job
(`orbit-retrieval`):

```text
question
   ↓  query planner            (plan)      intent, entities, concepts
   ↓  candidate generators     (candidate) five independent proposals
   ↓  reciprocal rank fusion   (fusion)    agreement, not intensity
   ↓  reranker                 (rerank)    evidence quality, not term hits
   ↓  evidence selector        (select)    a diverse, bounded set
   ↓  progressive reads        (retrieval) the selected files, in full
   ↓  confidence               (orbit-core::RetrievalConfidence)
grounded answer, or an explicit "not enough evidence"
```

### Query planner

Classifies the question into an intent — symbol explanation,
architecture, implementation location, requirement comparison, decision,
failure investigation, or general — and extracts the entities it names.
The intent decides which *kinds* of evidence are worth preferring.

Entities come from three sources: backtick-quoted spans, identifier-shaped
words (`SessionRuntime`, `run_turn`), and multi-word runs that normalize
onto a symbol the index actually contains, so "session runtime" reaches
`SessionRuntime` without guessing. A word is only accepted from the index
when it survives stopword filtering *and* names a type — otherwise a
repository containing `fn explain` would make "explain" the subject of
every question that used the verb.

### Candidate generators

Five independent proposals, each answering a different question:

| Generator | Answers |
|---|---|
| lexical | which lines mention these words (BM25, as before) |
| symbol | where is this identifier declared (`syn` AST index) |
| path | which files are *named* after the subject |
| heading | which documentation section is titled after it |
| context | what was this conversation already looking at |

The symbol generator is the direct answer to the reported failure: a
declaration mentions its own name exactly once, so term frequency can
never rank it first.

### Fusion

Reciprocal Rank Fusion combines the five rankings using only *position*,
never their incomparable scores:

```text
score(d) = Σ  weight(g) / (60 + rank_g(d))
```

A file ranked 3rd by three generators beats one ranked 1st by a single
generator. Agreement is a far better proxy for "this is about the
subject" than any one generator's enthusiasm.

### Reranker

Judges what a candidate *is*, not how often it matched:

| Feature | Separates |
|---|---|
| declares a named entity | the definition from every mention of it |
| subject alignment (filename, title) | `SESSIONS.md` from `SEARCH.md` |
| mention ratio over file length | a topic document from a passing reference |
| heading match | a section about X from a document containing X |
| generator agreement | corroborated evidence from a lone hit |
| intent alignment | what this question actually needs |

Each candidate is typed — Definition, Implementation,
DomainDocumentation, Architecture, Requirement, ADR, Test,
IncidentalReference — using structural conventions (a `tests/`
directory, a numbered file under `architecture/`), never a list of this
repository's filenames.

**Incidental-mention detection.** A candidate is incidental when nothing
about its file says it is about the subject: it declares none of the
named entities, it is not named or titled for them, no heading announces
them, and the mentions are sparse relative to its length. Incidental
evidence is demoted, not deleted — a hard exclusion would make a
genuinely relevant passage unreachable — and the selector caps how many
may appear.

An optional bounded local-model reranking pass exists
(`rerank::validate_model_rerank`). It is strictly validated: the model
sees numbered candidates and may return only a permutation of those
numbers. Anything else — an out-of-range index, a repeat, prose, a path —
is rejected and the deterministic order stands. **The model can never
introduce a candidate, name a file, or cause a read.**

### Symbol evidence

Knowing *where* a type is declared is not enough to explain it. A single
line reading `pub struct SessionRuntime {` says nothing about the state
it stores, so a model falls back on whatever else is in its context —
which is how an answer about a type ends up describing the tests that
exercise it.

For a question naming an entity, `orbit-retrieval::evidence` builds an
AST-backed bundle from `syn`:

```rust
pub struct SymbolEvidence {
    pub name: String,
    pub kind: SymbolKind,
    pub definition: SourceSpan,        // declaration incl. doc comments
    pub fields: Vec<FieldEvidence>,    // name, type as written, docs, span
    pub impl_blocks: Vec<ImplEvidence>,// inherent and trait, with methods
    pub documentation: Vec<SourceSpan>,
}
```

Every span is exact, so a comment mentioning `struct SessionRuntime` can
never be mistaken for the declaration.

Whole `impl` blocks are the wrong granularity — `impl SessionRuntime` is
570 lines — so the bundle is quoted under a **span budget**: the
declaration whole (it is short and carries the fields), then individual
methods, most relevant first, each truncated to its opening lines if
oversized. A method too large to quote keeps its doc comment and
signature rather than being dropped. The cost of explaining a type does
not scale with the size of the type.

Which methods are relevant is decided by the question, not by a list of
interesting names. Ranking is term overlap against each method's name,
signature, and docs — the signature matters, because a method taking
`&mut SessionState` answers "how does it store state" even though
nothing in its name says so.

Spans are fetched with **ranged `project.read_file`**, through the same
permission checks a whole-file read performs. A range can only ever
return less than the caller was already allowed to see.

### Evidence selector

Ranking and selecting are different jobs. Taking the top N by score
gives N variations of the same claim — three passages of one document,
or five tests of one type. Selection instead maximizes *marginal* value
(in the spirit of Maximal Marginal Relevance): at each step it takes the
best candidate **after** subtracting how much it repeats what is already
chosen, with hard caps of 2 per file, 3 per evidence type, and 1
incidental reference in a set of 6.

Two rules protect the code that defines the subject:

- **A slot is reserved for direct implementation.** While the set has no
  Definition or Implementation, the last slot is held for one, so
  better-scoring prose cannot squeeze the declaration out entirely.
- **Tests are capped at one while an implementation exists.** A test
  repeats the type's name on every line and describes what the author
  expected, which reads like documentation to a ranking and not at all
  like it to a reader. Where the repository has no implementation to
  offer, the cap lifts — "how is this tested" is a real question.

A score floor of zero applies. A question the repository cannot answer
selects *nothing*, so the grounding policy can say so, rather than
returning the six least-bad incidental mentions and looking sourced.

### Reading

Selection is ordered for coverage; reading is a different decision.
Only a few files fit in a local model's context, so the read list leads
with the evidence types the intent asked for, ordered by score. Without
this, "Explain SessionRuntime" spends one of its three read slots on a
536-line test file and the answer describes the tests.

When a symbol bundle exists it is read **last**, after the supporting
documents, and that ordering is load-bearing: a model answers from what
is nearest its question, and an 8 KB architecture document read after
the declaration produced an answer about session lifecycle in general
rather than about the type. A trusted instruction naming the declaration
accompanies it, stating a fact the AST established rather than asking
the model to verify anything.

Only the files actually selected and read are cited. An earlier version
also cited every line the lexical ranking matched; those excerpt
citations were the visible half of the reported failure, listing five
test files under an answer about one type.

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
