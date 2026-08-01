//! Candidate generation: several independent ways of proposing evidence.
//!
//! Each generator answers a different question, and none of them is
//! authoritative:
//!
//! | Generator | Answers |
//! |---|---|
//! | lexical | which lines mention these words |
//! | symbol  | where is this identifier declared |
//! | path    | which files are *named* after the subject |
//! | heading | which documentation section is titled after it |
//! | context | what was this conversation already looking at |
//!
//! Keeping them separate is the point. A single scoring function that
//! tried to weigh "mentions the words a lot" against "declares the type"
//! has to pick one scale for two incomparable things, and the verbose
//! document always wins. Independent rankings fused by rank (see
//! [`crate::fusion`]) instead reward a file that several generators agree
//! on, which is a much better proxy for "this is about the subject".
//!
//! Generators never invent a path. Every candidate they emit points at a
//! file that came from project discovery, so nothing outside the security
//! boundary can enter the pipeline here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use orbit_project::{DiscoveredFile, LexicalIndex, SearchOptions, content_terms, tokenize};

use crate::plan::RetrievalPlan;
use crate::symbols::{SymbolIndex, SymbolKind};

/// Which generator proposed a candidate.
///
/// Fusion uses this to keep one generator from dominating, and the
/// diagnostics use it to explain where a piece of evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CandidateOrigin {
    Lexical,
    Symbol,
    Path,
    Heading,
    Context,
}

impl CandidateOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateOrigin::Lexical => "lexical",
            CandidateOrigin::Symbol => "symbol",
            CandidateOrigin::Path => "path",
            CandidateOrigin::Heading => "heading",
            CandidateOrigin::Context => "context",
        }
    }
}

/// A proposed piece of evidence: a location in a discovered file.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Project-relative path, always from discovery.
    pub path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    /// A short quotation of what was matched, for diagnostics and for the
    /// model when the full file is not read.
    pub excerpt: String,
    /// Section title, when the match sits under a markdown heading.
    pub section: Option<String>,
    pub origin: CandidateOrigin,
    /// The generator's own score, only meaningful within that generator.
    pub generator_score: f64,
    /// Set when the symbol generator produced this candidate.
    pub symbol_kind: Option<SymbolKind>,
    /// Set when this candidate *declares* the entity the plan asked for.
    pub defines_entity: Option<String>,
}

impl Candidate {
    /// The identity used for deduplication across generators: two
    /// candidates are the same evidence when they name the same file and
    /// overlapping lines.
    pub fn key(&self) -> (PathBuf, usize) {
        (self.path.clone(), self.line_start)
    }
}

/// What kind of file this is, as far as evidence selection cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Rust,
    Markdown,
    Config,
    Other,
}

/// A markdown heading and the line span of the section it opens.
#[derive(Debug, Clone)]
pub struct Heading {
    pub text: String,
    pub level: usize,
    pub line: usize,
    /// Last line of this section (exclusive of the next heading).
    pub section_end: usize,
}

/// Everything the pipeline needs to know about one file, computed once.
#[derive(Debug, Clone)]
pub struct FileFacts {
    pub path: PathBuf,
    pub content: String,
    pub line_count: usize,
    pub kind: FileKind,
    /// Stemmed terms from the file's basename.
    pub filename_terms: Vec<String>,
    /// Stemmed terms from the whole relative path.
    pub path_terms: Vec<String>,
    pub headings: Vec<Heading>,
    /// The first level-1 heading, which is what a document calls itself.
    pub title: Option<String>,
    /// For each term in this file, how many of its lines contain it.
    ///
    /// Precomputed because the alternative — re-tokenizing whole files
    /// once per candidate during reranking — is the single most expensive
    /// thing the pipeline could do, and it would repeat the same work for
    /// every candidate that shares a file.
    term_line_counts: HashMap<String, usize>,
}

impl FileFacts {
    pub fn line(&self, number: usize) -> &str {
        self.content
            .lines()
            .nth(number.saturating_sub(1))
            .unwrap_or("")
    }

    /// How many of this file's lines mention the subject.
    ///
    /// Taken as the maximum over the subject's terms rather than the size
    /// of their union: the union would need a per-term line set for every
    /// file, and the number is only ever compared against this same
    /// file's length, so the strongest single term is the honest measure
    /// of "how much of this file is about the subject".
    pub fn mention_lines(&self, terms: &[String]) -> usize {
        terms
            .iter()
            .filter_map(|term| self.term_line_counts.get(term))
            .copied()
            .max()
            .unwrap_or(0)
    }
}

/// The corpus, read once and shared by every layer.
pub struct Corpus {
    files: Vec<FileFacts>,
    symbols: SymbolIndex,
    lexical: LexicalIndex,
}

impl Corpus {
    /// Read and pre-analyze every discovered text file.
    ///
    /// Binary and unreadable files are kept with empty content so they
    /// stay findable by name, which is what a "where is X" question often
    /// actually needs.
    pub fn build(files: &[DiscoveredFile]) -> Self {
        let mut facts = Vec::with_capacity(files.len());
        for file in files {
            let content = if file.is_text {
                std::fs::read(&file.absolute_path)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            facts.push(file_facts(&file.relative_path, content));
        }

        let symbols =
            SymbolIndex::from_sources(facts.iter().map(|f| (f.path.as_path(), f.content.as_str())));

        Self {
            files: facts,
            symbols,
            lexical: LexicalIndex::build(files),
        }
    }

    pub fn files(&self) -> &[FileFacts] {
        &self.files
    }

    pub fn symbols(&self) -> &SymbolIndex {
        &self.symbols
    }

    pub fn lexical(&self) -> &LexicalIndex {
        &self.lexical
    }

    pub fn get(&self, path: &Path) -> Option<&FileFacts> {
        self.files.iter().find(|f| f.path == path)
    }
}

fn file_facts(path: &Path, content: String) -> FileFacts {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = match extension.as_str() {
        "rs" => FileKind::Rust,
        "md" | "markdown" => FileKind::Markdown,
        "yaml" | "yml" | "toml" | "json" => FileKind::Config,
        _ => FileKind::Other,
    };

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let filename_terms = tokenize(filename).into_iter().map(|t| t.stem).collect();
    let path_terms = tokenize(&path.to_string_lossy())
        .into_iter()
        .map(|t| t.stem)
        .collect();

    let line_count = content.lines().count();
    let mut term_line_counts: HashMap<String, usize> = HashMap::new();
    for line in content.lines() {
        let line_terms: HashSet<String> = tokenize(line).into_iter().map(|t| t.stem).collect();
        for term in line_terms {
            *term_line_counts.entry(term).or_default() += 1;
        }
    }

    let headings = if matches!(kind, FileKind::Markdown) {
        markdown_headings(&content, line_count)
    } else {
        Vec::new()
    };
    let title = headings
        .iter()
        .find(|h| h.level == 1)
        .map(|h| h.text.clone());

    FileFacts {
        path: path.to_path_buf(),
        content,
        line_count,
        kind,
        filename_terms,
        path_terms,
        headings,
        title,
        term_line_counts,
    }
}

/// Headings with the line span of the section each one opens.
///
/// A section ends at the next heading of the same or higher level, so
/// quoting "the section titled X" quotes what a reader would consider
/// that section — not the rest of the document.
fn markdown_headings(content: &str, line_count: usize) -> Vec<Heading> {
    let mut headings: Vec<Heading> = Vec::new();
    let mut in_fence = false;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !trimmed.starts_with('#') {
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        let text = trimmed.trim_start_matches('#').trim().to_string();
        if text.is_empty() {
            continue;
        }
        let line_number = index + 1;
        // Close every open section this heading terminates.
        for open in headings.iter_mut().rev() {
            if open.section_end == usize::MAX && open.level >= level {
                open.section_end = line_number.saturating_sub(1);
            }
        }
        headings.push(Heading {
            text,
            level,
            line: line_number,
            section_end: usize::MAX,
        });
    }
    for heading in &mut headings {
        if heading.section_end == usize::MAX {
            heading.section_end = line_count;
        }
    }
    headings
}

/// How many candidates any single generator may contribute.
///
/// Fusion cares about rank, not volume, so a long tail from one generator
/// adds noise without adding agreement.
const PER_GENERATOR_LIMIT: usize = 24;

/// Lexical candidates: BM25 over the plan's text queries.
///
/// This is the existing deterministic search, used unchanged. It is the
/// broadest generator and the one most prone to favoring verbose files —
/// which is exactly why it is only one input among five.
pub fn lexical_candidates(corpus: &Corpus, plan: &RetrievalPlan) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let options = SearchOptions {
        limit: PER_GENERATOR_LIMIT,
        max_excerpt_chars: 240,
        max_matches_per_file: 3,
    };

    {
        for result in
            corpus
                .lexical()
                .search_terms(&plan.lexical_terms, plan.phrase.as_deref(), &options)
        {
            let line_start = result.source.line_start.unwrap_or(1);
            let candidate = Candidate {
                path: result.source.path.clone(),
                line_start,
                line_end: result.source.line_end.unwrap_or(line_start),
                excerpt: result.excerpt.clone(),
                section: result.source.section.clone(),
                origin: CandidateOrigin::Lexical,
                generator_score: result.components.total(),
                symbol_kind: None,
                defines_entity: None,
            };
            if !candidates.iter().any(|c| c.key() == candidate.key()) {
                candidates.push(candidate);
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.generator_score
            .partial_cmp(&a.generator_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    candidates.truncate(PER_GENERATOR_LIMIT);
    candidates
}

/// Symbol candidates: where the named identifiers are actually declared.
///
/// This generator is the direct answer to the reported failure. Asked
/// about `SessionRuntime`, it proposes the struct and its `impl` block —
/// evidence a term-frequency ranking can never rank first, because a
/// declaration mentions its own name exactly once.
pub fn symbol_candidates(corpus: &Corpus, plan: &RetrievalPlan) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();

    for query in &plan.symbol_queries {
        for (rank, symbol) in corpus.symbols().lookup(query).into_iter().enumerate() {
            // `lookup` already orders definitions before impls before
            // everything else; keep that order and let later matches for
            // the same query decay.
            let score = 1.0 / (1.0 + rank as f64);
            let candidate = Candidate {
                path: symbol.path.clone(),
                line_start: symbol.line_start,
                line_end: symbol.line_end,
                excerpt: symbol.signature(),
                section: None,
                origin: CandidateOrigin::Symbol,
                generator_score: score,
                symbol_kind: Some(symbol.kind),
                defines_entity: Some(query.clone()),
            };
            if !candidates.iter().any(|c| c.key() == candidate.key()) {
                candidates.push(candidate);
            }
            if candidates.len() >= PER_GENERATOR_LIMIT {
                break;
            }
        }
    }

    candidates
}

/// Path candidates: files whose *name* is about the subject.
///
/// `crates/session/src/session.rs` and `docs/SESSIONS.md` are both named
/// after sessions. A file named after a subject is usually about it,
/// independently of how many times the word appears inside.
pub fn path_candidates(corpus: &Corpus, plan: &RetrievalPlan) -> Vec<Candidate> {
    let terms = plan_terms(plan);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(f64, &FileFacts)> = Vec::new();
    for file in corpus.files() {
        let filename_hits = terms
            .iter()
            .filter(|t| file.filename_terms.contains(t))
            .count();
        let path_hits = terms.iter().filter(|t| file.path_terms.contains(t)).count();
        if filename_hits == 0 && path_hits == 0 {
            continue;
        }
        // A filename match is much stronger evidence than a directory
        // component: every file under `crates/session/` matches `session`
        // on path, but only a few are named for it.
        let score = filename_hits as f64 * 2.0 + path_hits as f64;
        let coverage = (filename_hits.max(path_hits)) as f64 / terms.len() as f64;
        scored.push((score + coverage, file));
    }

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.path.cmp(&b.1.path))
    });

    scored
        .into_iter()
        .take(PER_GENERATOR_LIMIT)
        .map(|(score, file)| Candidate {
            path: file.path.clone(),
            line_start: 1,
            line_end: file.line_count.max(1),
            excerpt: file
                .title
                .clone()
                .unwrap_or_else(|| file.path.to_string_lossy().to_string()),
            section: None,
            origin: CandidateOrigin::Path,
            generator_score: score,
            symbol_kind: None,
            defines_entity: None,
        })
        .collect()
}

/// Heading candidates: the documentation section titled after the subject.
///
/// A heading names its section, so a matching heading proposes that
/// section rather than the whole document — which is how a 500-line
/// architecture document can contribute the twenty lines that are about
/// the question without contributing the other 480.
pub fn heading_candidates(corpus: &Corpus, plan: &RetrievalPlan) -> Vec<Candidate> {
    let terms = plan_terms(plan);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(f64, &FileFacts, &Heading)> = Vec::new();
    for file in corpus.files() {
        for heading in &file.headings {
            let heading_terms: Vec<String> = content_terms(&heading.text);
            if heading_terms.is_empty() {
                continue;
            }
            let hits = terms.iter().filter(|t| heading_terms.contains(t)).count();
            if hits == 0 {
                continue;
            }
            // Reward matching a large share of both the query and the
            // heading: "Cancellation" matched by [cancel] is a better
            // section than "Sessions, events, and cancellation".
            let query_coverage = hits as f64 / terms.len() as f64;
            let heading_precision = hits as f64 / heading_terms.len() as f64;
            // A shallower heading covers a broader section.
            let depth_weight = 1.0 / (heading.level.max(1) as f64).sqrt();
            scored.push((
                (query_coverage + heading_precision) * depth_weight,
                file,
                heading,
            ));
        }
    }

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.2.line.cmp(&b.2.line))
    });

    scored
        .into_iter()
        .take(PER_GENERATOR_LIMIT)
        .map(|(score, file, heading)| Candidate {
            path: file.path.clone(),
            line_start: heading.line,
            line_end: heading.section_end.max(heading.line),
            excerpt: heading.text.clone(),
            section: Some(heading.text.clone()),
            origin: CandidateOrigin::Heading,
            generator_score: score,
            symbol_kind: None,
            defines_entity: None,
        })
        .collect()
}

/// Context candidates: files this conversation already cited.
///
/// A follow-up question ("and how does it store state?") often has too
/// little of its own to retrieve on. Re-proposing what the previous turn
/// actually used keeps the thread anchored — and only paths that came
/// from real retrieval are eligible, so nothing the model merely
/// *mentioned* can become trusted evidence here.
pub fn context_candidates(corpus: &Corpus, recent_paths: &[PathBuf]) -> Vec<Candidate> {
    recent_paths
        .iter()
        .enumerate()
        .filter_map(|(rank, path)| {
            let file = corpus.get(path)?;
            Some(Candidate {
                path: file.path.clone(),
                line_start: 1,
                line_end: file.line_count.max(1),
                excerpt: file
                    .title
                    .clone()
                    .unwrap_or_else(|| file.path.to_string_lossy().to_string()),
                section: None,
                origin: CandidateOrigin::Context,
                generator_score: 1.0 / (1.0 + rank as f64),
                symbol_kind: None,
                defines_entity: None,
            })
        })
        .take(PER_GENERATOR_LIMIT)
        .collect()
}

/// The plan's terms as normalized stems: entity words plus concepts.
pub fn plan_terms(plan: &RetrievalPlan) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for entity in &plan.entities {
        for term in content_terms(entity) {
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
    }
    for concept in &plan.concepts {
        if !terms.contains(concept) {
            terms.push(concept.clone());
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Vec<DiscoveredFile>) {
        let dir = TempDir::new().unwrap();
        let write = |relative: &str, body: &str| {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
        };

        write(
            "crates/session/src/session.rs",
            "//! Session runtime.\n\
             pub struct SessionRuntime {\n\
             \x20   history: Vec<String>,\n\
             }\n\
             \n\
             impl SessionRuntime {\n\
             \x20   pub fn new() -> Self {\n\
             \x20       Self { history: Vec::new() }\n\
             \x20   }\n\
             }\n",
        );
        write(
            "docs/SESSIONS.md",
            "# Sessions\n\
             \n\
             A session keeps conversation state in memory.\n\
             \n\
             ## Cancellation\n\
             \n\
             A running turn can be cancelled.\n\
             \n\
             ## Storage\n\
             \n\
             Nothing is written to disk.\n",
        );
        // The document that caused the reported failure: long, about
        // something else, and repeating the question's words often enough
        // to out-score the file that actually defines the subject.
        let mut search_doc = String::from("# Search and retrieval\n\n");
        for section in 0..6 {
            search_doc.push_str(&format!("## Ranking stage {section}\n\n"));
            search_doc.push_str(
                "Search ranks lines by BM25 over normalized tokens.\n\
                 Ranking is deterministic and reproducible across runs.\n\
                 The session runtime calls search to build session state.\n\
                 Scores are scaled to integers so ordering never drifts.\n\
                 Excerpts are truncated per file before global ranking.\n\n",
            );
        }
        write("docs/SEARCH.md", &search_doc);

        let files: Vec<DiscoveredFile> = [
            "crates/session/src/session.rs",
            "docs/SESSIONS.md",
            "docs/SEARCH.md",
        ]
        .iter()
        .map(|relative| {
            let absolute = dir.path().join(relative);
            DiscoveredFile {
                relative_path: PathBuf::from(relative),
                size: fs::metadata(&absolute).unwrap().len(),
                absolute_path: absolute,
                is_text: true,
            }
        })
        .collect();

        (dir, files)
    }

    fn plan_for(question: &str, corpus: &Corpus) -> RetrievalPlan {
        plan::plan(question, &[], &[], Some(corpus.symbols()))
    }

    #[test]
    fn the_symbol_generator_finds_the_definition_the_lexical_one_misses() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let plan = plan_for(
            "Explain SessionRuntime and how it stores session state.",
            &corpus,
        );

        let symbol = symbol_candidates(&corpus, &plan);
        assert_eq!(
            symbol[0].path,
            PathBuf::from("crates/session/src/session.rs")
        );
        assert_eq!(symbol[0].symbol_kind, Some(SymbolKind::Struct));
        assert_eq!(symbol[0].defines_entity.as_deref(), Some("SessionRuntime"));
    }

    /// The reported failure, at the generator level: a verbose document
    /// about something else earns lexical candidates by repeating the
    /// question's words, and earns none from the generator that asks
    /// where the subject is declared.
    #[test]
    fn a_verbose_unrelated_document_is_lexical_only() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let plan = plan_for(
            "Explain SessionRuntime and how it stores session state.",
            &corpus,
        );

        let lexical = lexical_candidates(&corpus, &plan);
        let symbol = symbol_candidates(&corpus, &plan);
        assert!(!lexical.is_empty() && !symbol.is_empty());

        let in_lexical = |name: &str| lexical.iter().any(|c| c.path.ends_with(name));
        let in_symbol = |name: &str| symbol.iter().any(|c| c.path.ends_with(name));

        assert!(in_lexical("SEARCH.md"), "{lexical:?}");
        assert!(!in_symbol("SEARCH.md"), "{symbol:?}");
        assert!(in_symbol("session.rs"));
    }

    #[test]
    fn path_candidates_prefer_a_filename_match_over_a_directory_match() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let plan = plan_for("Explain the session architecture.", &corpus);

        let paths = path_candidates(&corpus, &plan);
        assert!(!paths.is_empty());
        // SESSIONS.md and session.rs are named for the subject; SEARCH.md
        // is not, and must not appear at all.
        assert!(
            !paths.iter().any(|c| c.path.ends_with("SEARCH.md")),
            "{paths:?}"
        );
    }

    #[test]
    fn heading_candidates_return_a_section_not_a_whole_document() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let plan = plan_for("How does cancellation work?", &corpus);

        let headings = heading_candidates(&corpus, &plan);
        let cancellation = headings
            .iter()
            .find(|c| c.section.as_deref() == Some("Cancellation"))
            .expect("cancellation section");
        assert_eq!(cancellation.path, PathBuf::from("docs/SESSIONS.md"));
        assert_eq!(cancellation.line_start, 5);
        // The section stops where the next heading begins, not at EOF.
        assert_eq!(cancellation.line_end, 8);
    }

    #[test]
    fn context_candidates_only_accept_discovered_paths() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);

        let candidates = context_candidates(
            &corpus,
            &[
                PathBuf::from("docs/SESSIONS.md"),
                // A path the model might have invented.
                PathBuf::from("src/imaginary.rs"),
            ],
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, PathBuf::from("docs/SESSIONS.md"));
    }

    #[test]
    fn headings_inside_code_fences_are_not_headings() {
        let facts = file_facts(
            Path::new("docs/X.md"),
            "# Real\n\n```\n# not a heading\n```\n\n## Also real\n".to_string(),
        );
        let titles: Vec<&str> = facts.headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(titles, vec!["Real", "Also real"]);
    }

    #[test]
    fn mention_counting_separates_a_subject_from_a_passing_reference() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let terms = vec!["session".to_string()];

        let sessions = corpus.get(Path::new("docs/SESSIONS.md")).unwrap();
        let search = corpus.get(Path::new("docs/SEARCH.md")).unwrap();
        let sessions_ratio = sessions.mention_lines(&terms) as f64 / sessions.line_count as f64;
        let search_ratio = search.mention_lines(&terms) as f64 / search.line_count as f64;
        assert!(
            sessions_ratio > search_ratio,
            "{sessions_ratio} {search_ratio}"
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let (_dir, files) = fixture();
        let corpus = Corpus::build(&files);
        let plan = plan_for("Explain SessionRuntime.", &corpus);

        let first: Vec<PathBuf> = lexical_candidates(&corpus, &plan)
            .into_iter()
            .map(|c| c.path)
            .collect();
        let second: Vec<PathBuf> = lexical_candidates(&corpus, &plan)
            .into_iter()
            .map(|c| c.path)
            .collect();
        assert_eq!(first, second);
    }
}
