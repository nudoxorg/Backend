//! Traversal of the evaluated flake output tree — the part that replaces
//! pasta's `genericClosure`, but in Rust against `snix_eval::Value` directly.
//!
//! Responsibilities:
//!   * walk `outputs.{packages,legacyPackages,devShells,apps,checks,formatter,
//!     overlays,nixosModules,homeModules,templates,lib,…}.<system>.…`, with
//!     per-node forcing wrapped so catchables/eval errors degrade to a
//!     documented "unevaluable" leaf and never abort the package;
//!   * stop-descend heuristics — `type = "derivation"` → package leaf,
//!     `_type = "option"` → option leaf (`options`), `recurseForDerivations`
//!     respected, depth + node-count budgets (nixpkgs `legacyPackages` is 120k
//!     nodes; full enumeration is gated to Phase 7);
//!   * **fusion** — a `Value::Closure` leaf resolves its `lambda`'s span back
//!     to the static table (doc comment, defaults, declared signature) and is
//!     married to runtime facts (required flags, ellipsis, `args@`);
//!   * **alias pass** — closures sharing a `(file, span)` collapse to one
//!     canonical `Symbol` whose `aliases` gets every attrpath that reached it.
//!
//! snix API touch-points are isolated in the small helpers at the bottom.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use ir::entry::NudoxPath;
use ir::function::Function;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use snix_eval::{SourceCode, Value};

use super::docs;
use super::item::path_of;
use super::package::FlakeMeta;
use super::syntax::StaticTable;
use super::{function, options, types};

/// The output categories we descend, in a stable order.
const OUTPUT_CATEGORIES: &[&str] = &[
    "lib",
    "packages",
    "legacyPackages",
    "devShells",
    "apps",
    "checks",
    "formatter",
    "overlays",
    "nixosModules",
    "homeModules",
    "templates",
];

/// Max descent depth and node budget — nixpkgs-scale enumeration is gated off
/// by default (Phase 7 decision).
const MAX_DEPTH: usize = 12;
const NODE_BUDGET: usize = 20_000;

struct Walk<'a> {
    source: &'a SourceCode,
    table:  &'a StaticTable,
    /// Canonical symbol per (file, span) for the alias pass.
    canonical: HashMap<(PathBuf, usize), NudoxPath>,
    /// Accumulated alias attrpaths per canonical path.
    aliases: HashMap<NudoxPath, HashSet<Vec<String>>>,
    entries: HashMap<NudoxPath, Entry>,
    roots:   Vec<NudoxPath>,
    budget:  usize,
    deadline: Instant,
}

/// Walk the evaluated `outputs` attrset and lower it into a [`Surface`].
pub fn walk(
    outputs: &Value,
    source: &SourceCode,
    table: &StaticTable,
    meta: &FlakeMeta,
    deadline: Instant,
) -> super::context::Surface {
    let mut walk = Walk {
        source,
        table,
        canonical: HashMap::default(),
        aliases: HashMap::default(),
        entries: HashMap::default(),
        roots: Vec::new(),
        budget: NODE_BUDGET,
        deadline,
    };

    let Some(attrs) = as_attrs(outputs) else {
        return super::context::Surface::default();
    };

    for (key, value) in attrs {
        if !OUTPUT_CATEGORIES.contains(&key.as_str()) {
            continue;
        }
        let category_path = path_of(&[key.clone()]);
        walk.mint_module(&category_path, &key, None);
        walk.roots.push(category_path.clone());

        match key.as_str() {
            "nixosModules" | "homeModules" => {
                walk.walk_modules(&value, &[key.clone()]);
            }
            _ => {
                // Most categories are `<system>.<name>` two-level maps; collapse
                // systems by lowering the union of member names once.
                walk.walk_category(&value, &[key.clone()], 0);
            }
        }
    }

    let _ = meta; // reserved for cross-linking flake-level metadata

    // Fold accumulated alias sets onto their canonical symbols.
    for (path, alias_set) in walk.aliases {
        if let Some(entry) = walk.entries.get_mut(&path) {
            set_aliases(entry, alias_set);
        }
    }

    super::context::Surface {
        entries: walk.entries.into_iter().collect(),
        roots:   walk.roots,
    }
}

impl<'a> Walk<'a> {
    /// Generic descent through a `<system>.<member>.…` category, collapsing
    /// per-system duplication (identical member names across systems dedupe to
    /// one entry; the systems are recorded via the shared attrpath).
    fn walk_category(&mut self, value: &Value, prefix: &[String], depth: usize) {
        if depth > MAX_DEPTH || self.budget == 0 || Instant::now() > self.deadline {
            return;
        }
        let Some(attrs) = as_attrs(value) else { return };

        for (key, child) in attrs {
            self.budget = self.budget.saturating_sub(1);
            if self.budget == 0 {
                tracing::warn!(prefix = ?prefix, "nix walker: node budget exhausted; truncating");
                return;
            }

            // A per-system layer (`x86_64-linux`, …) is transparent: descend
            // without extending the semantic path.
            if is_system_name(&key) {
                self.walk_category(&child, prefix, depth);
                continue;
            }

            let mut path_segs = prefix.to_vec();
            path_segs.push(key.clone());

            match classify(&child) {
                Node::Derivation => self.mint_derivation(&path_segs, &child),
                Node::Option => { /* options handled in walk_modules */ }
                Node::Closure => self.mint_closure(&path_segs, &child),
                Node::Attrs if should_descend(&child) => {
                    let node_path = path_of(&path_segs);
                    self.mint_module(&node_path, &key, None);
                    self.walk_category(&child, &path_segs, depth + 1);
                }
                Node::Attrs => {
                    // A leaf attrset that isn't a derivation and shouldn't be
                    // recursed — record it as a record-shaped constant.
                    self.mint_constant(&path_segs, None);
                }
                Node::Scalar | Node::Unevaluable => self.mint_constant(&path_segs, None),
            }
        }
    }

    /// Walk `nixosModules`/`homeModules`, delegating each module to `options`.
    fn walk_modules(&mut self, value: &Value, prefix: &[String]) {
        let Some(attrs) = as_attrs(value) else { return };
        for (key, module) in attrs {
            let mut segs = prefix.to_vec();
            segs.push(key.clone());
            let entries = options::extract_module(&module, &segs, self.source, self.table);
            for (path, entry) in entries {
                self.entries.insert(path, entry);
            }
        }
    }

    /// A derivation leaf → `Entry::Constant` typed `Derivation`, documentation
    /// harvested from `meta.*` in deterministic markdown.
    fn mint_derivation(&mut self, segs: &[String], value: &Value) {
        let path = path_of(segs);
        let name = segs.last().cloned().unwrap_or_default();
        let documentation = harvest_meta(value);
        let symbol = Symbol {
            name,
            path: path.clone(),
            aliases: None,
            visibility: Visibility::Public,
            documentation,
            deprecation: None,
            doc_links: None,
            inner: (),
        };
        // Constants carry no type slot; the Derivation typing is conveyed via
        // the (documented) meta block and the render backend.
        let _ = types::derivation_type();
        self.entries.insert(path, Entry::Constant(symbol));
    }

    /// A closure leaf → `Entry::Function`, fused with the static table by span
    /// and joined into the alias group for its `(file, span)`.
    fn mint_closure(&mut self, segs: &[String], value: &Value) {
        let path = path_of(segs);
        let name = segs.last().cloned().unwrap_or_default();

        // Fusion: resolve the closure's declaration span.
        let span = closure_span(value, self.source);

        // Alias unification: if another attrpath already reached this exact
        // (file, span), this is the same function — record the alias and stop.
        if let Some(key) = &span {
            if let Some(canonical) = self.canonical.get(key).cloned() {
                self.aliases
                    .entry(canonical)
                    .or_default()
                    .insert(segs.to_vec());
                return;
            }
            self.canonical.insert(key.clone(), path.clone());
        }

        // Build the Function from the static record when we can fuse (doc +
        // defaults + declared signature), else a bare shell with runtime
        // formals only.
        let (function, documentation) = match &span {
            Some((file, offset)) => self.fuse_function(file, *offset, value),
            None => (runtime_only_function(value), None),
        };

        let symbol = Symbol {
            name,
            path: path.clone(),
            aliases: None, // filled by the alias-fold pass after the walk
            visibility: Visibility::Public,
            documentation,
            deprecation: None,
            doc_links: None,
            inner: function,
        };
        self.entries.insert(path, Entry::Function(symbol));
    }

    /// Fuse the static lambda record at `(file, offset)` with runtime formals,
    /// returning the built `Function` and its documentation markdown.
    ///
    /// The static record supplies defaults, the parsed `::` signature, and the
    /// doc comment (RFC 145 binding-beats-lambda: prefer the owning binding's
    /// comment, fall back to the lambda's own); the runtime closure supplies
    /// required-flags via `apply_runtime_formals`.
    fn fuse_function(
        &self,
        file: &PathBuf,
        offset: usize,
        value: &Value,
    ) -> (Function, Option<String>) {
        let Some((sf, lambda)) = self.table.lambda_at(file, offset) else {
            return (runtime_only_function(value), None);
        };

        // Recover the raw doc: the binding that points at this lambda, else the
        // lambda's own attached comment.
        let lambda_idx = sf.lambdas.iter().position(|l| l.span == lambda.span);
        let raw = sf
            .bindings
            .iter()
            .find(|b| b.lambda == lambda_idx)
            .and_then(|b| b.doc.clone())
            .or_else(|| lambda.doc.clone());

        let parsed = raw.as_deref().map(docs::parse_doc).unwrap_or_default();
        let sig = function::signature_from_doc(&parsed);

        let mut function = function::lower_lambda(lambda, &parsed, sig.as_ref());
        apply_runtime_formals(&mut function, value);

        let documentation = (!parsed.markdown.is_empty()).then(|| parsed.markdown.clone());
        (function, documentation)
    }

    fn mint_module(&mut self, path: &NudoxPath, name: &str, documentation: Option<String>) {
        self.entries.entry(path.clone()).or_insert_with(|| {
            Entry::Module(Symbol {
                name: name.to_string(),
                path: path.clone(),
                aliases: None,
                visibility: Visibility::Public,
                documentation,
                deprecation: None,
                doc_links: None,
                inner: Module { members: None },
            })
        });
    }

    fn mint_constant(&mut self, segs: &[String], documentation: Option<String>) {
        let path = path_of(segs);
        let name = segs.last().cloned().unwrap_or_default();
        self.entries.insert(
            path.clone(),
            Entry::Constant(Symbol {
                name,
                path,
                aliases: None,
                visibility: Visibility::Public,
                documentation,
                deprecation: None,
                doc_links: None,
                inner: (),
            }),
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Node classification & heuristics
// ───────────────────────────────────────────────────────────────────────────

enum Node {
    Derivation,
    Option,
    Closure,
    Attrs,
    Scalar,
    Unevaluable,
}

fn classify(value: &Value) -> Node {
    match value {
        Value::Closure(_) | Value::Builtin(_) => Node::Closure,
        Value::Attrs(_) => {
            if is_derivation(value) {
                Node::Derivation
            } else if is_option(value) {
                Node::Option
            } else {
                Node::Attrs
            }
        }
        Value::Thunk(_) => Node::Unevaluable,
        _ => Node::Scalar,
    }
}

/// Whether we should recurse into a non-derivation attrset: respect
/// `recurseForDerivations = true`, otherwise descend by default up to budget.
fn should_descend(value: &Value) -> bool {
    if let Some(attrs) = as_attrs(value) {
        // If it explicitly opts out, stop.
        for (k, v) in &attrs {
            if k == "recurseForDerivations" {
                return matches!(v, Value::Bool(true));
            }
        }
    }
    true
}

// ───────────────────────────────────────────────────────────────────────────
// snix API touch-points (isolated)
// ───────────────────────────────────────────────────────────────────────────

/// Force-free view of an attrset's already-evaluated members as owned
/// `(name, Value)` pairs in key order. Thunks are surfaced as-is (the caller
/// classifies them as `Unevaluable`). Returns `None` for non-attrs.
fn as_attrs(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Attrs(attrs) => {
            let mut out: Vec<(String, Value)> = attrs
                .iter()
                .map(|(k, v)| (nix_string(k), v.clone()))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Some(out)
        }
        _ => None,
    }
}

/// Select one attribute by name from an attrset value.
fn select<'v>(value: &'v Value, key: &str) -> Option<Value> {
    as_attrs(value)?.into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn is_derivation(value: &Value) -> bool {
    matches!(select(value, "type"), Some(Value::String(ref s)) if nix_string(s) == "derivation")
}

fn is_option(value: &Value) -> bool {
    matches!(select(value, "_type"), Some(Value::String(ref s)) if nix_string(s) == "option")
}

/// Convert a `NixString` to a Rust `String`. (Isolated so the exact snix
/// string API is a single touch-point.)
fn nix_string(s: &snix_eval::NixString) -> String {
    s.to_string()
}

/// A system attribute name like `x86_64-linux` / `aarch64-darwin`.
fn is_system_name(key: &str) -> bool {
    matches!(
        key,
        "x86_64-linux"
            | "aarch64-linux"
            | "x86_64-darwin"
            | "aarch64-darwin"
            | "i686-linux"
            | "armv7l-linux"
            | "riscv64-linux"
    )
}

/// Resolve a closure's declaration site to `(relative file path, byte offset)`
/// via its lambda chunk's first span + the `SourceCode` codemap. This is the
/// fusion key against the static table.
fn closure_span(value: &Value, source: &SourceCode) -> Option<(PathBuf, usize)> {
    let Value::Closure(closure) = value else { return None };
    // `closure.lambda` → `Rc<Lambda>`; `lambda.chunk` → `Chunk`;
    // `chunk.first_span()` → `codemap::Span`; the codemap resolves it to a file.
    let span = closure.chunk().first_span();
    let file = source.get_file(span);
    let name = file.name();
    let file_start = file.span.low();
    let offset = usize::try_from(span.low() - file_start).unwrap_or(0);
    Some((PathBuf::from(name), offset))
}

/// Harvest `meta.*` from a derivation into deterministic markdown
/// (description, longDescription, mainProgram, license, platforms,
/// maintainers, homepage) — the field set flake-info proved out.
fn harvest_meta(value: &Value) -> Option<String> {
    let meta = select(value, "meta")?;
    let mut out = String::new();
    let mut push = |label: &str, key: &str| {
        if let Some(Value::String(s)) = select(&meta, key) {
            out.push_str(&format!("**{label}:** {}\n\n", nix_string(&s)));
        }
    };
    push("Description", "description");
    push("Details", "longDescription");
    push("Main program", "mainProgram");
    push("Homepage", "homepage");
    // license / platforms / maintainers are often structured; render names only.
    if let Some(Value::String(s)) = select(&meta, "license").and_then(|l| select(&l, "fullName")) {
        out.push_str(&format!("**License:** {}\n\n", nix_string(&s)));
    }
    (!out.trim().is_empty()).then(|| out.trim().to_string())
}

/// Apply runtime formals (required flags, ellipsis, `args@`) recovered from the
/// closure to a statically-built `Function` (which already has defaults +
/// types). Only fields the evaluator knows better than the static layer are
/// overwritten.
fn apply_runtime_formals(function: &mut Function, value: &Value) {
    let Value::Closure(closure) = value else { return };
    // `Lambda.formals` is a `pub` field (widened by the snix visibility patch),
    // `Formals.arguments` a `BTreeMap<NixString, bool /*required*/>`.
    let Some(formals) = &closure.lambda.formals else { return };
    let required: BTreeMap<String, bool> = formals
        .arguments
        .iter()
        .map(|(k, req)| (nix_string(k), *req))
        .collect();
    if let Some(params) = &mut function.input_parameters {
        for p in params.iter_mut() {
            if let ir::parameter::Parameter::Literal(l) = p {
                if let Some(req) = required.get(&l.name) {
                    // A non-required formal without a static default is optional.
                    if !*req && l.default_value.is_none() {
                        let attrs = l.attributes.get_or_insert_with(Vec::new);
                        if !attrs.contains(&ir::parameter::ParameterAttribute::Optional) {
                            attrs.push(ir::parameter::ParameterAttribute::Optional);
                        }
                    }
                }
            }
        }
    }
}

/// Build a `Function` from only the runtime closure (no static record found).
fn runtime_only_function(value: &Value) -> Function {
    let mut params = Vec::new();
    if let Value::Closure(closure) = value {
        if let Some(formals) = &closure.lambda.formals {
            for (name, required) in formals.arguments.iter() {
                let mut attrs = Vec::new();
                if !*required {
                    attrs.push(ir::parameter::ParameterAttribute::Optional);
                }
                params.push(ir::parameter::Parameter::Literal(ir::parameter::LiteralParameter {
                    name:          nix_string(name),
                    r#type:        None,
                    attributes:    (!attrs.is_empty()).then_some(attrs),
                    default_value: None,
                    description:   None,
                }));
            }
        }
    }
    Function {
        input_parameters:      (!params.is_empty()).then_some(params),
        output_parameters:     None,
        type_links:            None,
        attributes:            None,
        generics:              None,
        receiver:              None,
        overloads:             None,
        implemented:           true,
        members:               None,
        implemented_protocols: None,
    }
}

/// Attach an alias set to whatever entry kind carries it.
fn set_aliases(entry: &mut Entry, aliases: HashSet<Vec<String>>) {
    let slot = match entry {
        Entry::Module(s) => &mut s.aliases,
        Entry::Function(s) => &mut s.aliases,
        Entry::RecordType(s) => &mut s.aliases,
        Entry::Constant(s) => &mut s.aliases,
        Entry::TypeAlias(s) => &mut s.aliases,
        _ => return,
    };
    if aliases.is_empty() {
        return;
    }
    *slot = Some(aliases);
}
