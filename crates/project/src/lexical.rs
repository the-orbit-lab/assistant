//! Tokenization and word normalization shared by search and query
//! analysis.
//!
//! The point of this module is that a query and the repository text it
//! should match are reduced to the *same* normalized tokens, so a question
//! phrased in prose can find code and documentation phrased differently:
//!
//! ```text
//! "session architecture"  →  [session, architecture]
//! "SessionRuntime"        →  [sessionruntime, session, runtime]
//! "session_state"         →  [session, state]
//! "cancellation"          →  [cancel]
//! "cancelled"             →  [cancel]
//! ```
//!
//! Everything here is deterministic and local: no embeddings, no model,
//! no network. Two identical inputs always produce identical tokens.

use std::collections::HashSet;

/// Conversational filler that must never drive retrieval.
///
/// These are removed from *queries*, not from indexed content, so a
/// document is still findable by any word it contains — the list only
/// stops "explain", "now", "how", "works" from outranking "session" and
/// "cancellation" in a question like "Now explain how cancellation
/// works."
const STOPWORDS_EN: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "cannot",
    "could",
    "describe",
    "detail",
    "detailed",
    "details",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "explain",
    "explanation",
    "few",
    "find",
    "for",
    "from",
    "further",
    "get",
    "give",
    "got",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "know",
    "let",
    "like",
    "look",
    "made",
    "make",
    "many",
    "may",
    "me",
    "might",
    "more",
    "most",
    "must",
    "my",
    "need",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "otherwise",
    "ought",
    "our",
    "ours",
    "out",
    "over",
    "own",
    "part",
    "please",
    "put",
    "same",
    "say",
    "see",
    "shall",
    "she",
    "should",
    "show",
    "since",
    "so",
    "some",
    "such",
    "take",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "us",
    "use",
    "used",
    "using",
    "very",
    "want",
    "was",
    "way",
    "we",
    "well",
    "were",
    "what",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "within",
    "work",
    "working",
    "works",
    "would",
    "you",
    "your",
    "yours",
    // Words for the container rather than the subject. "What does the
    // OBC project do?" is about OBC, not about the word "project", which
    // occurs in nearly every file of every repository and would swamp
    // the real terms. A question genuinely about project *configuration*
    // still retrieves on "configuration".
    "codebase",
    "project",
    "projects",
    "repo",
    "repos",
    "repositories",
    "repository",
];

/// Portuguese filler, so a question asked in Portuguese is analyzed as
/// well as an English one rather than retrieving on `explique` and `como`.
const STOPWORDS_PT: &[&str] = &[
    "agora",
    "ainda",
    "antes",
    "apenas",
    "aqui",
    "assim",
    "cada",
    "depois",
    "então",
    "hoje",
    "onde",
    "outra",
    "outras",
    "outro",
    "outros",
    "quanto",
    "quantos",
    "sempre",
    "talvez",
    "toda",
    "todas",
    "todo",
    "todos",
    "a",
    "ao",
    "aos",
    "aquela",
    "aquelas",
    "aquele",
    "aqueles",
    "aquilo",
    "as",
    "até",
    "com",
    "como",
    "da",
    "das",
    "de",
    "dela",
    "delas",
    "dele",
    "deles",
    "depois",
    "detalhe",
    "detalhes",
    "diga",
    "do",
    "dos",
    "e",
    "ela",
    "elas",
    "ele",
    "eles",
    "em",
    "entre",
    "era",
    "eram",
    "essa",
    "essas",
    "esse",
    "esses",
    "esta",
    "estas",
    "este",
    "estes",
    "está",
    "estão",
    "eu",
    "explica",
    "explicar",
    "explique",
    "faz",
    "fazer",
    "fale",
    "foi",
    "for",
    "foram",
    "funciona",
    "funcionam",
    "funcionamento",
    "há",
    "isso",
    "isto",
    "já",
    "lhe",
    "lhes",
    "mais",
    "mas",
    "me",
    "mesmo",
    "meu",
    "meus",
    "minha",
    "minhas",
    "mostra",
    "mostre",
    "muito",
    "na",
    "nas",
    "nem",
    "no",
    "nos",
    "nossa",
    "nosso",
    "num",
    "numa",
    "não",
    "o",
    "os",
    "ou",
    "para",
    "pela",
    "pelas",
    "pelo",
    "pelos",
    "por",
    "porque",
    "pode",
    "podem",
    "pois",
    "qual",
    "quais",
    "quando",
    "que",
    "quem",
    "se",
    "sem",
    "ser",
    "seu",
    "seus",
    "sobre",
    "sua",
    "suas",
    "são",
    "também",
    "tem",
    "tinha",
    "tudo",
    "um",
    "uma",
    "vez",
    "você",
    "voces",
    "vocês",
    "é",
    "projeto",
    "projetos",
    "repositorio",
    "repositório",
    "repositorios",
    "repositórios",
];

/// Words that refer back to something already discussed rather than
/// naming it. Their presence means the question cannot be understood on
/// its own and should be resolved against the conversation's topic state.
const REFERENCE_WORDS: &[&str] = &[
    // English
    "it", "its", "that", "this", "these", "those", "them", "they", "there", "above", "previous",
    "same", "such", // Portuguese
    "isso", "isto", "aquilo", "esse", "essa", "este", "esta", "aquele", "aquela", "deles", "dele",
    "dela", "mesmo", "anterior",
];

fn stopwords() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS_EN.iter().chain(STOPWORDS_PT).copied().collect())
}

fn reference_words() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| REFERENCE_WORDS.iter().copied().collect())
}

pub fn is_stopword(word: &str) -> bool {
    stopwords().contains(word.to_lowercase().as_str())
}

/// Whether `word` refers back to earlier conversation instead of naming
/// its subject (`that`, `it`, `isso`, ...).
pub fn is_reference_word(word: &str) -> bool {
    reference_words().contains(word.to_lowercase().as_str())
}

/// Split a run of alphanumerics on case boundaries.
///
/// `SessionRuntime` → `[Session, Runtime]`, `HTTPServer` → `[HTTP,
/// Server]`. Letter/digit boundaries are deliberately *not* split, so
/// `STM32` stays one token and remains findable as written.
fn split_case(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();

    for (index, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && index > 0 {
            let previous = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|n| n.is_lowercase());
            // `aB` starts a new word; so does the `S` in `HTTPServer`.
            let boundary = previous.is_lowercase() || (previous.is_uppercase() && next_is_lower);
            if boundary && !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Collapse a doubled final consonant (`cancell` → `cancel`), which is
/// what English suffix stripping leaves behind.
fn undouble(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() >= 4 {
        let last = chars[chars.len() - 1];
        let previous = chars[chars.len() - 2];
        if last == previous && !"aeiou".contains(last) {
            return chars[..chars.len() - 1].iter().collect();
        }
    }
    word.to_string()
}

/// Conservative English suffix stripping.
///
/// Deliberately not a full Porter stemmer: it only removes suffixes whose
/// removal is unlikely to merge unrelated words, and never shortens a word
/// below four characters. The case that matters most in practice is
/// `cancellation`/`cancelled`/`cancelling` all reducing to `cancel`, so a
/// question about "cancellation" finds a `CancellationToken`.
///
/// Portuguese is handled by stopword filtering rather than stemming; see
/// the module docs.
pub fn stem(word: &str) -> String {
    const MIN_LEN: usize = 4;
    let lower = word.to_lowercase();
    if lower.chars().count() <= MIN_LEN {
        return lower;
    }

    // Longest suffix first, so `-ations` is not mistaken for `-s`.
    //
    // `-ment` is deliberately absent: stripping it would map `document`
    // to `docu` while `documentation` maps to `document`, so the two
    // would stop matching each other. Only suffixes that stem
    // consistently across a whole word family are listed.
    const SUFFIXES: &[&str] = &[
        "ations", "ation", "izations", "ization", "nesses", "ness", "ingly", "ing", "edly", "ed",
        "ies", "es", "s",
    ];

    let mut base = lower.clone();
    for suffix in SUFFIXES {
        if let Some(stripped) = lower.strip_suffix(suffix)
            && stripped.chars().count() >= MIN_LEN
        {
            // `ies` → `y` keeps `properties`/`property` together.
            base = if *suffix == "ies" {
                format!("{stripped}y")
            } else {
                stripped.to_string()
            };
            break;
        }
    }

    let base = undouble(&base);
    // Applied to stripped and unstripped words alike, so a silent final
    // `e` cannot split a family: without this, `require` stays whole
    // while `requires`/`required`/`requiring` all become `requir`, and
    // the stemmer would prevent the very matches it exists to enable.
    drop_silent_e(&base)
}

/// Remove a trailing `e` from a word long enough to keep its meaning.
fn drop_silent_e(word: &str) -> String {
    if word.chars().count() > 4 && word.ends_with('e') {
        let mut chars: Vec<char> = word.chars().collect();
        chars.pop();
        return chars.into_iter().collect();
    }
    word.to_string()
}

/// One normalized token and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Lowercased surface form, with identifiers split.
    pub text: String,
    /// Normalized form used for matching.
    pub stem: String,
}

/// Tokenize arbitrary text into normalized tokens.
///
/// Splits on any non-alphanumeric character, then on case boundaries. A
/// compound identifier also yields its joined form, so both
/// `SessionRuntime` and `session` find the same line:
///
/// ```text
/// "SessionRuntime::cancel()" → [sessionruntime, session, runtime, cancel]
/// ```
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let parts = split_case(raw);
        if parts.len() > 1 {
            // The whole identifier, so an exact `SessionRuntime` query
            // still scores against it directly.
            let joined = raw.to_lowercase();
            tokens.push(Token {
                stem: stem(&joined),
                text: joined,
            });
        }
        for part in parts {
            let lower = part.to_lowercase();
            if lower.is_empty() {
                continue;
            }
            tokens.push(Token {
                stem: stem(&lower),
                text: lower,
            });
        }
    }
    tokens
}

/// Tokenize and reduce to distinct normalized terms, dropping stopwords
/// and single characters. Used for queries, where filler must not
/// compete with real subject terms.
pub fn content_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for token in tokenize(text) {
        if token.text.chars().count() < 2 || is_stopword(&token.text) {
            continue;
        }
        if seen.insert(token.stem.clone()) {
            terms.push(token.stem);
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stems(text: &str) -> Vec<String> {
        tokenize(text).into_iter().map(|t| t.stem).collect()
    }

    /// Stems are internal matching keys, so tests compare against
    /// `stem(word)` rather than a literal: what matters is that the word
    /// is findable, not what its normalized form happens to look like.
    #[test]
    fn splits_camel_case_and_keeps_the_whole_identifier() {
        let tokens = stems("SessionRuntime");
        assert!(tokens.contains(&stem("SessionRuntime")), "{tokens:?}");
        assert!(tokens.contains(&stem("session")), "{tokens:?}");
        assert!(tokens.contains(&stem("runtime")), "{tokens:?}");
    }

    #[test]
    fn splits_acronym_boundaries() {
        let tokens: Vec<String> = tokenize("HTTPServer").into_iter().map(|t| t.text).collect();
        assert!(tokens.contains(&"http".to_string()), "{tokens:?}");
        assert!(tokens.contains(&"server".to_string()), "{tokens:?}");
    }

    #[test]
    fn splits_snake_and_kebab_case() {
        assert!(stems("session_state").contains(&stem("state")));
        assert!(stems("session-state").contains(&stem("state")));
        assert!(stems("crates/session/src/runtime.rs").contains(&stem("runtime")));
    }

    /// Splitting letters from digits would make `STM32` unfindable as
    /// written, which is exactly how it appears in real documents.
    #[test]
    fn does_not_split_letters_from_digits() {
        let tokens: Vec<String> = tokenize("STM32").into_iter().map(|t| t.text).collect();
        assert_eq!(tokens, vec!["stm32".to_string()]);
    }

    #[test]
    fn stems_cancellation_family_to_one_term() {
        for word in [
            "cancel",
            "cancels",
            "cancelled",
            "cancelling",
            "cancellation",
        ] {
            assert_eq!(stem(word), "cancel", "{word} should stem to `cancel`");
        }
    }

    #[test]
    fn stems_plurals_and_gerunds() {
        assert_eq!(stem("sessions"), "session");
        assert_eq!(stem("engineering"), "engineer");
        assert_eq!(stem("properties"), "property");
        assert_eq!(stem("requirements"), "requirement");
        assert_eq!(stem("architectures"), stem("architecture"));
        assert_eq!(stem("implementations"), "implement");
    }

    /// A word and its longer relatives must stem to the same term, or the
    /// stemmer would *prevent* matches instead of enabling them.
    #[test]
    fn word_families_stem_consistently() {
        for family in [
            vec!["document", "documents", "documentation", "documentations"],
            vec!["implement", "implements", "implementing", "implementation"],
            vec!["require", "requires", "required", "requiring"],
            vec![
                "cancel",
                "cancels",
                "cancelled",
                "cancelling",
                "cancellation",
            ],
        ] {
            let stems: Vec<String> = family.iter().map(|w| stem(w)).collect();
            assert!(
                stems.windows(2).all(|w| w[0] == w[1]),
                "{family:?} stemmed inconsistently to {stems:?}"
            );
        }
    }

    /// Over-stemming would merge unrelated words; short words are left
    /// alone entirely.
    /// Short words are left completely alone, so unrelated ones are
    /// never merged by aggressive truncation.
    #[test]
    fn does_not_over_stem_short_words() {
        for word in ["this", "bus", "api", "id", "fn", "type", "file"] {
            assert_eq!(stem(word), word, "{word} must not be altered");
        }
    }

    /// Distinct concepts must not collapse into one term.
    #[test]
    fn keeps_unrelated_words_distinct() {
        assert_ne!(stem("session"), stem("source"));
        assert_ne!(stem("cancel"), stem("channel"));
        assert_ne!(stem("event"), stem("evening"));
        assert_ne!(stem("document"), stem("docker"));
    }

    #[test]
    fn filters_english_conversational_filler() {
        let terms = content_terms("Now explain how cancellation works.");
        assert_eq!(terms, vec![stem("cancellation")]);
        assert_eq!(terms, vec!["cancel".to_string()]);
    }

    #[test]
    fn filters_portuguese_conversational_filler() {
        let terms = content_terms("Agora explique como funciona o cancelamento da sessão");
        assert!(terms.contains(&"cancelamento".to_string()), "{terms:?}");
        assert!(terms.contains(&"sessão".to_string()), "{terms:?}");
        for filler in ["agora", "explique", "como", "funciona", "da"] {
            assert!(
                !terms.contains(&filler.to_string()),
                "{filler} in {terms:?}"
            );
        }
    }

    #[test]
    fn keeps_subject_terms_of_a_conversational_question() {
        assert_eq!(
            content_terms("Explain the session architecture."),
            vec![stem("session"), stem("architecture")]
        );
    }

    /// Container words describe where something lives, not what it is,
    /// and would otherwise match nearly every file.
    #[test]
    fn filters_container_words() {
        // `project` is filler; `obc` is a real term here. (Excluding a
        // project's *own* name happens a layer up, in workspace
        // retrieval, where it is known which project is being searched.)
        assert_eq!(
            content_terms("What does the OBC project do?"),
            vec!["obc".to_string()]
        );
        assert!(is_stopword("repository"));
        assert!(is_stopword("projeto"));
        // ...but a real subject alongside them survives.
        assert_eq!(
            content_terms("explain the project cancellation flow"),
            vec![stem("cancellation"), stem("flow")]
        );
    }

    #[test]
    fn recognizes_reference_words_in_both_languages() {
        assert!(is_reference_word("that"));
        assert!(is_reference_word("it"));
        assert!(is_reference_word("isso"));
        assert!(!is_reference_word("session"));
    }

    #[test]
    fn is_deterministic() {
        let once = content_terms("Explain the session architecture and cancellation");
        let twice = content_terms("Explain the session architecture and cancellation");
        assert_eq!(once, twice);
    }
}
