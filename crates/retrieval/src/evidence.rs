//! Structured, AST-backed evidence for one named Rust symbol.
//!
//! The symbol index answers *where* a type is declared. That is not
//! enough to explain it. Asked "Explain SessionRuntime and how it stores
//! session state", a single line reading `pub struct SessionRuntime {`
//! tells a model nothing about the state it stores — so the model falls
//! back on whatever else is in its context, which is how an answer about
//! a type ends up describing the tests that exercise it.
//!
//! Answering that question needs the whole shape of the type:
//!
//! - the complete declaration, not the line the name appears on;
//! - every field, with its type and its doc comment;
//! - the documentation attached to the type itself;
//! - every `impl` block, inherent and trait;
//! - the methods, so "how does it store state" can be answered from the
//!   methods that touch that state.
//!
//! All of it comes from `syn`, so the spans are exact and a comment
//! mentioning `struct SessionRuntime` can never be mistaken for the
//! declaration.
//!
//! Which methods matter is decided by the *question*, not by a list of
//! interesting method names. A bundle is rendered against the query's
//! terms, so "how does it store state" surfaces the state methods and
//! "how does cancellation work" surfaces the cancellation ones, with no
//! special case for either.

use std::path::{Path, PathBuf};

use syn::spanned::Spanned;

use crate::symbols::SymbolKind;

/// An exact region of one file, 1-indexed and inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
}

impl SourceSpan {
    pub fn line_count(&self) -> usize {
        self.line_end.saturating_sub(self.line_start) + 1
    }

    /// `path:start-end`, the form used in diagnostics.
    pub fn locator(&self) -> String {
        format!(
            "{}:{}-{}",
            self.path.display(),
            self.line_start,
            self.line_end
        )
    }
}

/// One field of a struct, or one variant of an enum.
#[derive(Debug, Clone)]
pub struct FieldEvidence {
    /// Field name, or the variant name for an enum. Tuple-struct fields
    /// are numbered, as Rust itself names them.
    pub name: String,
    /// The declared type, exactly as written in the source.
    pub type_text: String,
    pub docs: String,
    pub span: SourceSpan,
}

/// One method inside an `impl` block.
#[derive(Debug, Clone)]
pub struct MethodEvidence {
    pub name: String,
    /// The signature as written, without the body.
    pub signature: String,
    pub docs: String,
    pub span: SourceSpan,
}

impl MethodEvidence {
    /// Part of the type's public surface.
    pub fn is_public(&self) -> bool {
        self.signature.starts_with("pub ")
    }
}

/// One `impl` block targeting the symbol.
#[derive(Debug, Clone)]
pub struct ImplEvidence {
    /// The trait implemented, for `impl Trait for Type`. `None` for an
    /// inherent `impl Type`, which is where a type's own behavior lives
    /// and therefore what an explanation usually wants first.
    pub trait_name: Option<String>,
    pub span: SourceSpan,
    pub methods: Vec<MethodEvidence>,
}

impl ImplEvidence {
    pub fn is_inherent(&self) -> bool {
        self.trait_name.is_none()
    }

    pub fn header(&self, type_name: &str) -> String {
        match &self.trait_name {
            Some(trait_name) => format!("impl {trait_name} for {type_name}"),
            None => format!("impl {type_name}"),
        }
    }
}

/// Bounds on the source a bundle may contribute to a model's context.
///
/// Retrieval must cost the same whether the type has three methods or
/// three hundred; without a budget, explaining a large type is what
/// crowds every other piece of evidence out.
#[derive(Debug, Clone)]
pub struct SpanBudget {
    pub max_spans: usize,
    pub max_lines_per_span: usize,
    pub total_lines: usize,
}

impl Default for SpanBudget {
    fn default() -> Self {
        Self {
            // The declaration plus a handful of methods.
            max_spans: 5,
            // A method longer than this is a subsystem, not an
            // explanation; its signature still appears in the summary.
            max_lines_per_span: 60,
            total_lines: 200,
        }
    }
}

/// Everything the repository states about one named symbol.
#[derive(Debug, Clone)]
pub struct SymbolEvidence {
    pub name: String,
    pub kind: SymbolKind,
    /// The complete declaration.
    ///
    /// `syn` item spans include the item's doc attributes, so this
    /// covers the documentation through the closing brace — which is
    /// exactly the region worth quoting, and why a ranged read of this
    /// span answers "what is this" on its own.
    pub definition: SourceSpan,
    pub fields: Vec<FieldEvidence>,
    pub impl_blocks: Vec<ImplEvidence>,
    /// Doc-comment spans attached to the declaration.
    pub documentation: Vec<SourceSpan>,
    /// The doc comment's text, already extracted.
    pub docs: String,
}

impl SymbolEvidence {
    pub fn method_count(&self) -> usize {
        self.impl_blocks.iter().map(|b| b.methods.len()).sum()
    }

    /// Every span this bundle covers, most important first.
    ///
    /// The declaration always leads: it is the answer to "what is this",
    /// and every other span is context around it. Inherent `impl` blocks
    /// follow, then trait impls, because a type's own methods describe it
    /// better than its conformance to someone else's interface.
    pub fn spans(&self) -> Vec<SourceSpan> {
        let mut spans = vec![self.definition.clone()];
        let mut inherent: Vec<&ImplEvidence> = self
            .impl_blocks
            .iter()
            .filter(|b| b.is_inherent())
            .collect();
        let mut traits: Vec<&ImplEvidence> = self
            .impl_blocks
            .iter()
            .filter(|b| !b.is_inherent())
            .collect();
        inherent.sort_by_key(|b| b.span.line_start);
        traits.sort_by_key(|b| b.span.line_start);
        spans.extend(inherent.into_iter().chain(traits).map(|b| b.span.clone()));
        spans
    }

    /// The spans actually worth putting in front of a model, in order.
    ///
    /// Whole `impl` blocks are the wrong granularity: `impl
    /// SessionRuntime` is 570 lines here, and quoting it would crowd out
    /// everything else exactly as the verbose document did. So the
    /// declaration is quoted whole — it is short and it carries the
    /// fields, which are the substance of "what does it store" — and the
    /// rest of the budget goes to individual methods, most relevant
    /// first, skipping any single one too large to be worth its cost.
    ///
    /// The result is bounded, so the size of the context this produces
    /// does not depend on the size of the type it describes.
    pub fn budgeted_spans(&self, terms: &[String], budget: &SpanBudget) -> Vec<SourceSpan> {
        let mut spans = vec![self.definition.clone()];
        let mut remaining = budget
            .total_lines
            .saturating_sub(self.definition.line_count());

        let ranked = self.methods_for(terms);
        // Once any method matches the question, the ones that do not are
        // noise. Quoting two constructors because they happened to come
        // first in the file is how a budget gets spent saying nothing.
        let relevant = self.relevance_cutoff(terms);

        for (index, method) in ranked.iter().enumerate() {
            if spans.len() >= budget.max_spans || remaining == 0 {
                break;
            }
            if relevant > 0 && index >= relevant {
                break;
            }

            // A method too long to quote is *truncated*, not skipped.
            // `run_turn` is 209 lines here; its first forty carry the
            // doc comment and the signature, which is what tells a
            // reader it is the method that owns the state. Dropping it
            // entirely leaves the question answered by whatever came
            // next in the file.
            let available = budget.max_lines_per_span.min(remaining);
            if available == 0 {
                break;
            }
            let lines = method.span.line_count().min(available);
            spans.push(SourceSpan {
                path: method.span.path.clone(),
                line_start: method.span.line_start,
                line_end: method.span.line_start + lines - 1,
            });
            remaining -= lines;
        }
        spans
    }

    /// How many of the ranked methods actually matched the question.
    ///
    /// Zero means nothing matched, and the caller falls back to source
    /// order — for a question with no usable terms, a type's first few
    /// public methods are a reasonable thing to show.
    fn relevance_cutoff(&self, terms: &[String]) -> usize {
        if terms.is_empty() {
            return 0;
        }
        self.impl_blocks
            .iter()
            .flat_map(|block| block.methods.iter())
            .filter(|method| {
                let haystack = orbit_project::content_terms(&format!(
                    "{} {} {}",
                    method.name, method.signature, method.docs
                ));
                terms.iter().any(|t| haystack.contains(t))
            })
            .count()
    }

    /// Methods ordered by how well they match the question's terms.
    ///
    /// Relevance is term overlap against the method's own name and its
    /// documentation — nothing about this knows what "state" or
    /// "cancellation" mean, so a question about either finds its methods
    /// by the same rule, and a question about something else finds
    /// different ones.
    ///
    /// Pass the plan's *concepts* rather than all its terms. Every method
    /// of `SessionRuntime` mentions sessions, so including the type's own
    /// words scores them all equally and the order collapses back to
    /// source order — which is how `single_project` outranked the methods
    /// that actually touch state.
    pub fn methods_for(&self, terms: &[String]) -> Vec<&MethodEvidence> {
        let mut scored: Vec<(usize, &MethodEvidence)> = self
            .impl_blocks
            .iter()
            .flat_map(|block| block.methods.iter())
            .map(|method| {
                // The signature belongs in the haystack: a method that
                // takes or returns `SessionState` is exactly what "how
                // does it store state" is asking about, and its name
                // alone ("clear", "end") would never say so.
                let haystack: Vec<String> = orbit_project::content_terms(&format!(
                    "{} {} {}",
                    method.name, method.signature, method.docs
                ));
                let hits = terms.iter().filter(|t| haystack.contains(t)).count();
                (hits, method)
            })
            .collect();

        // Stable: score, then public before private, then source order,
        // so the same question always renders the same bundle. The
        // visibility tiebreak matters when nothing matches — a type's
        // public surface explains it better than its constructors do.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.is_public().cmp(&b.1.is_public()).reverse())
                .then_with(|| a.1.span.line_start.cmp(&b.1.span.line_start))
        });
        scored.into_iter().map(|(_, method)| method).collect()
    }

    /// A compact, readable rendering for diagnostics and for the model.
    ///
    /// This is a *summary* of the bundle — the declaration with its
    /// fields, then method signatures. The full source of each span is
    /// fetched separately through `project.read_file`, which is what
    /// keeps every byte the model sees behind the permission boundary.
    pub fn render(&self, source: &str, terms: &[String], max_methods: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} {} — {}\n",
            self.kind.as_str(),
            self.name,
            self.definition.locator()
        ));
        if !self.docs.trim().is_empty() {
            for line in self.docs.lines().take(6) {
                out.push_str(&format!("  /// {}\n", line.trim()));
            }
        }

        out.push_str(&slice_span(source, &self.definition));
        out.push('\n');

        if !self.fields.is_empty() {
            out.push_str(&format!("fields ({}):\n", self.fields.len()));
            for field in &self.fields {
                out.push_str(&format!("  {}: {}\n", field.name, field.type_text));
            }
        }

        let methods = self.methods_for(terms);
        if !methods.is_empty() {
            out.push_str(&format!(
                "methods ({} of {}, most relevant first):\n",
                methods.len().min(max_methods),
                self.method_count()
            ));
            for method in methods.into_iter().take(max_methods) {
                out.push_str(&format!(
                    "  {}  [{}]\n",
                    method.signature,
                    method.span.locator()
                ));
            }
        }

        for block in &self.impl_blocks {
            out.push_str(&format!(
                "{} — {}\n",
                block.header(&self.name),
                block.span.locator()
            ));
        }
        out
    }
}

/// The text of `span` from `source`.
pub fn slice_span(source: &str, span: &SourceSpan) -> String {
    source
        .lines()
        .skip(span.line_start.saturating_sub(1))
        .take(span.line_count())
        .collect::<Vec<_>>()
        .join("\n")
}

fn span_of(path: &Path, span: proc_macro2::Span) -> SourceSpan {
    let start = span.start().line.max(1);
    let end = span.end().line.max(start);
    SourceSpan {
        path: path.to_path_buf(),
        line_start: start,
        line_end: end,
    }
}

fn doc_text(attrs: &[syn::Attribute]) -> String {
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
    lines.join("\n")
}

fn doc_spans(path: &Path, attrs: &[syn::Attribute]) -> Vec<SourceSpan> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .map(|attr| span_of(path, attr.span()))
        .collect()
}

/// The source text of a span, with column precision on one line.
///
/// A field's type is usually a fragment of a line (`tokio::sync::Mutex<
/// SessionState>`), so quoting whole lines would drag in the field name
/// and trailing comma. Multi-line types fall back to whole lines, which
/// is correct if slightly wide.
fn span_text(source_lines: &[&str], span: proc_macro2::Span) -> String {
    let start = span.start();
    let end = span.end();
    if start.line == end.line {
        let Some(line) = source_lines.get(start.line.saturating_sub(1)) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let from = start.column.min(chars.len());
        let to = end.column.min(chars.len()).max(from);
        return chars[from..to]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
    }
    source_lines
        .iter()
        .skip(start.line.saturating_sub(1))
        .take(end.line - start.line + 1)
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

fn fields_of(path: &Path, source_lines: &[&str], fields: &syn::Fields) -> Vec<FieldEvidence> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| FieldEvidence {
            name: field
                .ident
                .as_ref()
                .map(|i| i.to_string())
                // Tuple-struct fields have no name; Rust calls them by
                // position, so this does too.
                .unwrap_or_else(|| index.to_string()),
            type_text: span_text(source_lines, field.ty.span()),
            docs: doc_text(&field.attrs),
            span: span_of(path, field.span()),
        })
        .collect()
}

/// The signature of a method, without its body.
///
/// `syn::Signature` does not cover the visibility keyword, so `pub` has
/// to be prepended: a reader deciding whether a method is part of a
/// type's public surface needs it, and it is the difference between
/// "this is how you use it" and "this is an internal helper".
fn method_signature(source_lines: &[&str], vis: &syn::Visibility, sig: &syn::Signature) -> String {
    let text = span_text(source_lines, sig.span());
    // A multi-line signature arrives with its lines joined; collapse the
    // repeated whitespace that produces.
    let signature = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        // A multi-line signature is reconstructed from whole lines, so
        // the brace that opens the body comes along with it.
        .trim_end_matches('{')
        .trim()
        .to_string();

    match vis {
        syn::Visibility::Inherited => signature,
        _ => {
            let keyword = span_text(source_lines, vis.span());
            // Whole-line reconstruction already carries the visibility;
            // prepending it again produced `pub pub async fn`.
            if keyword.is_empty() || signature.starts_with(&keyword) {
                signature
            } else {
                format!("{keyword} {signature}")
            }
        }
    }
}

fn methods_of(path: &Path, source_lines: &[&str], items: &[syn::ImplItem]) -> Vec<MethodEvidence> {
    items
        .iter()
        .filter_map(|item| match item {
            syn::ImplItem::Fn(method) => Some(MethodEvidence {
                name: method.sig.ident.to_string(),
                signature: method_signature(source_lines, &method.vis, &method.sig),
                docs: doc_text(&method.attrs),
                span: span_of(path, method.span()),
            }),
            _ => None,
        })
        .collect()
}

/// The type an `impl` block targets, if it is a plain named type.
fn impl_target(item: &syn::ItemImpl) -> Option<String> {
    match &*item.self_ty {
        syn::Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn impl_trait_name(item: &syn::ItemImpl) -> Option<String> {
    item.trait_
        .as_ref()
        .and_then(|(_, path, _)| path.segments.last())
        .map(|s| s.ident.to_string())
}

/// What a declaration contributes to a bundle, before its `impl` blocks
/// are gathered.
struct Declaration {
    kind: SymbolKind,
    span: SourceSpan,
    fields: Vec<FieldEvidence>,
    documentation: Vec<SourceSpan>,
    docs: String,
}

/// Collect the declaration of `name`, if this item is it.
fn declaration_of(
    path: &Path,
    source_lines: &[&str],
    item: &syn::Item,
    name: &str,
) -> Option<Declaration> {
    match item {
        syn::Item::Struct(i) if i.ident == name => Some(Declaration {
            kind: SymbolKind::Struct,
            span: span_of(path, i.span()),
            fields: fields_of(path, source_lines, &i.fields),
            documentation: doc_spans(path, &i.attrs),
            docs: doc_text(&i.attrs),
        }),
        syn::Item::Enum(i) if i.ident == name => Some(Declaration {
            kind: SymbolKind::Enum,
            span: span_of(path, i.span()),
            // An enum's variants are its shape, so they take the field
            // slot: "what states can this be in" is the same question
            // that "what does it store" asks of a struct.
            fields: i
                .variants
                .iter()
                .map(|variant| FieldEvidence {
                    name: variant.ident.to_string(),
                    type_text: span_text(source_lines, variant.fields.span()),
                    docs: doc_text(&variant.attrs),
                    span: span_of(path, variant.span()),
                })
                .collect(),
            documentation: doc_spans(path, &i.attrs),
            docs: doc_text(&i.attrs),
        }),
        syn::Item::Trait(i) if i.ident == name => Some(Declaration {
            kind: SymbolKind::Trait,
            span: span_of(path, i.span()),
            fields: Vec::new(),
            documentation: doc_spans(path, &i.attrs),
            docs: doc_text(&i.attrs),
        }),
        syn::Item::Type(i) if i.ident == name => Some(Declaration {
            kind: SymbolKind::Type,
            span: span_of(path, i.span()),
            fields: Vec::new(),
            documentation: doc_spans(path, &i.attrs),
            docs: doc_text(&i.attrs),
        }),
        _ => None,
    }
}

/// Walk `items`, gathering the declaration of `name` and every `impl`
/// that targets it. Recurses into inline modules, because a type and its
/// impls are frequently nested.
fn collect(
    path: &Path,
    source_lines: &[&str],
    items: &[syn::Item],
    name: &str,
    declaration: &mut Option<Declaration>,
    impls: &mut Vec<ImplEvidence>,
) {
    for item in items {
        if declaration.is_none()
            && let Some(found) = declaration_of(path, source_lines, item, name)
        {
            *declaration = Some(found);
        }

        match item {
            syn::Item::Impl(i) if impl_target(i).as_deref() == Some(name) => {
                impls.push(ImplEvidence {
                    trait_name: impl_trait_name(i),
                    span: span_of(path, i.span()),
                    methods: methods_of(path, source_lines, &i.items),
                });
            }
            syn::Item::Mod(i) => {
                if let Some((_, inner)) = &i.content {
                    collect(path, source_lines, inner, name, declaration, impls);
                }
            }
            _ => {}
        }
    }
}

/// Build the evidence bundle for `name` from one Rust file.
///
/// Returns `None` when the file does not declare it — an `impl` without
/// the declaration is not a bundle, because the fields are the substance
/// of the answer and they live with the `struct`.
pub fn extract(path: &Path, source: &str, name: &str) -> Option<SymbolEvidence> {
    let parsed = syn::parse_file(source).ok()?;
    let source_lines: Vec<&str> = source.lines().collect();

    let mut declaration = None;
    let mut impl_blocks = Vec::new();
    collect(
        path,
        &source_lines,
        &parsed.items,
        name,
        &mut declaration,
        &mut impl_blocks,
    );

    let found = declaration?;
    Some(SymbolEvidence {
        name: name.to_string(),
        kind: found.kind,
        definition: found.span,
        fields: found.fields,
        impl_blocks,
        documentation: found.documentation,
        docs: found.docs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"//! Sessions.
use std::sync::Arc;

/// A stateful conversation.
/// Cheap to share.
pub struct SessionRuntime {
    id: SessionId,
    /// Held for the duration of a turn.
    state: tokio::sync::Mutex<SessionState>,
    current_cancel: std::sync::Mutex<Option<CancellationToken>>,
    pub streaming: bool,
}

impl SessionRuntime {
    /// Start a session.
    pub fn new(id: SessionId) -> Self {
        Self { id, state: Default::default(), current_cancel: Default::default(), streaming: false }
    }

    /// Store a message in the session state.
    pub async fn record_state(&self, message: String) {
        self.state.lock().await.history.push(message);
    }

    pub fn cancel_current_turn(&self) -> bool {
        true
    }
}

impl Clone for SessionRuntime {
    fn clone(&self) -> Self {
        unimplemented!()
    }
}

pub struct Unrelated {
    other: u8,
}

impl Unrelated {
    fn noop(&self) {}
}
"#;

    fn bundle() -> SymbolEvidence {
        extract(
            Path::new("crates/session/src/session.rs"),
            SOURCE,
            "SessionRuntime",
        )
        .expect("SessionRuntime is declared in this source")
    }

    #[test]
    fn the_definition_span_covers_the_whole_declaration() {
        let evidence = bundle();
        assert_eq!(evidence.kind, SymbolKind::Struct);
        // The span starts at the doc comment, not at `pub struct`, and
        // runs to the closing brace: quoting it yields the declaration
        // *and* the prose explaining it.
        assert_eq!(evidence.definition.line_start, 4);
        assert_eq!(evidence.definition.line_end, 12);
        // Not a single line — that was the whole defect.
        assert_eq!(evidence.definition.line_count(), 9);
    }

    #[test]
    fn every_field_is_captured_with_its_type() {
        let evidence = bundle();
        let names: Vec<&str> = evidence.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["id", "state", "current_cancel", "streaming"]);

        let state = &evidence.fields[1];
        assert_eq!(state.type_text, "tokio::sync::Mutex<SessionState>");
        assert_eq!(state.docs, "Held for the duration of a turn.");

        let cancel = &evidence.fields[2];
        assert_eq!(
            cancel.type_text,
            "std::sync::Mutex<Option<CancellationToken>>"
        );
    }

    #[test]
    fn documentation_on_the_type_is_captured() {
        let evidence = bundle();
        assert_eq!(evidence.docs, "A stateful conversation.\nCheap to share.");
        assert_eq!(evidence.documentation.len(), 2);
    }

    #[test]
    fn inherent_and_trait_impls_are_both_found() {
        let evidence = bundle();
        assert_eq!(evidence.impl_blocks.len(), 2);

        let inherent = evidence
            .impl_blocks
            .iter()
            .find(|b| b.is_inherent())
            .expect("inherent impl");
        assert_eq!(inherent.methods.len(), 3);
        assert_eq!(inherent.header("SessionRuntime"), "impl SessionRuntime");

        let clone = evidence
            .impl_blocks
            .iter()
            .find(|b| !b.is_inherent())
            .expect("trait impl");
        assert_eq!(clone.trait_name.as_deref(), Some("Clone"));
        assert_eq!(
            clone.header("SessionRuntime"),
            "impl Clone for SessionRuntime"
        );
    }

    #[test]
    fn methods_carry_signatures_docs_and_spans() {
        let evidence = bundle();
        let record = evidence
            .impl_blocks
            .iter()
            .flat_map(|b| &b.methods)
            .find(|m| m.name == "record_state")
            .expect("record_state");
        assert_eq!(
            record.signature,
            "pub async fn record_state(&self, message: String)"
        );
        assert_eq!(record.docs, "Store a message in the session state.");
        // The span opens at the doc comment, so a ranged read of a
        // method quotes the prose that explains it.
        assert_eq!(record.span.line_start, 20);
        assert_eq!(
            record.span.path,
            PathBuf::from("crates/session/src/session.rs")
        );
    }

    /// Which methods matter is decided by the question's own terms, with
    /// no list of interesting names anywhere in this module.
    #[test]
    fn method_relevance_follows_the_question() {
        let evidence = bundle();

        let for_state = evidence.methods_for(&orbit_project::content_terms("stores session state"));
        assert_eq!(for_state[0].name, "record_state");

        let for_cancel = evidence.methods_for(&orbit_project::content_terms("cancellation"));
        assert_eq!(for_cancel[0].name, "cancel_current_turn");
    }

    #[test]
    fn a_symbol_declared_elsewhere_yields_no_bundle() {
        assert!(extract(Path::new("a.rs"), SOURCE, "NotHere").is_none());
        // An impl alone is not a bundle: the fields are the substance.
        assert!(extract(Path::new("a.rs"), "impl Foo { fn a(&self) {} }", "Foo").is_none());
    }

    #[test]
    fn impls_for_other_types_are_not_collected() {
        let evidence = bundle();
        assert!(
            evidence.impl_blocks.iter().all(|b| b.span.line_start < 36),
            "picked up Unrelated's impl"
        );
    }

    #[test]
    fn spans_lead_with_the_declaration_then_inherent_impls() {
        let evidence = bundle();
        let spans = evidence.spans();
        assert_eq!(spans[0], evidence.definition);
        // Inherent impl before the Clone impl.
        assert_eq!(spans[1].line_start, 14);
        assert_eq!(spans[2].line_start, 30);
    }

    #[test]
    fn an_enum_reports_its_variants_as_fields() {
        let source =
            "/// Modes.\npub enum Mode {\n    /// One.\n    Single,\n    Workspace(String),\n}\n";
        let evidence = extract(Path::new("a.rs"), source, "Mode").unwrap();
        assert_eq!(evidence.kind, SymbolKind::Enum);
        let names: Vec<&str> = evidence.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Single", "Workspace"]);
    }

    #[test]
    fn slicing_a_span_returns_exactly_those_lines() {
        let evidence = bundle();
        let text = slice_span(SOURCE, &evidence.definition);
        assert!(text.starts_with("/// A stateful conversation."));
        assert!(text.contains("pub struct SessionRuntime {"));
        assert!(text.trim_end().ends_with('}'));
        assert!(text.contains("state: tokio::sync::Mutex<SessionState>"));
    }

    #[test]
    fn rendering_includes_fields_and_relevant_methods() {
        let evidence = bundle();
        let rendered = evidence.render(SOURCE, &orbit_project::content_terms("session state"), 5);
        assert!(rendered.contains("struct SessionRuntime"));
        assert!(rendered.contains("state: tokio::sync::Mutex<SessionState>"));
        assert!(rendered.contains("record_state"));
        assert!(rendered.contains("impl Clone for SessionRuntime"));
    }

    #[test]
    fn extraction_is_deterministic() {
        let a = bundle();
        let b = bundle();
        assert_eq!(a.definition, b.definition);
        assert_eq!(
            a.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
            b.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn budgeted_spans_lead_with_the_declaration() {
        let evidence = bundle();
        let spans = evidence.budgeted_spans(&["stat".to_string()], &SpanBudget::default());
        assert_eq!(spans[0], evidence.definition);
    }

    /// A method too large to quote whole is truncated to its opening
    /// lines — the doc comment and signature — rather than dropped. Its
    /// identity is the useful part; its 200-line body is not.
    #[test]
    fn an_oversized_method_is_truncated_not_skipped() {
        let evidence = bundle();
        let budget = SpanBudget {
            max_spans: 5,
            max_lines_per_span: 2,
            total_lines: 200,
        };
        let spans = evidence.budgeted_spans(&orbit_project::content_terms("state"), &budget);
        assert!(spans.len() > 1, "the method was dropped: {spans:?}");
        for span in &spans[1..] {
            assert!(span.line_count() <= 2, "{span:?}");
        }
    }

    /// Once any method matches the question, methods that do not must
    /// not consume the budget just for coming first in the file.
    #[test]
    fn irrelevant_methods_do_not_fill_the_budget() {
        let source = "pub struct S { v: u8 }\nimpl S {\n    pub fn new() -> Self { Self { v: 0 } }\n    pub fn other(&self) {}\n    /// Cancel the running turn.\n    pub fn cancel(&self) {}\n}\n";
        let evidence = extract(Path::new("a.rs"), source, "S").unwrap();
        let spans = evidence.budgeted_spans(
            &orbit_project::content_terms("cancellation"),
            &SpanBudget::default(),
        );
        // The declaration plus exactly the one matching method.
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[1].line_start, 5);
    }

    /// The cost of explaining a type must not scale with its size.
    #[test]
    fn the_span_budget_is_respected() {
        let evidence = bundle();
        let budget = SpanBudget {
            max_spans: 3,
            max_lines_per_span: 60,
            total_lines: 40,
        };
        let spans = evidence.budgeted_spans(&["stat".to_string()], &budget);
        assert!(spans.len() <= 3);
        let total: usize = spans.iter().map(|s| s.line_count()).sum();
        assert!(total <= 40 + evidence.definition.line_count(), "{total}");
    }

    #[test]
    fn a_method_signature_carries_visibility_exactly_once() {
        let evidence = bundle();
        for method in evidence.impl_blocks.iter().flat_map(|b| &b.methods) {
            assert!(
                !method.signature.contains("pub pub"),
                "{}",
                method.signature
            );
            assert!(!method.signature.ends_with('{'), "{}", method.signature);
        }
    }

    /// The signature is part of what a method is *about*: a method taking
    /// `&mut SessionState` answers "how does it store state" even though
    /// neither its name nor its docs say so.
    #[test]
    fn a_methods_signature_counts_toward_its_relevance() {
        let source = "pub struct S { v: u8 }\nimpl S {\n    pub fn a(&self) {}\n    pub fn b(&self, cancel: CancellationToken) {}\n}\n";
        let evidence = extract(Path::new("a.rs"), source, "S").unwrap();
        let ranked = evidence.methods_for(&orbit_project::content_terms("cancellation"));
        assert_eq!(ranked[0].name, "b");
    }
}
