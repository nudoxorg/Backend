//! The language-agnostic rendering layer.
//!
//! Three layers sit between an IR entry and a string:
//!
//! 1. [`crate::render::doc`] — the document algebra (nil/text/line/group/…).
//! 2. this module — a [`Backend`] trait plus shared layout combinators
//!    ([`block`], [`arglist`], [`generic_list`]); the structural skeleton every
//!    language shares.
//! 3. [`crate::render::emit`] — one thin module per target language, each an
//!    `impl Backend` that supplies the language's surface syntax.
//!
//! A caller picks a [`Language`], wraps its options in a [`RenderCtx`], and
//! calls [`render_entry`]. Everything below the entry point is pure `Doc`
//! construction; the single decision about where lines break is deferred to the
//! printer at the very end.

use ir::function::Function;
use ir::generics::Generics;
use ir::kind::{Entry, Visibility};
use ir::protocols::TraitDef;
use ir::record::{Record, SumVariant};
use ir::ty::Type;

use super::doc::Doc;
use super::emit;

/// Columns of indentation per nesting level, shared by every backend.
pub const INDENT: isize = 4;

/// A target surface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Go,
    Java,
    TypeScript,
    Python,
    Nix,
}

impl Language {
    /// Every supported target, in a stable order (handy for demos and tests).
    pub const ALL: [Language; 6] = [
        Language::Rust,
        Language::Go,
        Language::Java,
        Language::TypeScript,
        Language::Python,
        Language::Nix,
    ];

    /// A human label for the language.
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Go => "Go",
            Language::Java => "Java",
            Language::TypeScript => "TypeScript",
            Language::Python => "Python",
            Language::Nix => "Nix",
        }
    }
}

/// Semantic role of a span of output. Carried through the [`Doc`] tree so that
/// a richer [`Sink`](super::doc::Sink) (syntax highlighting, HTML) can colour
/// the output; the default string rendering ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Annotation {
    Keyword,
    Ident,
    Type,
    Comment,
    Punct,
}

/// The rendered document type all backends produce.
pub type Rendered = Doc<Annotation>;

/// Shared knobs for all target languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Print fully-qualified paths (`std::vec::Vec`) instead of the last
    /// segment (`Vec`).
    pub qualified_paths: bool,
    /// Emit documentation carried inside payloads (fields, variants, methods).
    pub show_docs: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            qualified_paths: false,
            show_docs: false,
        }
    }
}

/// Everything a render pass needs: the target language, the options, and the
/// column budget the printer aims to keep lines within.
#[derive(Debug, Clone, Copy)]
pub struct RenderCtx {
    pub language: Language,
    pub options: RenderOptions,
    pub width: usize,
}

impl RenderCtx {
    /// A context for `language` with default options and an 80-column budget.
    pub fn new(language: Language) -> Self {
        Self {
            language,
            options: RenderOptions::default(),
            width: 80,
        }
    }

    /// Builder-style override of the column budget.
    pub fn with_width(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    /// Builder-style toggle for payload-internal documentation.
    pub fn with_docs(mut self, show: bool) -> Self {
        self.options.show_docs = show;
        self
    }

    fn backend(&self) -> &'static dyn Backend {
        match self.language {
            Language::Rust => &emit::rust::Rust,
            Language::Go => &emit::go::Go,
            Language::Java => &emit::java::Java,
            Language::TypeScript => &emit::typescript::TypeScript,
            Language::Python => &emit::python::Python,
            Language::Nix => &emit::nix::Nix,
        }
    }
}

/// The per-language surface. Implementors live in [`crate::render::emit`] and
/// are zero-sized; all state travels in the [`RenderCtx`].
pub trait Backend {
    /// Render a documentation comment block for `text` in this language's
    /// surface (`///`, `//`, `/** */`, `#`, …).
    fn doc_comment(&self, text: &str) -> Rendered;

    /// Render a type in type position.
    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered;

    /// Render a record as a struct / class / interface / dataclass.
    fn record(&self, name: &str, vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered;

    /// Render a sum type as an enum / tagged union / sealed hierarchy.
    fn sum(
        &self,
        name: &str,
        generics: Option<&Generics>,
        variants: &[SumVariant],
        vis: &Visibility,
        cx: &RenderCtx,
    ) -> Rendered;

    /// Render a free function or method signature.
    fn function(&self, name: &str, f: &Function, vis: &Visibility, cx: &RenderCtx) -> Rendered;

    /// Render a trait / interface / protocol declaration.
    fn interface(&self, name: &str, def: &TraitDef, vis: &Visibility, cx: &RenderCtx) -> Rendered;
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Render a single top-level [`Entry`] to a string in the context's language.
///
/// The entry's name, visibility, and (when [`RenderOptions::show_docs`] is set)
/// its documentation live on the `Symbol` wrapper and are woven in here; the
/// payload renderers see only the payload.
pub fn render_entry(entry: &Entry, cx: &RenderCtx) -> String {
    render_entry_doc(entry, cx).render(cx.width)
}

/// As [`render_entry`], but returns the underlying document (composable into a
/// larger layout, or rendered through a custom [`Sink`](super::doc::Sink)).
pub fn render_entry_doc(entry: &Entry, cx: &RenderCtx) -> Rendered {
    let be = cx.backend();
    let body = match entry {
        Entry::RecordType(s) => be.record(&s.name, &s.visibility, &s.inner, cx),
        Entry::SumType(s) => be.sum(&s.name, None, &s.inner, &s.visibility, cx),
        Entry::TraitDef(s) => be.interface(&s.name, &s.inner, &s.visibility, cx),
        Entry::Function(s) => be.function(&s.name, &s.inner, &s.visibility, cx),
        Entry::TypeAlias(s) => alias_doc(be, &s.name, &s.inner, cx),
        Entry::UnionType(s) => union_alias_doc(be, &s.name, &s.inner, cx),
        other => Doc::text(format!("// <unrendered {}>", other.kind_tag())),
    };
    match (cx.options.show_docs, entry.documentation()) {
        (true, Some(doc)) if !doc.is_empty() => be.doc_comment(doc) + Doc::hardline() + body,
        _ => body,
    }
}

fn alias_doc(be: &dyn Backend, name: &str, ty: &Type, cx: &RenderCtx) -> Rendered {
    use Annotation::*;
    match cx.language {
        Language::Rust => {
            kw("type") + sp() + ident(name) + sp() + punct("=") + sp() + be.ty(ty, cx) + punct(";")
        }
        Language::Go => kw("type") + sp() + ident(name) + sp() + be.ty(ty, cx),
        Language::TypeScript => {
            kw("type") + sp() + ident(name) + sp() + punct("=") + sp() + be.ty(ty, cx) + punct(";")
        }
        Language::Python => {
            ident(name)
                + punct(":")
                + sp()
                + kw("TypeAlias")
                + sp()
                + punct("=")
                + sp()
                + be.ty(ty, cx)
        }
        Language::Java => {
            // Java has no type aliases; the least-lossy surface is a comment.
            txt("// type ").annotate(Comment)
                + ident(name)
                + txt(" = ").annotate(Comment)
                + be.ty(ty, cx)
        }
        Language::Nix => {
            // Nix has no type aliases; document as a `# type` comment.
            txt("# type ").annotate(Comment)
                + ident(name)
                + txt(" = ").annotate(Comment)
                + be.ty(ty, cx)
        }
    }
}

fn union_alias_doc(be: &dyn Backend, name: &str, members: &[Type], cx: &RenderCtx) -> Rendered {
    let body = Doc::join(punct(" | "), members.iter().map(|m| be.ty(m, cx)));
    alias_doc_from_rendered(name, body, cx)
}

fn alias_doc_from_rendered(name: &str, rendered: Rendered, cx: &RenderCtx) -> Rendered {
    match cx.language {
        Language::Rust | Language::TypeScript => {
            kw("type") + sp() + ident(name) + sp() + punct("=") + sp() + rendered + punct(";")
        }
        Language::Python => ident(name) + sp() + punct("=") + sp() + rendered,
        Language::Go => kw("type") + sp() + ident(name) + sp() + rendered,
        Language::Java => {
            txt("// type ").annotate(Annotation::Comment)
                + ident(name)
                + txt(" = ").annotate(Annotation::Comment)
                + rendered
        }
        Language::Nix => {
            txt("# type ").annotate(Annotation::Comment)
                + ident(name)
                + txt(" = ").annotate(Annotation::Comment)
                + rendered
        }
    }
}

// ---------------------------------------------------------------------------
// Leaf combinators — the shared vocabulary every backend builds from.
// ---------------------------------------------------------------------------

/// A keyword (`struct`, `fn`, `class`, …).
pub fn kw(s: &str) -> Rendered {
    Doc::text(s.to_string()).annotate(Annotation::Keyword)
}

/// An identifier (a declared or referenced name).
pub fn ident(s: &str) -> Rendered {
    Doc::text(s.to_string()).annotate(Annotation::Ident)
}

/// A type name.
pub fn tyname(s: &str) -> Rendered {
    Doc::text(s.to_string()).annotate(Annotation::Type)
}

/// Punctuation (`:`, `,`, `->`, …).
pub fn punct(s: &str) -> Rendered {
    Doc::text(s.to_string()).annotate(Annotation::Punct)
}

/// Un-annotated literal text.
pub fn txt(s: &str) -> Rendered {
    Doc::text(s.to_string())
}

/// A single, non-breaking space.
pub fn sp() -> Rendered {
    Doc::text(" ")
}

// ---------------------------------------------------------------------------
// Layout combinators — the reusable prettifier shapes.
// ---------------------------------------------------------------------------

/// A brace-delimited body that always breaks one item per line, indented.
///
/// ```text
/// open
///     item0
///     item1
/// close
/// ```
///
/// An empty body collapses to `open close` on one line (e.g. `{}`).
pub fn block(open: &str, items: Vec<Rendered>, close: &str) -> Rendered {
    if items.is_empty() {
        return punct(open) + punct(close);
    }
    let inner = Doc::join(Doc::hardline(), items);
    punct(open) + (Doc::hardline() + inner).nest(INDENT) + Doc::hardline() + punct(close)
}

/// A parenthesised, comma-separated list that inlines when it fits and explodes
/// one-per-line (with a trailing comma) when it does not.
pub fn arglist(open: &str, items: Vec<Rendered>, close: &str) -> Rendered {
    if items.is_empty() {
        return punct(open) + punct(close);
    }
    let sep = punct(",") + Doc::line();
    let inner = Doc::join(sep, items);
    // Trailing comma appears only in the broken layout.
    let trailer = trailing_comma_when_broken();
    (punct(open)
        + (Doc::softline() + inner + trailer).nest(INDENT)
        + Doc::softline()
        + punct(close))
    .group()
}

/// A trailing separator that is present only when the enclosing group breaks.
/// In flat mode: nothing. In break mode: a comma.
fn trailing_comma_when_broken() -> Rendered {
    Doc::flat_alt(Doc::nil(), punct(","))
}

/// An angle/bracket-delimited generic list (`<A, B>` or `[A, B]`), breakable.
pub fn generic_list(open: &str, items: Vec<Rendered>, close: &str) -> Rendered {
    if items.is_empty() {
        return Doc::nil();
    }
    let sep = punct(",") + Doc::line();
    let inner = Doc::join(sep, items);
    (punct(open) + (Doc::softline() + inner).nest(INDENT) + Doc::softline() + punct(close)).group()
}

/// Shorten a `::`/`.`-qualified path to its final segment unless the context
/// asked for fully-qualified paths. Non-path strings pass through untouched.
pub fn short_name<'a>(path: &'a str, cx: &RenderCtx) -> &'a str {
    if cx.options.qualified_paths {
        return path;
    }
    if !path
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '<' | '>'))
    {
        return path;
    }
    let last_sep = path
        .rfind("::")
        .map(|i| i + 2)
        .or_else(|| path.rfind('.').map(|i| i + 1));
    match last_sep {
        Some(i) if i < path.len() => &path[i..],
        _ => path,
    }
}
