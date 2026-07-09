//! The static layer: parse every `.nix` file once with `rnix` and build a
//! per-file table of bindings and lambdas keyed by byte span.
//!
//! This is deliberately evaluation-free — it works on any `.nix` tree, flake
//! or not, and already captures more than `nixdoc` (curried chains, pattern
//! defaults, `args@`/ellipsis, doc comments, and the position data the dynamic
//! layer later fuses against).
//!
//! The one interesting subtlety is **full attrpath resolution**: `rnix` hands
//! each `AttrpathValue` only its *local* attrpath (`mapAttrs` in
//! `attrsets = { mapAttrs = …; }`), so we climb the ancestor chain prepending
//! each enclosing binding's attrpath to recover `attrsets.mapAttrs`.

use std::path::{Path, PathBuf};

use rnix::ast::{self, HasEntry};
use rowan::ast::AstNode;

use super::docs;
use super::error::{NixError, Result};

/// A byte range within a single file's source text. Offsets index into
/// [`StaticFile::source`] and are what the dynamic layer resolves snix
/// `codemap` spans back to during fusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end:   usize,
}

impl Span {
    /// Slice this span out of a source string (used to pretty-print default
    /// expressions and signature fragments verbatim).
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        source.get(self.start..self.end).unwrap_or("")
    }
}

/// One component of an attribute path. Dynamic components (`${expr}`) are
/// opaque — we keep their presence but not a resolvable name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrName {
    /// A statically-known attribute name.
    Static(String),
    /// A `${…}` interpolated / computed key we cannot statically resolve.
    Dynamic,
}

impl AttrName {
    pub fn as_static(&self) -> Option<&str> {
        match self {
            AttrName::Static(s) => Some(s),
            AttrName::Dynamic => None,
        }
    }
}

/// How a lambda parameter was introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// A simple `x: …` identifier parameter.
    Ident,
    /// A `{ a, b ? d, ... }` record-pattern field.
    PatternField,
}

/// A single lambda parameter recovered statically, with its default expression
/// (which the evaluator does *not* expose — this is the static layer's edge).
#[derive(Debug, Clone)]
pub struct StaticParam {
    pub name:    String,
    /// Verbatim source of the default expression (`? <expr>`), if any.
    pub default: Option<String>,
    pub kind:    ParamKind,
    /// A `# argname` line comment (legacy convention) attached to this formal.
    pub doc:     Option<String>,
    /// True when this parameter came from a *nested* lambda in a curried chain
    /// (`a: b: …`) rather than a single record pattern.
    pub curried: bool,
}

/// A lambda recovered from the CST, with its (possibly curried) parameter list
/// flattened into declaration order.
#[derive(Debug, Clone)]
pub struct LambdaInfo {
    /// Byte span of the lambda's first token — the fusion key against snix's
    /// `Chunk::first_span()`.
    pub span:     Span,
    pub params:   Vec<StaticParam>,
    /// `{ ... }` ellipsis present on the outermost record pattern.
    pub ellipsis: bool,
    /// The `args@` / `@args` bind name on a record pattern, if any.
    pub args_bind: Option<String>,
    /// Raw doc comment attached to the lambda directly (rare; usually the
    /// binding carries it — RFC 145 precedence handles that in `docs`).
    pub doc:      Option<String>,
}

/// A `name = value;` binding recovered from the CST.
#[derive(Debug, Clone)]
pub struct Binding {
    /// Fully-qualified attribute path (ancestors prepended).
    pub attrpath:  Vec<AttrName>,
    /// Byte span of the value expression.
    pub value_span: Span,
    /// Raw (dedented) doc comment attached per RFC 145 / legacy rules.
    pub doc:       Option<String>,
    /// Index into [`StaticFile::lambdas`] when the value is a lambda.
    pub lambda:    Option<usize>,
}

impl Binding {
    /// The attrpath as plain strings, dynamic components rendered as `${}`.
    pub fn path_strings(&self) -> Vec<String> {
        self.attrpath
            .iter()
            .map(|a| match a {
                AttrName::Static(s) => s.clone(),
                AttrName::Dynamic => "${}".to_string(),
            })
            .collect()
    }

    /// Whether every component of the path is statically known.
    pub fn is_fully_static(&self) -> bool {
        self.attrpath.iter().all(|a| matches!(a, AttrName::Static(_)))
    }
}

/// A single parsed `.nix` file.
#[derive(Debug, Clone)]
pub struct StaticFile {
    /// Path relative to the flake root (stable across machines).
    pub path:     PathBuf,
    /// Owned source text; spans index into this.
    pub source:   String,
    pub bindings: Vec<Binding>,
    pub lambdas:  Vec<LambdaInfo>,
}

impl StaticFile {
    /// Find the lambda whose first token starts at `offset` (fusion lookup).
    pub fn lambda_at(&self, offset: usize) -> Option<&LambdaInfo> {
        self.lambdas.iter().find(|l| l.span.start == offset)
    }

    /// Find the binding whose value span starts at `offset`.
    pub fn binding_at(&self, offset: usize) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.value_span.start == offset)
    }
}

/// The whole static table for a flake tree.
#[derive(Debug, Clone, Default)]
pub struct StaticTable {
    pub files: Vec<StaticFile>,
}

impl StaticTable {
    /// Resolve a `(file path, byte offset)` back to the lambda declared there.
    /// This is the core fusion primitive the dynamic walker uses.
    pub fn lambda_at(&self, file: &Path, offset: usize) -> Option<(&StaticFile, &LambdaInfo)> {
        let f = self.files.iter().find(|f| f.path == file)?;
        f.lambda_at(offset).map(|l| (f, l))
    }

    pub fn file(&self, path: &Path) -> Option<&StaticFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// Parse every `.nix` file under `root` into a [`StaticTable`].
///
/// Recoverable `rnix` errors (the parser is error-tolerant) are ignored so a
/// single malformed file never sinks the whole package; a file that produces
/// no usable tree at all is skipped with a warning.
pub fn parse_tree(root: &Path) -> Result<StaticTable> {
    let mut files = Vec::new();
    for path in collect_nix_files(root) {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(source) => {
                tracing::warn!(path = %path.display(), %source, "skipping unreadable .nix file");
                continue;
            }
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_file(rel, source) {
            Ok(file) => files.push(file),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping unparseable .nix file");
            }
        }
    }
    Ok(StaticTable { files })
}

/// Parse a single file's source into a [`StaticFile`].
pub fn parse_file(rel_path: PathBuf, source: String) -> Result<StaticFile> {
    let parse = rnix::Root::parse(&source);
    // Tolerate recoverable errors; only bail if there is no root expression.
    let root = parse.tree();
    let root_node = root.syntax().clone();

    let mut file = StaticFile {
        path:     rel_path.clone(),
        source:   source.clone(),
        bindings: Vec::new(),
        lambdas:  Vec::new(),
    };

    // First pass: index every lambda by span (curried chains flattened).
    // We only record *outermost* lambdas here; nested curried lambdas are
    // folded into their head's parameter list, and their spans are still
    // discoverable because we index the head span (the fusion point snix
    // reports for the whole closure).
    let mut lambda_index: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for node in root_node.descendants() {
        if let Some(lambda) = ast::Lambda::cast(node.clone()) {
            let span = node_span(&node);
            // Skip lambdas that are the *body* of another lambda (curry tail):
            // they are captured by their head.
            if is_curry_tail(&node) {
                continue;
            }
            let info = lower_lambda_chain(&lambda, &source);
            lambda_index.insert(span.start, file.lambdas.len());
            file.lambdas.push(info);
        }
    }

    // Second pass: every binding, with full attrpath + doc + lambda link.
    for node in root_node.descendants() {
        if let Some(av) = ast::AttrpathValue::cast(node.clone()) {
            let attrpath = full_attrpath(&node, &av);
            let Some(value) = av.value() else { continue };
            let value_node = value.syntax().clone();
            let value_span = node_span(&value_node);

            let doc = docs::raw_doc_for(&node);
            let lambda = lambda_index.get(&value_span.start).copied();

            file.bindings.push(Binding { attrpath, value_span, doc, lambda });
        }
    }

    let _ = &root; // keep parse tree alive for the duration of the walk
    Ok(file)
}

// ───────────────────────────────────────────────────────────────────────────
// Attrpath resolution
// ───────────────────────────────────────────────────────────────────────────

/// The fully-qualified attrpath of an `AttrpathValue` node: its own attrpath
/// with every enclosing binding's attrpath prepended.
fn full_attrpath(node: &rnix::SyntaxNode, av: &ast::AttrpathValue) -> Vec<AttrName> {
    let mut prefix: Vec<AttrName> = Vec::new();
    // Climb ancestors; each enclosing AttrpathValue contributes a prefix.
    for ancestor in node.ancestors().skip(1) {
        if let Some(parent_av) = ast::AttrpathValue::cast(ancestor) {
            let mut segs = attrpath_names(&parent_av);
            segs.extend(std::mem::take(&mut prefix));
            prefix = segs;
        }
    }
    let mut out = prefix;
    out.extend(attrpath_names(av));
    out
}

/// The local attrpath components of one `AttrpathValue`.
fn attrpath_names(av: &ast::AttrpathValue) -> Vec<AttrName> {
    let Some(attrpath) = av.attrpath() else { return Vec::new() };
    attrpath
        .attrs()
        .map(|attr| match attr {
            ast::Attr::Ident(ident) => AttrName::Static(ident_text(&ident)),
            ast::Attr::Str(s) => match str_literal_text(&s) {
                Some(text) => AttrName::Static(text),
                None => AttrName::Dynamic,
            },
            ast::Attr::Dynamic(_) => AttrName::Dynamic,
        })
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Lambda lowering (curried-chain flattening)
// ───────────────────────────────────────────────────────────────────────────

/// Flatten a lambda and any curried tail (`a: b: …`) into a single ordered
/// parameter list.
fn lower_lambda_chain(lambda: &ast::Lambda, source: &str) -> LambdaInfo {
    let span = node_span(lambda.syntax());
    let mut params = Vec::new();
    let mut ellipsis = false;
    let mut args_bind = None;

    let mut current = Some(lambda.clone());
    let mut first = true;
    while let Some(lam) = current {
        match lam.param() {
            Some(ast::Param::IdentParam(id)) => {
                if let Some(ident) = id.ident() {
                    params.push(StaticParam {
                        name:    ident_text(&ident),
                        default: None,
                        kind:    ParamKind::Ident,
                        doc:     None,
                        curried: !first,
                    });
                }
            }
            Some(ast::Param::Pattern(pat)) => {
                if first {
                    ellipsis = pat.ellipsis_token().is_some();
                    args_bind = pat
                        .pat_bind()
                        .and_then(|b| b.ident())
                        .map(|i| ident_text(&i));
                }
                for entry in pat.pat_entries() {
                    let Some(ident) = entry.ident() else { continue };
                    let default = entry
                        .default()
                        .map(|d| d.syntax().text().to_string().trim().to_string());
                    let doc = docs::formal_doc_for(entry.syntax());
                    params.push(StaticParam {
                        name: ident_text(&ident),
                        default,
                        kind: ParamKind::PatternField,
                        doc,
                        curried: !first,
                    });
                }
            }
            None => {}
        }

        // Descend into a curried body lambda, if any.
        current = match lam.body() {
            Some(ast::Expr::Lambda(inner)) => Some(inner),
            _ => None,
        };
        first = false;
    }

    let doc = docs::raw_doc_for(lambda.syntax());
    LambdaInfo { span, params, ellipsis, args_bind, doc }
}

/// Whether this lambda node is the immediate body of an enclosing lambda (i.e.
/// a curry tail already folded into its head).
fn is_curry_tail(node: &rnix::SyntaxNode) -> bool {
    let Some(parent) = node.parent() else { return false };
    let Some(parent_lambda) = ast::Lambda::cast(parent) else { return false };
    parent_lambda
        .body()
        .map(|b| b.syntax() == node)
        .unwrap_or(false)
}

// ───────────────────────────────────────────────────────────────────────────
// Small helpers
// ───────────────────────────────────────────────────────────────────────────

fn node_span(node: &rnix::SyntaxNode) -> Span {
    let range = node.text_range();
    Span { start: range.start().into(), end: range.end().into() }
}

fn ident_text(ident: &ast::Ident) -> String {
    ident
        .ident_token()
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| ident.syntax().text().to_string())
}

/// The literal text of a simple `"…"` attribute string, if it has no
/// interpolation. Returns `None` for interpolated / non-trivial strings.
fn str_literal_text(s: &ast::Str) -> Option<String> {
    let mut out = String::new();
    for part in s.normalized_parts() {
        match part {
            ast::InterpolPart::Literal(text) => out.push_str(&text),
            ast::InterpolPart::Interpolation(_) => return None,
        }
    }
    Some(out)
}

/// Recursively collect every `.nix` file under `root`, skipping VCS and result
/// symlink noise.
fn collect_nix_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || name.starts_with("result") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "nix").unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
