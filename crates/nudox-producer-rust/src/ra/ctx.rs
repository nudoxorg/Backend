//! Lowering context: canonical path → RaId map, impl index, alias cache,
//! duplicate-name counter, and visibility/doc helpers.
//!
//! # What changed vs the old ctx.rs
//!
//! The old context tracked `NudoxPath` (a `Local`/`External` enum pointing to a
//! `PathBuf`).  That type is **deleted** in the new IR.  We keep the canonical
//! path string as `RaId` directly.  `NudoxPath` is gone; reference identity is
//! achieved via `Lowering::refer(id)` which resolves at `finish` time.
//!
//! The `cache: FxHashMap<PathKey, Entry>` and `deferred: Vec<Entry>` from the
//! old code are also gone — the new `Lowering` sink is order-independent, so
//! every item can be declared in one pass without buffering.
//!
//! The `duplicate counter` is new: when two items share the same canonical path
//! (e.g. two `fn new` in different inherent impls) we append `#1`, `#2`, … so
//! `Lowering` does not see duplicate keys.

use std::sync::Arc;

use ra_ap_base_db::SourceDatabase;
use ra_ap_hir::{
    Adt, Crate, DisplayTarget, HasVisibility, Impl, Module, ModuleDef, ScopeDef, Semantics,
    Trait, Visibility,
};
use ra_ap_ide_db::{FileId, RootDatabase, line_index::LineIndex};
use ra_ap_syntax::Edition;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use smol_str::SmolStr;
use std::hash::Hasher;

use nudox_ir::entry::{Deprecation, DocLink, Visibility as IrVisibility};

use super::docs;
use crate::RaId;

/// A canonical path key, e.g. `"my_crate::Foo::bar"`.
pub(crate) type PathKey = SmolStr;

/// Bucketed impls for inherent methods + trait protocol membership.
#[derive(Default)]
pub(crate) struct ImplIndex {
    /// ADT-keyed local inherent/trait impls.
    pub(crate) by_self_ty: FxHashMap<PathKey, Vec<Impl>>,
    /// Blanket impls whose self_ty is a bare generic param.
    pub(crate) blanket: Vec<(Trait, Impl)>,
}

/// Per-crate lowering state.
pub(crate) struct LowerCtx<'db> {
    pub(crate) db: &'db RootDatabase,
    pub(crate) sema: Semantics<'db, RootDatabase>,
    pub(crate) krate: Crate,
    pub(crate) display: DisplayTarget,
    pub(crate) edition: Edition,
    pub(crate) document_private: bool,

    /// Canonical path string → `RaId` (same string; kept for cache hit test).
    pub(crate) path_cache: FxHashMap<PathKey, RaId>,

    /// All public paths per def (re-export aliases, incl. globs).
    pub(crate) alias_cache: FxHashMap<PathKey, FxHashSet<Vec<String>>>,

    /// Trait- and self-ty-bucketed impls.
    pub(crate) impls: ImplIndex,

    /// FileId → LineIndex, lazily built for source-map extraction.
    pub(crate) lines: FxHashMap<FileId, Arc<LineIndex>>,

    pub(crate) visiting: FxHashSet<PathKey>,

    /// Counter per canonical path stem; used to disambiguate overloads.
    ///
    /// When `(parent_path + "::" + name)` is seen for the N-th time (N>0) we
    /// suffix the `RaId` with `#N`.  The counter is keyed by the *un-suffixed*
    /// path so the suffix is stable across items.
    overload_count: FxHashMap<PathKey, u32>,
}

impl<'db> LowerCtx<'db> {
    pub(crate) fn new(db: &'db RootDatabase, krate: Crate, document_private: bool) -> Self {
        let display = krate.to_display_target(db);
        let edition = krate.edition(db);
        LowerCtx {
            sema: Semantics::new(db),
            db,
            krate,
            display,
            edition,
            document_private,
            path_cache: FxHashMap::default(),
            alias_cache: FxHashMap::default(),
            impls: ImplIndex::default(),
            lines: FxHashMap::default(),
            visiting: FxHashSet::default(),
            overload_count: FxHashMap::default(),
        }
    }

    // ── Path / ID helpers ─────────────────────────────────────────────────────

    /// Compute the canonical path key for `def`.
    ///
    /// Returns `None` for items whose path cannot be determined (anonymous
    /// `impl`s that have no defining name, etc.).
    pub(crate) fn canonical(&mut self, def: ModuleDef) -> Option<PathKey> {
        let segments = self.path_segments(def)?;
        let key = PathKey::from(segments.join("::"));
        if !self.path_cache.contains_key(&key) {
            self.path_cache.insert(key.clone(), key.clone());
        }
        Some(key)
    }

    /// Canonical path as a `RaId`.  Same as `canonical` — both return the same
    /// string; the type alias distinction is documentary only.
    pub(crate) fn ra_id(&mut self, def: ModuleDef) -> Option<RaId> {
        self.canonical(def)
    }

    /// Allocate a unique `RaId` for a method or associated item at
    /// `parent_path::name`.
    ///
    /// When two items share the same `(parent_path, name)` pair (e.g. two
    /// inherent `impl` blocks both supplying `fn new`) the second and subsequent
    /// ones get a `#N` suffix so `Lowering` does not see duplicate keys.
    pub(crate) fn method_id(&mut self, parent_path: &PathKey, name: &str) -> RaId {
        let stem = PathKey::from(format!("{parent_path}::{name}"));
        let count = self.overload_count.entry(stem.clone()).or_insert(0);
        if *count == 0 {
            *count = 1;
            stem
        } else {
            let id = PathKey::from(format!("{stem}#{}", *count));
            *count += 1;
            id
        }
    }

    /// Alias segment vectors for a canonical key (excluding the primary path).
    pub(crate) fn aliases_of(&self, key: &PathKey) -> Option<&FxHashSet<Vec<String>>> {
        self.alias_cache.get(key).filter(|s| !s.is_empty())
    }

    // ── Visibility ────────────────────────────────────────────────────────────

    /// Gate: include `def` given the `document_private` setting.
    pub(crate) fn include(&self, def: ModuleDef) -> bool {
        if self.document_private {
            return true;
        }
        matches!(def.visibility(self.db), Visibility::Public)
    }

    /// Map HIR visibility → IR visibility.
    ///
    /// | RA                          | IR                  |
    /// |-----------------------------|---------------------|
    /// | `Public`                    | `Public`            |
    /// | `PubCrate(_)`               | `Crate`             |
    /// | `Module(_, Implicit)`       | `Private`           |
    /// | `Module(root, Explicit)`    | `Crate`             |
    /// | `Module(other, Explicit)`   | `Package`           |
    pub(crate) fn visibility(&self, def: impl HasVisibility) -> IrVisibility {
        match def.visibility(self.db) {
            Visibility::Public => IrVisibility::Public,
            Visibility::PubCrate(_) => IrVisibility::Crate,
            Visibility::Module(m, expl) => {
                if !expl.is_explicit() {
                    return IrVisibility::Private;
                }
                let module = Module::from(m);
                if module.is_crate_root(self.db) {
                    IrVisibility::Crate
                } else {
                    IrVisibility::Package
                }
            }
        }
    }

    // ── Documentation / deprecation / cfg ────────────────────────────────────

    /// Build the `Symbol` metadata fields for a `ModuleDef`.
    ///
    /// Returns `(name, visibility, documentation, aliases, deprecation, doc_links, cfg)`.
    /// The IR `Symbol` is assembled by each item lowerer so it can supply the
    /// correct `source` path and `span`.
    pub(crate) fn symbol_parts(
        &mut self,
        def: ModuleDef,
    ) -> Option<SymbolParts> {
        let name = match def {
            ModuleDef::Module(m) if m.is_crate_root(self.db) => {
                crate_name(self.db, m.krate(self.db))
            }
            _ => def.name(self.db)?.as_str().to_owned(),
        };
        let visibility = self.visibility(def);
        let documentation = docs::documentation(self, def);
        let deprecation = docs::deprecation(self, def);
        let doc_links = docs::doc_links(self, def, documentation.as_deref());
        let cfg = docs::cfg_expr(self, def);

        let aliases: Vec<String> = self
            .canonical(def)
            .and_then(|key| self.aliases_of(&key))
            .into_iter()
            .flatten()
            .map(|segs| segs.join("::"))
            .collect();

        Some(SymbolParts {
            name,
            visibility,
            documentation: documentation.unwrap_or_default(),
            aliases,
            deprecation,
            doc_links: doc_links.unwrap_or_default(),
            cfg,
        })
    }

    // ── Alias collection (one pass over all module scopes) ────────────────────

    /// Walk every local module's scope and record re-export aliases.
    ///
    /// Ported verbatim from the old `ctx.rs::collect_aliases`.
    pub(crate) fn collect_aliases(&mut self) {
        let mut stack = vec![self.krate.root_module(self.db)];
        while let Some(module) = stack.pop() {
            stack.extend(module.children(self.db));

            let Some(module_segs) = self.path_segments(ModuleDef::Module(module)) else {
                continue;
            };

            for (name, scope_def) in module.scope(self.db, None) {
                let ScopeDef::ModuleDef(def) = scope_def else {
                    continue;
                };
                let Some(canon_key) = self.canonical(def) else {
                    continue;
                };

                let mut alias_segs = module_segs.clone();
                alias_segs.push(name.as_str().to_owned());

                let primary: Vec<String> = canon_key.split("::").map(str::to_owned).collect();
                if alias_segs == primary {
                    continue;
                }
                self.alias_cache
                    .entry(canon_key)
                    .or_default()
                    .insert(alias_segs);
            }
        }
    }

    // ── Impl index ────────────────────────────────────────────────────────────

    /// Rebuild the impl index from `Impl::all_in_crate`.
    ///
    /// Must be called after the full module walk so all ADT paths are cached.
    pub(crate) fn build_impl_index(&mut self) {
        let all: Vec<Impl> = Impl::all_in_crate(self.db, self.krate);
        let mut by_self_ty: FxHashMap<PathKey, Vec<Impl>> = FxHashMap::default();
        let mut blanket: Vec<(Trait, Impl)> = Vec::new();

        for imp in all {
            let self_ty = imp.self_ty(self.db);

            if let Some(adt) = self_ty.as_adt() {
                if let Some(key) = self.canonical(ModuleDef::Adt(adt)) {
                    by_self_ty.entry(key).or_default().push(imp);
                }
                continue;
            }
            if self_ty.as_type_param(self.db).is_some() {
                if let Some(tr) = imp.trait_(self.db) {
                    blanket.push((tr, imp));
                }
            }
        }

        self.impls.by_self_ty = by_self_ty;
        self.impls.blanket = blanket;
    }

    // ── LineIndex ─────────────────────────────────────────────────────────────

    /// Lazy `LineIndex` for `file_id`.
    pub(crate) fn line_index(&mut self, file_id: FileId) -> Option<Arc<LineIndex>> {
        if let Some(li) = self.lines.get(&file_id) {
            return Some(li.clone());
        }
        let text = self.db.file_text(file_id).text(self.db);
        let li = Arc::new(LineIndex::new(text));
        self.lines.insert(file_id, li.clone());
        Some(li)
    }

    // ── Path segments ─────────────────────────────────────────────────────────

    /// Defining-module path segments for `def`, crate name first.
    ///
    /// Ported verbatim from old `ctx.rs::path_segments`.
    pub(crate) fn path_segments(&self, def: ModuleDef) -> Option<Vec<String>> {
        if let ModuleDef::Module(m) = def
            && m.is_crate_root(self.db)
        {
            return Some(vec![crate_name(self.db, m.krate(self.db))]);
        }
        if let ModuleDef::BuiltinType(b) = def {
            return Some(vec![b.name().as_str().to_owned()]);
        }

        let mut segs = Vec::new();
        segs.push(def.name(self.db)?.as_str().to_owned());

        let containing = def.module(self.db)?;
        for m in containing.path_to_root(self.db) {
            if let Some(n) = m.name(self.db) {
                segs.push(n.as_str().to_owned());
            } else if m.is_crate_root(self.db) {
                segs.push(crate_name(self.db, m.krate(self.db)));
            }
        }
        segs.reverse();
        Some(segs)
    }
}

// ── Stable hash (for type_links, deferred to resolution plane) ───────────────

/// Deterministic hash of a canonical path; used for link keys when no Ref is
/// available (e.g. external types that are not lowered in this pass).
pub(crate) fn stable_id(path: &PathKey) -> i64 {
    let mut hasher = FxHasher::default();
    hasher.write(path.as_bytes());
    hasher.finish() as i64
}

/// Rustc / display crate name (`odd-duck` package → `odd_duck`).
pub(crate) fn crate_name(db: &RootDatabase, krate: Crate) -> String {
    krate
        .display_name(db)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "_".into())
}

// ── SymbolParts — assembled metadata for one entry ────────────────────────────

/// Pre-assembled fields for constructing a `nudox_ir::entry::Symbol`.
///
/// The `source` path and `span` are caller-supplied (they come from the AST
/// source node, which the individual item lowerers have access to).
pub(crate) struct SymbolParts {
    pub(crate) name: String,
    pub(crate) visibility: IrVisibility,
    pub(crate) documentation: String,
    /// All alias strings for this symbol.
    pub(crate) aliases: Vec<String>,
    pub(crate) deprecation: Option<Deprecation>,
    /// Resolved intra-doc links.
    pub(crate) doc_links: Vec<DocLink>,
    /// Rendered cfg predicate string, if any.
    pub(crate) cfg: Option<nudox_ir::entry::CfgExpr>,
}

impl SymbolParts {
    /// Assemble into a `nudox_ir::entry::Symbol` given `source` and `span`.
    pub(crate) fn into_symbol(
        self,
        source: std::path::PathBuf,
        span: std::ops::Range<usize>,
    ) -> nudox_ir::entry::Symbol {
        nudox_ir::entry::Symbol {
            name: self.name,
            visibility: self.visibility,
            documentation: self.documentation,
            source,
            span,
            aliases: self.aliases.into_boxed_slice(),
            deprecation: self.deprecation,
            doc_links: self.doc_links.into_boxed_slice(),
            attrs: Box::new([]), // see below
            cfg: self.cfg,
        }
    }
}
