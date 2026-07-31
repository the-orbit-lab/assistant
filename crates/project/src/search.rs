//! Deterministic, local, model-free lexical search.
//!
//! Ranking is BM25 over normalized tokens, computed per line so every
//! result keeps a precise source location. Term weights come from inverse
//! document frequency over the project's own files, so a word that
//! appears everywhere (`the`, `session` in a session crate) contributes
//! little and a distinctive word contributes a lot.
//!
//! On top of the lexical score, a match is boosted for *where* it
//! occurred — filename, path component, Markdown heading, Rust symbol —
//! and for matching the query's exact wording rather than only its
//! individual words.
//!
//! An earlier version required the whole query to appear as one literal
//! substring. That made ordinary questions unanswerable: "session
//! architecture" found nothing in a repository full of `SessionRuntime`
//! and "session lifecycle", because no single line contained that exact
//! pair of words. Individual-token matching with IDF weighting is what
//! fixes that, and is why the phrase bonus is a *bonus* rather than a
//! requirement.

use std::collections::{HashMap, HashSet};

use orbit_core::SourceReference;

use crate::discovery::DiscoveredFile;
use crate::lexical;

/// BM25 term-frequency saturation. Standard value; a second occurrence of
/// a term on one line matters much less than the first.
const BM25_K1: f64 = 1.2;
/// BM25 length normalization. Standard value; keeps a long line from
/// outscoring a short, precise one purely by containing more words.
const BM25_B: f64 = 0.75;

/// Multipliers and bonuses applied on top of the lexical score. Tuned so
/// that *where* a term appears can promote a result, but never so much
/// that a single incidental mention in a filename outranks a line that
/// genuinely matches several query terms.
const FILENAME_TERM_BONUS: f64 = 3.0;
const PATH_TERM_BONUS: f64 = 1.2;
const HEADING_MULTIPLIER: f64 = 2.2;
const SYMBOL_BONUS: f64 = 1.6;
const PHRASE_BONUS: f64 = 6.0;
/// Rewards a line that covers *more distinct* query terms, so "session
/// architecture" prefers a line about both over one repeating "session".
const COVERAGE_WEIGHT: f64 = 2.5;

/// Where a match came from, kept for `--verbose` debug output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreComponents {
    /// BM25 sum over matched query terms.
    pub lexical: f64,
    /// Fraction of distinct query terms this result matched.
    pub coverage: f64,
    pub filename: f64,
    pub path: f64,
    pub heading: f64,
    pub symbol: f64,
    pub phrase: f64,
    /// The query terms this result actually matched.
    pub matched_terms: Vec<String>,
}

impl ScoreComponents {
    pub fn total(&self) -> f64 {
        (self.lexical
            + self.filename
            + self.path
            + self.symbol
            + self.phrase
            + self.coverage * COVERAGE_WEIGHT)
            * if self.heading > 0.0 {
                HEADING_MULTIPLIER
            } else {
                1.0
            }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub source: SourceReference,
    pub excerpt: String,
    /// Rank score, scaled to an integer so ordering is exactly
    /// reproducible and comparable across runs.
    pub score: u32,
    pub components: ScoreComponents,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub max_excerpt_chars: usize,
    /// Maximum matching lines kept per file, before global ranking.
    pub max_matches_per_file: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            max_excerpt_chars: 240,
            max_matches_per_file: 5,
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn is_markdown(file: &DiscoveredFile) -> bool {
    file.relative_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn is_rust(file: &DiscoveredFile) -> bool {
    file.relative_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("rs"))
        .unwrap_or(false)
}

/// The identifier a Rust definition line introduces, if any.
///
/// Only definitions count: a line that *uses* `SessionRuntime` is
/// ordinary content, while the line that declares it is the one a
/// question about sessions most likely wants.
fn rust_symbol(line: &str) -> Option<&str> {
    let trimmed = line.trim_start().trim_start_matches("pub ").trim_start();
    for keyword in [
        "fn ", "struct ", "enum ", "trait ", "mod ", "type ", "const ", "static ", "impl ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(keyword) {
            let name = rest
                .trim_start()
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .find(|s| !s.is_empty())?;
            return Some(name);
        }
    }
    None
}

fn markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        Some(trimmed.trim_start_matches('#').trim().to_string())
    } else {
        None
    }
}

struct IndexedFile<'a> {
    file: &'a DiscoveredFile,
    content: String,
    /// Distinct normalized terms anywhere in the file, for IDF.
    terms: HashSet<String>,
    filename_terms: HashSet<String>,
    path_terms: HashSet<String>,
}

/// Read and tokenize every searchable file once.
fn index<'a>(files: &'a [DiscoveredFile]) -> Vec<IndexedFile<'a>> {
    let mut indexed = Vec::new();
    for file in files {
        let path_text = file.relative_path.to_string_lossy().to_string();
        let filename = file
            .relative_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        let filename_terms: HashSet<String> = lexical::tokenize(filename)
            .into_iter()
            .map(|t| t.stem)
            .collect();
        let path_terms: HashSet<String> = lexical::tokenize(&path_text)
            .into_iter()
            .map(|t| t.stem)
            .collect();

        // A non-text file is still findable by name and path, which is
        // often exactly what a question about "where is X" needs.
        let content = if file.is_text {
            std::fs::read(&file.absolute_path)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let mut terms: HashSet<String> = lexical::tokenize(&content)
            .into_iter()
            .map(|t| t.stem)
            .collect();
        terms.extend(path_terms.iter().cloned());

        indexed.push(IndexedFile {
            file,
            content,
            terms,
            filename_terms,
            path_terms,
        });
    }
    indexed
}

/// Inverse document frequency per query term, over the indexed files.
fn idf_table(indexed: &[IndexedFile<'_>], query_terms: &[String]) -> HashMap<String, f64> {
    let total = indexed.len().max(1) as f64;
    query_terms
        .iter()
        .map(|term| {
            let df = indexed.iter().filter(|f| f.terms.contains(term)).count() as f64;
            // BM25 IDF, smoothed so a term present in every file still
            // contributes a small positive amount rather than going
            // negative and penalizing its own matches.
            let idf = ((total - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.05);
            (term.clone(), idf)
        })
        .collect()
}

/// Search `files` for `query`.
///
/// `query` is tokenized and stopword-filtered, so a conversational
/// sentence works as well as a bare keyword. Results are ranked by BM25
/// with positional bonuses and are stable across runs: ties break on
/// path, then line number.
pub fn search_files(
    files: &[DiscoveredFile],
    query: &str,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    let query_terms = lexical::content_terms(query);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let phrase = normalized_phrase(query);
    search_terms(files, &query_terms, phrase.as_deref(), options)
}

/// Lowercased, whitespace-collapsed query, used for the exact-phrase
/// bonus. Only meaningful for multi-word queries.
fn normalized_phrase(query: &str) -> Option<String> {
    let collapsed = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.contains(' ') {
        Some(collapsed.to_lowercase())
    } else {
        None
    }
}

/// Search for already-normalized `terms`, bypassing query tokenization.
///
/// Used by callers that built their terms themselves — notably
/// conversational retrieval, which merges the current question's terms
/// with the topic carried in from earlier turns.
pub fn search_terms(
    files: &[DiscoveredFile],
    terms: &[String],
    phrase: Option<&str>,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    if terms.is_empty() {
        return Vec::new();
    }
    let indexed = index(files);
    let idf = idf_table(&indexed, terms);
    let query_set: HashSet<&String> = terms.iter().collect();

    let average_line_terms = {
        let (total, count) = indexed.iter().fold((0usize, 0usize), |(t, c), f| {
            let lines = f.content.lines().count();
            (t + lexical::tokenize(&f.content).len(), c + lines)
        });
        if count == 0 {
            1.0
        } else {
            (total as f64 / count as f64).max(1.0)
        }
    };

    let mut results: Vec<SearchResult> = Vec::new();

    for entry in &indexed {
        let display_path = entry.file.relative_path.clone();

        let filename_hits: Vec<&String> = terms
            .iter()
            .filter(|t| entry.filename_terms.contains(*t))
            .collect();
        let path_hits: Vec<&String> = terms
            .iter()
            .filter(|t| entry.path_terms.contains(*t) && !entry.filename_terms.contains(*t))
            .collect();

        let filename_score = filename_hits
            .iter()
            .map(|t| FILENAME_TERM_BONUS * idf.get(*t).copied().unwrap_or(0.05))
            .sum::<f64>();
        let path_score = path_hits
            .iter()
            .map(|t| PATH_TERM_BONUS * idf.get(*t).copied().unwrap_or(0.05))
            .sum::<f64>();

        // A file whose name *or* directory matches is worth surfacing
        // even when no single line does: "where is the session runtime"
        // is answered by `crates/session/src/runtime.rs` itself, and a
        // question about sessions should reach `crates/session/` even if
        // a particular file inside it never spells the word out.
        if !filename_hits.is_empty() || !path_hits.is_empty() {
            let mut matched: Vec<String> = filename_hits.iter().map(|t| (*t).clone()).collect();
            matched.extend(path_hits.iter().map(|t| (*t).clone()));
            let coverage = matched.len() as f64 / terms.len() as f64;
            let where_ = if filename_hits.is_empty() {
                "path"
            } else {
                "file name"
            };
            let components = ScoreComponents {
                filename: filename_score,
                path: path_score,
                coverage,
                matched_terms: matched,
                ..Default::default()
            };
            results.push(SearchResult {
                source: SourceReference::whole_file(display_path.clone()),
                excerpt: format!("{where_} matches {}", quoted(&components.matched_terms)),
                score: scale(components.total()),
                components,
            });
        }

        if entry.content.is_empty() {
            continue;
        }

        let mut per_file: Vec<SearchResult> = Vec::new();
        let mut current_heading: Option<String> = None;
        let markdown = is_markdown(entry.file);
        let rust = is_rust(entry.file);

        for (index, line) in entry.content.lines().enumerate() {
            let line_number = index + 1;
            let heading = if markdown {
                markdown_heading(line)
            } else {
                None
            };
            if let Some(heading) = &heading {
                current_heading = Some(heading.clone());
            }

            let line_tokens = lexical::tokenize(line);
            if line_tokens.is_empty() {
                continue;
            }
            let line_len = line_tokens.len() as f64;

            let mut frequencies: HashMap<&str, usize> = HashMap::new();
            for token in &line_tokens {
                if query_set.contains(&token.stem) {
                    *frequencies.entry(token.stem.as_str()).or_default() += 1;
                }
            }
            if frequencies.is_empty() {
                continue;
            }

            let lexical_score: f64 = frequencies
                .iter()
                .map(|(term, &tf)| {
                    let tf = tf as f64;
                    let weight = idf.get(*term).copied().unwrap_or(0.05);
                    weight * (tf * (BM25_K1 + 1.0))
                        / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * line_len / average_line_terms))
                })
                .sum();

            let symbol_score = if rust {
                rust_symbol(line)
                    .map(|name| {
                        let symbol_terms: HashSet<String> = lexical::tokenize(name)
                            .into_iter()
                            .map(|t| t.stem)
                            .collect();
                        let hits = terms.iter().filter(|t| symbol_terms.contains(*t)).count();
                        SYMBOL_BONUS * hits as f64
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            let phrase_score = phrase
                .filter(|p| line.to_lowercase().contains(*p))
                .map(|_| PHRASE_BONUS)
                .unwrap_or(0.0);

            let mut matched_terms: Vec<String> =
                frequencies.keys().map(|t| (*t).to_string()).collect();
            matched_terms.sort();
            let coverage = matched_terms.len() as f64 / terms.len() as f64;

            let components = ScoreComponents {
                lexical: lexical_score,
                coverage,
                filename: filename_score,
                path: path_score,
                heading: if heading.is_some() { 1.0 } else { 0.0 },
                symbol: symbol_score,
                phrase: phrase_score,
                matched_terms,
            };

            let mut source = SourceReference::lines(display_path.clone(), line_number, line_number);
            // A heading line names its own section; a body line belongs to
            // the heading above it.
            if let Some(section) = heading.clone().or_else(|| current_heading.clone()) {
                source = source.with_section(section);
            }

            per_file.push(SearchResult {
                source,
                excerpt: truncate(line.trim(), options.max_excerpt_chars),
                score: scale(components.total()),
                components,
            });
        }

        // Keep the strongest lines of each file, so one large document
        // cannot fill the whole result set.
        per_file.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.source.line_start.cmp(&b.source.line_start))
        });
        per_file.truncate(options.max_matches_per_file);
        results.extend(per_file);
    }

    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.source.path.cmp(&b.source.path))
            .then_with(|| a.source.line_start.cmp(&b.source.line_start))
    });
    results.truncate(options.limit);
    results
}

fn quoted(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("`{t}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Scale a float score into a stable integer rank. Comparing integers
/// keeps ordering exactly reproducible.
fn scale(score: f64) -> u32 {
    (score.max(0.0) * 1000.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn file(root: &std::path::Path, relative: &str, content: &str) -> DiscoveredFile {
        let absolute = root.join(relative);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&absolute, content).unwrap();
        DiscoveredFile {
            relative_path: PathBuf::from(relative),
            absolute_path: absolute,
            size: content.len() as u64,
            is_text: true,
        }
    }

    fn paths(results: &[SearchResult]) -> Vec<String> {
        results
            .iter()
            .map(|r| r.source.path.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn ranks_filename_matches_highly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(root, "watchdog.md", "unrelated prose here\n"),
            file(root, "other.md", "mentions watchdog in passing\n"),
        ];
        let results = search_files(&files, "watchdog", &SearchOptions::default());
        assert_eq!(results[0].source.path, PathBuf::from("watchdog.md"));
    }

    #[test]
    fn ranks_heading_above_plain_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![file(
            root,
            "doc.md",
            "some intro\n## Watchdog Design\nbody mentions watchdog here too\n",
        )];
        let results = search_files(&files, "watchdog", &SearchOptions::default());
        assert_eq!(results[0].source.line_start, Some(2));
        assert_eq!(
            results[0].source.section.as_deref(),
            Some("Watchdog Design")
        );
    }

    #[test]
    fn preserves_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![file(
            root,
            "notes.txt",
            "line one\nline two target\nline three\n",
        )];
        let results = search_files(&files, "target", &SearchOptions::default());
        assert_eq!(results[0].source.line_start, Some(2));
        assert_eq!(results[0].source.line_end, Some(2));
    }

    #[test]
    fn is_deterministic_across_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(root, "a.md", "alpha beta alpha\n"),
            file(root, "b.md", "alpha gamma\n"),
        ];
        let first = search_files(&files, "alpha", &SearchOptions::default());
        let second = search_files(&files, "alpha", &SearchOptions::default());
        assert_eq!(paths(&first), paths(&second));
        let first_scores: Vec<u32> = first.iter().map(|r| r.score).collect();
        let second_scores: Vec<u32> = second.iter().map(|r| r.score).collect();
        assert_eq!(first_scores, second_scores);
    }

    #[test]
    fn empty_query_returns_no_results() {
        assert!(search_files(&[], "  ", &SearchOptions::default()).is_empty());
    }

    /// A query made only of conversational filler has no subject and must
    /// not match everything in the repository.
    #[test]
    fn a_filler_only_query_returns_no_results() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(tmp.path(), "a.md", "the thing that does the work\n")];
        assert!(
            search_files(
                &files,
                "please explain how this works",
                &SearchOptions::default()
            )
            .is_empty()
        );
    }

    // --- the regression this rewrite exists for --------------------------

    /// The exact failure reported from a live session: no line contains
    /// the literal string "session architecture", but the repository is
    /// full of relevant material.
    #[test]
    fn a_multi_word_query_matches_without_an_exact_substring() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(
                root,
                "docs/SESSIONS.md",
                "# Sessions\n\nA session is a stateful conversation.\n\
                 The session runtime owns conversation state and turn orchestration.\n",
            ),
            file(
                root,
                "crates/session/src/runtime.rs",
                "pub struct SessionRuntime {\n    state: SessionState,\n}\n",
            ),
            file(root, "docs/UNRELATED.md", "# Colours\n\nNothing to see.\n"),
        ];
        let results = search_files(&files, "session architecture", &SearchOptions::default());

        assert!(!results.is_empty(), "must not come back empty");
        let found = paths(&results);
        assert!(
            found.iter().any(|p| p.contains("SESSIONS.md")),
            "expected session documentation: {found:?}"
        );
        assert!(
            found.iter().any(|p| p.contains("runtime.rs")),
            "expected session implementation: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.contains("UNRELATED")),
            "unrelated files must not match: {found:?}"
        );
    }

    #[test]
    fn matches_camel_case_identifiers_from_prose_words() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "runtime.rs",
            "pub struct CancellationToken;\npub fn cancel_current_turn() {}\n",
        )];
        let results = search_files(&files, "cancellation", &SearchOptions::default());
        assert!(!results.is_empty());
        // Both the type and the function are about cancelling.
        assert!(results.len() >= 2, "{:?}", paths(&results));
    }

    /// The spec's example: a query must match a longer phrase containing
    /// its words.
    #[test]
    fn ai_assistant_matches_ai_engineering_assistant() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![
            file(
                tmp.path(),
                "README.md",
                "Orbit is a local-first AI engineering assistant.\n",
            ),
            file(tmp.path(), "other.md", "nothing relevant\n"),
        ];
        let results = search_files(&files, "AI assistant", &SearchOptions::default());
        assert!(
            paths(&results).iter().any(|p| p == "README.md"),
            "{:?}",
            paths(&results)
        );
    }

    #[test]
    fn stemming_connects_selected_and_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "adr.md",
            "STM32 selection rationale: low power draw.\n",
        )];
        let results = search_files(&files, "Why was STM32 selected?", &SearchOptions::default());
        assert!(!results.is_empty(), "`selected` must reach `selection`");
    }

    #[test]
    fn prefers_lines_covering_more_query_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "notes.md",
            "session session session session\nturn cancellation in the session runtime\n",
        )];
        let results = search_files(&files, "session cancellation", &SearchOptions::default());
        assert_eq!(
            results[0].source.line_start,
            Some(2),
            "a line matching both terms must beat one repeating a single term"
        );
    }

    #[test]
    fn an_exact_phrase_outranks_scattered_words() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![
            file(
                tmp.path(),
                "a.md",
                "the session architecture is described here\n",
            ),
            file(
                tmp.path(),
                "b.md",
                "session handling\narchitecture overview\n",
            ),
        ];
        let results = search_files(&files, "session architecture", &SearchOptions::default());
        assert_eq!(results[0].source.path, PathBuf::from("a.md"));
        assert!(results[0].components.phrase > 0.0);
    }

    #[test]
    fn matches_path_components() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "crates/session/src/lib.rs",
            "// nothing relevant in the body\n",
        )];
        let results = search_files(&files, "session", &SearchOptions::default());
        assert!(!results.is_empty(), "a path component must be matchable");
    }

    #[test]
    fn rewards_rust_definitions_over_incidental_uses() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "lib.rs",
            "// a comment mentioning watchdog\nlet x = watchdog_value + 1;\npub fn watchdog() {}\n",
        )];
        let results = search_files(&files, "watchdog", &SearchOptions::default());
        assert_eq!(
            results[0].source.line_start,
            Some(3),
            "the definition should rank first: {results:?}"
        );
        assert!(results[0].components.symbol > 0.0);
    }

    /// Inverse document frequency must actually discriminate: a term in
    /// every file is worth less than one in a single file.
    #[test]
    fn idf_prefers_distinctive_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = vec![
            file(root, "a.md", "common common brownout\n"),
            file(root, "b.md", "common word\n"),
            file(root, "c.md", "common word\n"),
            file(root, "d.md", "common word\n"),
        ];
        let results = search_files(&files, "common brownout", &SearchOptions::default());
        assert_eq!(
            results[0].source.path,
            PathBuf::from("a.md"),
            "the distinctive term must dominate the ubiquitous one"
        );
    }

    #[test]
    fn respects_the_per_file_match_limit_and_global_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let content = "target\n".repeat(50);
        let files = vec![file(tmp.path(), "many.txt", &content)];
        let options = SearchOptions {
            limit: 3,
            max_matches_per_file: 2,
            ..SearchOptions::default()
        };
        let results = search_files(&files, "target", &options);
        assert!(results.len() <= 3);
    }

    #[test]
    fn score_components_explain_the_ranking() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![file(
            tmp.path(),
            "docs/SESSIONS.md",
            "# Session architecture\n\nbody\n",
        )];
        let results = search_files(&files, "session architecture", &SearchOptions::default());
        let top = &results[0];
        assert!(top.components.lexical > 0.0);
        assert!(top.components.coverage > 0.0);
        assert!(top.components.heading > 0.0);
        assert!(!top.components.matched_terms.is_empty());
        assert_eq!(top.score, scale(top.components.total()));
    }
}
