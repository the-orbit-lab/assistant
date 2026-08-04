//! A Rust symbol index.
//!
//! Built with [`syn`], not regular expressions. A question naming a type
//! must reach the line that *declares* it, and telling a declaration from
//! a mention requires understanding the grammar — `pub struct Foo`,
//! `impl Foo`, and `let x: Foo` are three very different pieces of
//! evidence that look similar to a pattern match.
//!
//! Identifiers are matched **exactly**, or by a normalization that only
//! removes case and separators (`SessionRuntime` ≡ `session runtime` ≡
//! `session_runtime`). Natural-language stemming is deliberately not
//! applied to source identifiers: it would merge `Session` with
//! `Sessions` and `cancel` with `cancellation`, which is right for prose
//! and wrong for symbols.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orbit_project::DiscoveredFile;
use syn::spanned::Spanned;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolKind {
    Struct,
    Enum,
    Trait,
    Impl,
    Fn,
    Mod,
    Type,
    Const,
    Static,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Fn => "fn",
            SymbolKind::Mod => "mod",
            SymbolKind::Type => "type",
            SymbolKind::Const => "const",
            SymbolKind::Static => "static",
        }
    }

    /// Whether this kind *defines* the named thing, as opposed to
    /// attaching behavior to it. A question about `SessionRuntime` wants
    /// the `struct` before the `impl`, and both before a free function.
    pub fn is_definition(self) -> bool {
        matches!(
            self,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Trait | SymbolKind::Type
        )
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Project-relative path.
    pub path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    /// Enclosing `mod` chain, outermost first.
    pub module_path: Vec<String>,
    /// Doc comment attached to the item, if any.
    pub docs: String,
}

impl Symbol {
    /// A short human-readable description for evidence excerpts.
    pub fn signature(&self) -> String {
        let module = if self.module_path.is_empty() {
            String::new()
        } else {
            format!("{}::", self.module_path.join("::"))
        };
        format!("{} {module}{}", self.kind.as_str(), self.name)
    }
}

/// Case- and separator-insensitive form used for symbol lookup.
///
/// Only case and separators are removed — never suffixes — so
/// `SessionRuntime`, `session_runtime`, and `session runtime` all agree
/// while `Session` and `Sessions` stay distinct.
pub fn normalize_symbol(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    symbols: Vec<Symbol>,
    by_exact: HashMap<String, Vec<usize>>,
    by_normalized: HashMap<String, Vec<usize>>,
}

impl SymbolIndex {
    /// Parse every Rust file in `files`. A file that fails to parse is
    /// skipped rather than failing the build: an index that covers most
    /// of a repository is far better than none, and unparseable input
    /// (a partial edit, a future syntax) must not break retrieval.
    pub fn build(files: &[DiscoveredFile]) -> Self {
        let sources: Vec<(PathBuf, String)> = files
            .iter()
            .filter(|file| is_rust(&file.relative_path))
            .filter_map(|file| {
                let text = std::fs::read_to_string(&file.absolute_path).ok()?;
                Some((file.relative_path.clone(), text))
            })
            .collect();
        Self::from_sources(
            sources
                .iter()
                .map(|(path, text)| (path.as_path(), text.as_str())),
        )
    }

    /// Build from already-read sources.
    ///
    /// The retrieval pipeline reads the corpus once and shares it between
    /// layers; re-reading every Rust file here purely to parse it would
    /// double the I/O of a question for no benefit. Non-Rust paths are
    /// ignored, so a caller can pass its whole corpus.
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a Path, &'a str)>) -> Self {
        let mut index = SymbolIndex::default();
        for (path, text) in sources {
            if !is_rust(path) {
                continue;
            }
            match syn::parse_file(text) {
                Ok(parsed) => {
                    let mut found = Vec::new();
                    collect_items(&parsed.items, &mut Vec::new(), &mut found);
                    for mut symbol in found {
                        symbol.path = path.to_path_buf();
                        index.push(symbol);
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %err,
                        "skipping unparseable Rust file in symbol index"
                    );
                }
            }
        }
        index
    }

    fn push(&mut self, symbol: Symbol) {
        let position = self.symbols.len();
        self.by_exact
            .entry(symbol.name.clone())
            .or_default()
            .push(position);
        self.by_normalized
            .entry(normalize_symbol(&symbol.name))
            .or_default()
            .push(position);
        self.symbols.push(symbol);
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// Symbols whose name or documentation matches the given concept
    /// terms, best first.
    ///
    /// [`lookup`](Self::lookup) answers "where is this identifier
    /// declared" and needs the identifier. A behavior question does not
    /// have one: "How is cancellation checked during model streaming?"
    /// names `cancel`, `stream`, and `check` — concepts, not symbols. So
    /// this matches the *words inside* identifiers, which is how
    /// `cancel_current_turn`, `CancellationToken`, and `chat_streaming`
    /// become reachable from a question that names none of them.
    ///
    /// Scoring is deliberately simple and explainable: a term matched in
    /// the identifier counts double a term matched only in its doc
    /// comment, because a name is what the author chose to call the
    /// thing. Ties break on kind then position, so the result is stable.
    pub fn search_by_terms(&self, terms: &[String]) -> Vec<&Symbol> {
        if terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(usize, usize, usize)> = Vec::new();
        for (position, symbol) in self.symbols.iter().enumerate() {
            let name_terms = orbit_project::content_terms(&symbol.name);
            let doc_terms = orbit_project::content_terms(&symbol.docs);

            let name_hits = terms.iter().filter(|t| name_terms.contains(t)).count();
            let doc_hits = terms
                .iter()
                .filter(|t| doc_terms.contains(t) && !name_terms.contains(t))
                .count();
            let score = name_hits * 2 + doc_hits;
            if score == 0 {
                continue;
            }

            // Behavior lives in code that runs. A `mod` matching a term
            // says only that the module is named for the subject, which
            // the path generator already reports.
            let kind_rank = match symbol.kind {
                SymbolKind::Fn => 0,
                SymbolKind::Enum | SymbolKind::Struct | SymbolKind::Trait | SymbolKind::Type => 1,
                SymbolKind::Const | SymbolKind::Static => 2,
                SymbolKind::Impl => 3,
                SymbolKind::Mod => 4,
            };
            scored.push((score, kind_rank, position));
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| self.symbols[a.2].path.cmp(&self.symbols[b.2].path))
                .then_with(|| a.2.cmp(&b.2))
        });
        scored
            .into_iter()
            .map(|(_, _, position)| &self.symbols[position])
            .collect()
    }

    /// The smallest symbol whose span contains `line` in `path`.
    ///
    /// Maps a line of code back to the function that runs it. A
    /// cancellation *check* is a line inside a body -- `if
    /// cancel.is_cancelled() { ... }` -- and quoting that line alone
    /// explains nothing; quoting the function it guards explains the
    /// mechanism.
    pub fn enclosing(&self, path: &Path, line: usize) -> Option<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| {
                s.path == path
                    && s.line_start <= line
                    && s.line_end >= line
                    // An `impl` block or `mod` encloses everything in it,
                    // which makes it the least informative answer to
                    // "which code does this".
                    && !matches!(s.kind, SymbolKind::Impl | SymbolKind::Mod)
            })
            .min_by_key(|s| s.line_end - s.line_start)
    }

    /// Whether the index declares `name` anywhere.
    ///
    /// Used to tell a symbol this repository actually defines from one a
    /// model invented while filling a gap in its evidence.
    pub fn declares(&self, name: &str) -> bool {
        self.by_exact.contains_key(name)
    }

    /// Every symbol declared in `path`.
    pub fn in_file(&self, path: &Path) -> Vec<&Symbol> {
        self.symbols.iter().filter(|s| s.path == path).collect()
    }

    /// Every symbol matching `query`, definitions first.
    ///
    /// Exact-case matches are preferred over normalized ones, and within
    /// each group a declaration outranks an `impl`, which outranks a
    /// function — the order in which a reader would want to see them.
    pub fn lookup(&self, query: &str) -> Vec<&Symbol> {
        let mut positions: Vec<usize> = Vec::new();
        if let Some(exact) = self.by_exact.get(query) {
            positions.extend(exact.iter().copied());
        }
        if let Some(normalized) = self.by_normalized.get(&normalize_symbol(query)) {
            for position in normalized {
                if !positions.contains(position) {
                    positions.push(*position);
                }
            }
        }

        let mut matches: Vec<&Symbol> = positions.into_iter().map(|p| &self.symbols[p]).collect();
        matches.sort_by_key(|symbol| {
            (
                // Definitions, then impls, then everything else.
                match symbol.kind {
                    k if k.is_definition() => 0,
                    SymbolKind::Impl => 1,
                    _ => 2,
                },
                // Exact case before normalized-only.
                if symbol.name == query { 0 } else { 1 },
                symbol.path.clone(),
                symbol.line_start,
            )
        });
        matches
    }

    /// Whether any symbol matches, used by the planner to decide that a
    /// word in a question is a real entity rather than ordinary prose.
    pub fn contains(&self, query: &str) -> bool {
        self.by_exact.contains_key(query)
            || self.by_normalized.contains_key(&normalize_symbol(query))
    }
}

fn is_rust(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("rs"))
        .unwrap_or(false)
}

fn doc_comment(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(expr) = &nv.value
            && let syn::Lit::Str(text) = &expr.lit
        {
            lines.push(text.value().trim().to_string());
        }
    }
    lines.join(" ")
}

fn span_lines(span: proc_macro2::Span) -> (usize, usize) {
    (span.start().line, span.end().line)
}

/// The name an `impl` block attaches behavior to (`impl SessionRuntime`,
/// `impl Action for SearchAction` → `SearchAction`).
fn impl_self_name(item: &syn::ItemImpl) -> Option<String> {
    match &*item.self_ty {
        syn::Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn collect_items(items: &[syn::Item], module_path: &mut Vec<String>, out: &mut Vec<Symbol>) {
    for item in items {
        let mut push = |name: String, kind: SymbolKind, attrs: &[syn::Attribute], span| {
            let (line_start, line_end) = span_lines(span);
            out.push(Symbol {
                name,
                kind,
                path: PathBuf::new(),
                line_start,
                line_end,
                module_path: module_path.clone(),
                docs: doc_comment(attrs),
            });
        };

        match item {
            syn::Item::Struct(i) => {
                push(i.ident.to_string(), SymbolKind::Struct, &i.attrs, i.span())
            }
            syn::Item::Enum(i) => push(i.ident.to_string(), SymbolKind::Enum, &i.attrs, i.span()),
            syn::Item::Trait(i) => push(i.ident.to_string(), SymbolKind::Trait, &i.attrs, i.span()),
            syn::Item::Fn(i) => push(
                i.sig.ident.to_string(),
                SymbolKind::Fn,
                &i.attrs,
                i.sig.span(),
            ),
            syn::Item::Type(i) => push(i.ident.to_string(), SymbolKind::Type, &i.attrs, i.span()),
            syn::Item::Const(i) => push(i.ident.to_string(), SymbolKind::Const, &i.attrs, i.span()),
            syn::Item::Static(i) => {
                push(i.ident.to_string(), SymbolKind::Static, &i.attrs, i.span())
            }
            syn::Item::Impl(i) => {
                if let Some(name) = impl_self_name(i) {
                    push(name.clone(), SymbolKind::Impl, &i.attrs, i.span());
                    // Methods are indexed under their own names too, so
                    // "how does cancel_current_turn work" finds the body.
                    module_path.push(name);
                    for impl_item in &i.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            // The method's *whole* span, body included.
                            // Indexing only the signature made it
                            // impossible to map a line of code back to
                            // the method that runs it, so a cancellation
                            // check inside a body was invisible to
                            // `enclosing` and a behavior question could
                            // never reach the code performing it.
                            let (line_start, line_end) = span_lines(method.span());
                            out.push(Symbol {
                                name: method.sig.ident.to_string(),
                                kind: SymbolKind::Fn,
                                path: PathBuf::new(),
                                line_start,
                                line_end,
                                module_path: module_path.clone(),
                                docs: doc_comment(&method.attrs),
                            });
                        }
                    }
                    module_path.pop();
                }
            }
            syn::Item::Mod(i) => {
                push(i.ident.to_string(), SymbolKind::Mod, &i.attrs, i.span());
                if let Some((_, inner)) = &i.content {
                    module_path.push(i.ident.to_string());
                    collect_items(inner, module_path, out);
                    module_path.pop();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn discovered(root: &Path, relative: &str, content: &str) -> DiscoveredFile {
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

    fn index_of(content: &str) -> (tempfile::TempDir, SymbolIndex) {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![discovered(tmp.path(), "src/session.rs", content)];
        let index = SymbolIndex::build(&files);
        (tmp, index)
    }

    const SAMPLE: &str = r#"
//! Module docs.

/// A stateful conversation.
pub struct SessionRuntime {
    state: SessionState,
}

impl SessionRuntime {
    /// Cancel the running turn.
    pub fn cancel_current_turn(&self) -> bool { true }
}

pub enum ExecutionState { Idle }

pub trait ModelProvider {}

pub mod inner {
    pub struct Nested;
}

pub const LIMIT: usize = 4;
pub type Alias = usize;
"#;

    #[test]
    fn extracts_every_supported_item_kind() {
        let (_tmp, index) = index_of(SAMPLE);
        let kinds: Vec<&str> = index
            .symbols()
            .iter()
            .map(|s| s.kind.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for expected in [
            "struct", "enum", "trait", "impl", "fn", "mod", "const", "type",
        ] {
            assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
        }
    }

    #[test]
    fn a_definition_outranks_its_impl_and_methods() {
        let (_tmp, index) = index_of(SAMPLE);
        let found = index.lookup("SessionRuntime");
        assert_eq!(found[0].kind, SymbolKind::Struct);
        assert_eq!(found[1].kind, SymbolKind::Impl);
    }

    #[test]
    fn records_line_ranges_and_paths() {
        let (_tmp, index) = index_of(SAMPLE);
        let found = index.lookup("SessionRuntime");
        assert_eq!(found[0].path, PathBuf::from("src/session.rs"));
        assert!(found[0].line_start > 0);
        assert!(found[0].line_end >= found[0].line_start);
    }

    #[test]
    fn captures_doc_comments() {
        let (_tmp, index) = index_of(SAMPLE);
        let found = index.lookup("SessionRuntime");
        assert!(
            found[0].docs.contains("stateful conversation"),
            "{:?}",
            found[0].docs
        );
    }

    /// The normalized forms the spec requires, and only those.
    #[test]
    fn matches_normalized_identifier_forms() {
        let (_tmp, index) = index_of(SAMPLE);
        for query in [
            "SessionRuntime",
            "session runtime",
            "session_runtime",
            "sessionruntime",
        ] {
            assert!(
                !index.lookup(query).is_empty(),
                "`{query}` should reach SessionRuntime"
            );
        }
    }

    /// Stemming must never be applied to identifiers: it would merge
    /// distinct types.
    #[test]
    fn does_not_stem_identifiers() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![discovered(
            tmp.path(),
            "src/a.rs",
            "pub struct Session; pub struct Sessions;",
        )];
        let index = SymbolIndex::build(&files);
        let session = index.lookup("Session");
        assert_eq!(session.len(), 1, "Session and Sessions must stay distinct");
        assert_eq!(session[0].name, "Session");
    }

    #[test]
    fn records_module_paths_and_method_owners() {
        let (_tmp, index) = index_of(SAMPLE);
        let nested = index.lookup("Nested");
        assert_eq!(nested[0].module_path, vec!["inner".to_string()]);

        let method = index.lookup("cancel_current_turn");
        assert_eq!(method[0].module_path, vec!["SessionRuntime".to_string()]);
        assert!(
            method[0]
                .signature()
                .contains("SessionRuntime::cancel_current_turn")
        );
    }

    #[test]
    fn indexes_the_type_an_impl_block_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![discovered(
            tmp.path(),
            "src/a.rs",
            "pub struct SearchAction;\nimpl Action for SearchAction {}\n",
        )];
        let index = SymbolIndex::build(&files);
        let found = index.lookup("SearchAction");
        assert!(found.iter().any(|s| s.kind == SymbolKind::Impl));
    }

    /// An unparseable file must not break the whole index.
    #[test]
    fn skips_unparseable_files_without_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![
            discovered(tmp.path(), "src/broken.rs", "pub struct {{{ not rust"),
            discovered(tmp.path(), "src/good.rs", "pub struct Good;"),
        ];
        let index = SymbolIndex::build(&files);
        assert!(!index.lookup("Good").is_empty());
    }

    #[test]
    fn ignores_non_rust_files() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![discovered(
            tmp.path(),
            "docs/SESSIONS.md",
            "pub struct SessionRuntime {}",
        )];
        assert!(SymbolIndex::build(&files).is_empty());
    }

    #[test]
    fn normalization_only_removes_case_and_separators() {
        assert_eq!(normalize_symbol("SessionRuntime"), "sessionruntime");
        assert_eq!(normalize_symbol("session_runtime"), "sessionruntime");
        assert_eq!(normalize_symbol("session runtime"), "sessionruntime");
        assert_ne!(normalize_symbol("Session"), normalize_symbol("Sessions"));
    }
}
