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
//!   * **package dual surface** — when a derivation still exposes `override`
//!     (callPackage product), also mint a Function at `<path>/override` from
//!     the override closure's formals (best-effort).
//!
//! snix API touch-points are isolated in the small helpers at the bottom.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use ir::entry::NudoxPath;
use ir::function::Function;
use ir::kind::{Entry, Symbol, TypedBinding, Visibility};
use ir::module::Module;
use ir::parameter::{LiteralParameter, Parameter, ParameterAttribute};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use smol_str::SmolStr;
use snix_eval::{
	EvalIO, EvalMode, Evaluation, GlobalsMap, SourceCode, Value,
};

use super::docs;
use super::item::path_of;
use super::package::FlakeMeta;
use super::syntax::StaticTable;
use super::{function, options, types};

/// Capability bundle for forcing suspended thunks via a one-shot snix VM.
///
/// snix's `Thunk::force` is GenCo/async and only runs inside the evaluator.
/// After `Evaluation::evaluate` returns, remaining suspended thunks are forced
/// by re-entering the VM: inject the value as a top-level binding and evaluate
/// it under [`EvalMode::Strict`] (which deep-forces the result).
pub struct ForceHost {
	pub io:      Rc<dyn EvalIO>,
	pub globals: Rc<GlobalsMap>,
	pub source:  SourceCode,
}

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
/// Per-node force budget (peeling nested forced thunks + VM re-entry).
const FORCE_BUDGET_PER_NODE: usize = 32;

struct Walk<'a> {
    source: &'a SourceCode,
    table:  &'a StaticTable,
    /// Optional host for GenCo/VM force of suspended thunks.
    force_host: Option<&'a ForceHost>,
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
	walk_with_force(outputs, source, table, meta, deadline, None)
}

/// Like [`walk`], but suspended thunks are forced via a one-shot snix VM when
/// `force_host` is provided (GenCo path).
pub fn walk_with_force(
    outputs: &Value,
    source: &SourceCode,
    table: &StaticTable,
    meta: &FlakeMeta,
    deadline: Instant,
    force_host: Option<&ForceHost>,
) -> super::context::Surface {
    let mut walk = Walk {
        source,
        table,
        force_host,
        canonical: HashMap::default(),
        aliases: HashMap::default(),
        entries: HashMap::default(),
        roots: Vec::new(),
        budget: NODE_BUDGET,
        deadline,
    };

    // Force the top-level outputs attrset (budgeted peel + optional VM).
    let outputs = match force_budgeted(outputs, &mut FORCE_BUDGET_PER_NODE.clone(), force_host) {
        ForceOutcome::Value(v) => v,
        ForceOutcome::Unevaluable(note) => {
            tracing::warn!(%note, "nix walker: outputs tree unevaluable; empty dynamic surface");
            return super::context::Surface::default();
        }
    };

    let Some(attrs) = as_attrs(&outputs) else {
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

        let mut force_budget = FORCE_BUDGET_PER_NODE;
        let value = match force_budgeted(value, &mut force_budget, self.force_host) {
            ForceOutcome::Value(v) => v,
            ForceOutcome::Unevaluable(note) => {
                // Document the gap; do not abort the package.
                self.mint_unevaluable(prefix, &note);
                return;
            }
        };

        let Some(attrs) = as_attrs(&value) else {
            // Forced to a non-attrset leaf under a category — record as constant.
            if !prefix.is_empty() {
                self.mint_constant(prefix, None, &value);
            }
            return;
        };

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

            let mut child_budget = FORCE_BUDGET_PER_NODE;
            let child = match force_budgeted(&child, &mut child_budget, self.force_host) {
                ForceOutcome::Value(v) => v,
                ForceOutcome::Unevaluable(note) => {
                    self.mint_unevaluable(&path_segs, &note);
                    continue;
                }
            };

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
                    self.mint_constant(&path_segs, None, &child);
                }
                Node::Scalar => self.mint_constant(&path_segs, None, &child),
                Node::Unevaluable => {
                    self.mint_unevaluable(&path_segs, "value remained a thunk after force budget");
                }
            }
        }
    }

    /// Walk `nixosModules`/`homeModules`, delegating each module to `options`.
    fn walk_modules(&mut self, value: &Value, prefix: &[String]) {
        let mut force_budget = FORCE_BUDGET_PER_NODE;
        let value = match force_budgeted(value, &mut force_budget, self.force_host) {
            ForceOutcome::Value(v) => v,
            ForceOutcome::Unevaluable(_) => return,
        };
        let Some(attrs) = as_attrs(&value) else { return };
        for (key, module) in attrs {
            let mut segs = prefix.to_vec();
            segs.push(key.clone());
            let mut mb = FORCE_BUDGET_PER_NODE;
            let module = match force_budgeted(&module, &mut mb, self.force_host) {
                ForceOutcome::Value(v) => v,
                ForceOutcome::Unevaluable(note) => {
                    self.mint_unevaluable(&segs, &note);
                    continue;
                }
            };
            let entries = options::extract_module(&module, &segs, self.source, self.table);
            for (path, entry) in entries {
                self.entries.insert(path, entry);
            }
        }
    }

    /// A derivation leaf → `Entry::Constant` typed `Derivation`, documentation
    /// harvested from `meta.*` in deterministic markdown. Dual surface: when
    /// the derivation still exposes an `override` closure (callPackage
    /// product), also mint a Function at `<path>/override`.
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
            // Typed as Derivation via value_kind_type / derivation_type.
            inner: types::derivation_binding(),
        };
        self.entries.insert(path, Entry::Constant(symbol));

        // Best-effort dual surface for callPackage products.
        if let Some(override_val) = select(value, "override") {
            let mut fb = FORCE_BUDGET_PER_NODE;
            if let ForceOutcome::Value(ov) = force_budgeted(&override_val, &mut fb, self.force_host) {
                if matches!(classify(&ov), Node::Closure) {
                    let mut override_segs = segs.to_vec();
                    override_segs.push("override".to_string());
                    self.mint_closure(&override_segs, &ov);
                }
            }
        }
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
    /// required-flags / ellipsis / `args@` via `apply_runtime_formals`.
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

        let documentation = parsed.to_documentation();
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

    fn mint_constant(&mut self, segs: &[String], documentation: Option<String>, value: &Value) {
        let path = path_of(segs);
        let name = segs.last().cloned().unwrap_or_default();
        let inner = types::typed_binding_from_value(value);
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
                inner,
            }),
        );
    }

    /// An unevaluable leaf — still present in the index so search can see the
    /// attrpath, with documentation explaining why it could not be forced.
    fn mint_unevaluable(&mut self, segs: &[String], note: &str) {
        let path = path_of(segs);
        let name = segs.last().cloned().unwrap_or_default();
        let documentation = Some(format!(
            "_Unevaluable:_ {note}. The binding is present in the output tree \
             but could not be forced within the walker budget (catchable error, \
             suspended thunk, or force budget exhausted)."
        ));
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
                inner: TypedBinding {
                    ty:      Some(ir::ty::Type::Any),
                    value:   None,
                    mutable: Some(false),
                },
            }),
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Budgeted, catchable force
// ───────────────────────────────────────────────────────────────────────────

/// Outcome of a budgeted force attempt.
#[derive(Debug)]
enum ForceOutcome {
    Value(Value),
    Unevaluable(String),
}

/// Force a value as far as possible.
///
/// 1. Peel already-forced nested thunks (no VM).
/// 2. If still suspended and a [`ForceHost`] is available, re-enter snix with
///    `EvalMode::Strict` injecting the value as env binding `v` — this is the
///    GenCo/`Thunk::force` path (strict evaluation deep-forces via the VM).
/// 3. Catchable / panic / budget exhaust → Unevaluable (never abort package).
fn force_budgeted(
	value: &Value,
	budget: &mut usize,
	host: Option<&ForceHost>,
) -> ForceOutcome {
	let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
		force_budgeted_inner(value, budget, host)
	}));
	match result {
		Ok(outcome) => outcome,
		Err(_) => ForceOutcome::Unevaluable(
			"snix force panicked (blackhole / internal invariant)".into(),
		),
	}
}

fn force_budgeted_inner(
	value: &Value,
	budget: &mut usize,
	host: Option<&ForceHost>,
) -> ForceOutcome {
	if *budget == 0 {
		return ForceOutcome::Unevaluable("force budget exhausted".into());
	}
	*budget = budget.saturating_sub(1);

	match value {
		Value::Catchable(c) => ForceOutcome::Unevaluable(format!("catchable: {c:?}")),
		Value::Thunk(thunk) => {
			if thunk.is_evaluated() {
				let inner = thunk.value().clone();
				return force_budgeted_inner(&inner, budget, host);
			}
			// Suspended / native — re-enter snix VM when a host is available.
			if thunk.is_suspended() {
				if let Some(host) = host {
					return force_suspended_via_vm(host, value.clone(), budget);
				}
				return ForceOutcome::Unevaluable(
					"suspended thunk (no ForceHost / GenCo path available)".into(),
				);
			}
			if let Some(host) = host {
				return force_suspended_via_vm(host, value.clone(), budget);
			}
			ForceOutcome::Unevaluable("thunk not evaluable without VM context".into())
		}
		other => ForceOutcome::Value(other.clone()),
	}
}

/// Re-enter snix: inject `value` as env `v`, evaluate `v` under Strict mode so
/// the VM deep-forces via GenCo/`Thunk::force`.
fn force_suspended_via_vm(
	host: &ForceHost,
	value: Value,
	budget: &mut usize,
) -> ForceOutcome {
	if *budget == 0 {
		return ForceOutcome::Unevaluable("force budget exhausted before VM re-entry".into());
	}
	*budget = budget.saturating_sub(1);

	// snix_eval's EvaluationBuilder::env requires FxBuildHasher (not the
	// BuildHasherDefault that FxHashMap::default() uses on some rustc_hash versions).
	let mut env: rustc_hash::FxHashMap<SmolStr, Value> =
		rustc_hash::FxHashMap::with_capacity_and_hasher(1, rustc_hash::FxBuildHasher);
	env.insert(SmolStr::new_static("v"), value);

	let evaluation = Evaluation::builder(host.io.clone())
		.with_globals(host.globals.clone())
		.with_source_map(host.source.clone())
		.env(Some(&env))
		.mode(EvalMode::Strict)
		.enable_import()
		.build();

	let result = evaluation.evaluate("v", None);
	if !result.errors.is_empty() {
		return ForceOutcome::Unevaluable(format!(
			"VM force failed: {} error(s)",
			result.errors.len()
		));
	}
	match result.value {
		Some(v) => {
			// Strict mode deep-forces; still peel any residual nested thunks.
			force_budgeted_inner(&v, budget, None)
		}
		None => ForceOutcome::Unevaluable("VM force returned no value".into()),
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
        Value::Catchable(_) => Node::Unevaluable,
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
/// forces them via [`force_budgeted`]). Returns `None` for non-attrs.
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
fn select(value: &Value, key: &str) -> Option<Value> {
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
/// overwritten / filled in.
///
/// Full contract:
/// 1. Mark formals optional when the runtime says `required = false` and the
///    static layer has no default.
/// 2. Emit a synthetic `...` variadic parameter when the runtime formals have
///    an ellipsis and the function does not already carry one.
/// 3. Emit an `args@` bind parameter when the runtime formals name the full
///    attribute set and the function does not already carry that name.
/// 4. Mark the function itself `Attribute::Variadic` when ellipsis is present.
fn apply_runtime_formals(function: &mut Function, value: &Value) {
    let Value::Closure(closure) = value else { return };
    // `Lambda.formals` is a `pub` field (widened by the snix visibility patch),
    // `Formals.arguments` a `BTreeMap<NixString, bool /*required*/>`.
    let Some(formals) = &closure.lambda.formals else {
        // Simple `x: …` lambda — nothing more to fuse.
        return;
    };
    let required: BTreeMap<String, bool> = formals
        .arguments
        .iter()
        .map(|(k, req)| (nix_string(k), *req))
        .collect();

    // Ensure we have a mutable parameter list to work with.
    let params = function.input_parameters.get_or_insert_with(Vec::new);

    // 1. Required flags → Optional attribute.
    for p in params.iter_mut() {
        if let Parameter::Literal(l) = p {
            if let Some(req) = required.get(&l.name) {
                // A non-required formal without a static default is optional.
                if !*req && l.default_value.is_none() {
                    let attrs = l.attributes.get_or_insert_with(Vec::new);
                    if !attrs.contains(&ParameterAttribute::Optional) {
                        attrs.push(ParameterAttribute::Optional);
                    }
                }
            }
        }
    }

    // 2. Ellipsis → synthetic `...` + Function Attribute::Variadic.
    if formals.ellipsis {
        let has_ellipsis = params.iter().any(|p| {
            matches!(p, Parameter::Literal(l) if l.name == "..."
                || l.attributes.as_ref().is_some_and(|a| a.contains(&ParameterAttribute::Variadic)))
        });
        if !has_ellipsis {
            params.push(Parameter::Literal(LiteralParameter {
                name:          "...".to_string(),
                r#type:        None,
                attributes:    Some(vec![ParameterAttribute::Variadic]),
                default_value: None,
                description:   Some(
                    "Additional attributes accepted via pattern ellipsis (runtime).".into(),
                ),
            }));
        }
        let attrs = function.attributes.get_or_insert_with(Vec::new);
        if !attrs.contains(&ir::function::Attribute::Variadic) {
            attrs.push(ir::function::Attribute::Variadic);
        }
    }

    // 3. `args@` bind name.
    if let Some(bind) = &formals.name {
        let has_bind = params.iter().any(|p| {
            matches!(p, Parameter::Literal(l) if &l.name == bind)
        });
        if !has_bind {
            params.insert(
                0,
                Parameter::Literal(LiteralParameter {
                    name:          bind.clone(),
                    r#type:        Some(types::attrset_type()),
                    attributes:    None,
                    default_value: None,
                    description:   Some(format!(
                        "Full attribute-set argument bound via `{bind}@` pattern (runtime)."
                    )),
                }),
            );
        }
    }

    // Drop the parameter list if it ended up empty (shouldn't).
    if params.is_empty() {
        function.input_parameters = None;
    }
}

/// Build a `Function` from only the runtime closure (no static record found).
fn runtime_only_function(value: &Value) -> Function {
    let mut params = Vec::new();
    let mut attributes = None;
    if let Value::Closure(closure) = value {
        if let Some(formals) = &closure.lambda.formals {
            // args@ first.
            if let Some(bind) = &formals.name {
                params.push(Parameter::Literal(LiteralParameter {
                    name:          bind.clone(),
                    r#type:        Some(types::attrset_type()),
                    attributes:    None,
                    default_value: None,
                    description:   Some(format!(
                        "Full attribute-set argument bound via `{bind}@` pattern."
                    )),
                }));
            }
            for (name, required) in formals.arguments.iter() {
                let mut attrs = Vec::new();
                if !*required {
                    attrs.push(ParameterAttribute::Optional);
                }
                params.push(Parameter::Literal(LiteralParameter {
                    name:          nix_string(name),
                    r#type:        None,
                    attributes:    (!attrs.is_empty()).then_some(attrs),
                    default_value: None,
                    description:   None,
                }));
            }
            if formals.ellipsis {
                params.push(Parameter::Literal(LiteralParameter {
                    name:          "...".to_string(),
                    r#type:        None,
                    attributes:    Some(vec![ParameterAttribute::Variadic]),
                    default_value: None,
                    description:   Some(
                        "Additional attributes accepted via pattern ellipsis.".into(),
                    ),
                }));
                attributes = Some(vec![ir::function::Attribute::Variadic]);
            }
        } else if !closure.lambda.param_name.is_empty() {
            // Simple `x: …` form.
            params.push(Parameter::Literal(LiteralParameter {
                name:          closure.lambda.param_name.to_string(),
                r#type:        None,
                attributes:    None,
                default_value: None,
                description:   None,
            }));
        }
    }
    Function {
        input_parameters:      (!params.is_empty()).then_some(params),
        output_parameters:     None,
        type_links:            None,
        attributes,
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

#[cfg(test)]
mod force_tests {
    use super::*;
    use snix_eval::CatchableErrorKind;

    #[test]
    fn force_budgeted_scalar_ok() {
        let mut budget = 8;
        match force_budgeted(&Value::Integer(42), &mut budget, None) {
            ForceOutcome::Value(Value::Integer(42)) => {}
            other => panic!("expected Integer(42), got {other:?}"),
        }
    }

    /// Catchable values must degrade to Unevaluable — never panic, never
    /// abort the package. This is the forced-thunk failure path the walker
    /// relies on when a node eval throws.
    #[test]
    fn force_budgeted_catchable_is_unevaluable_not_panic() {
        let catchable = Value::from(CatchableErrorKind::AssertionFailed);
        let mut budget = 8;
        match force_budgeted(&catchable, &mut budget, None) {
            ForceOutcome::Unevaluable(note) => {
                assert!(
                    note.to_ascii_lowercase().contains("catchable")
                        || note.to_ascii_lowercase().contains("assert"),
                    "expected catchable note, got {note}"
                );
            }
            ForceOutcome::Value(v) => panic!("catchable must not force to {v:?}"),
        }
        // Budget still usable after the soft failure.
        assert!(budget > 0);
    }

    #[test]
    fn force_budget_exhaustion_is_unevaluable() {
        let mut budget = 0;
        match force_budgeted(&Value::Integer(1), &mut budget, None) {
            ForceOutcome::Unevaluable(note) => {
                assert!(note.contains("budget"), "note={note}");
            }
            ForceOutcome::Value(_) => panic!("budget 0 must not force"),
        }
    }
}
