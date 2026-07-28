//! Lowering context: canonical path → RaId map, impl index, alias cache,
//! and visibility/doc helpers.
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
//! ## ID scheme
//!
//! Items are identified by a hierarchical path string.  The key rules are:
//!
//! 1. **Module-level items** (items whose parent is a module):
//!    `{module_id}::{name}` for type-namespace items;
//!    `{module_id}::{name}!v` for value-namespace items (fn / const / static)
//!    that would otherwise collide with a same-named type-namespace item (e.g.
//!    `mod serve` vs `fn serve` in axum);
//!    `{module_id}::{name}!m` for macro-namespace items.
//!    The `!v` / `!m` suffixes use `!` as the delimiter because `!` cannot
//!    appear in a Rust identifier or path, making false collisions impossible.
//!    The type namespace is left bare so existing ids for types, modules, traits,
//!    and type aliases are unchanged.
//!
//! 2. **Nested items** (members of `impl` / `trait` blocks, fields, variants,
//!    parameters): `{parent_id}::{name}`.  The parent `impl_id` or `trait_id`
//!    already encodes the self type and trait, so two different impls providing
//!    the same method name yield different ids without any counter.  This is
//!    the primary fix for the axum `fmt` / `Future` / `Rejection` collisions.
//!
//! **Why no positional counter?**  A counter whose value depends on visit order
//! (or worse, hash-map iteration order) produces different ids on different runs,
//! which destroys the IR-VCS version history.  The scheme above is purely
//! structural and deterministic.

use ra_ap_hir::{
    Crate, DisplayTarget, HasVisibility, Module, ModuleDef, ScopeDef, Semantics,
    Visibility,
};
use ra_ap_ide_db::RootDatabase;
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

use nudox_ir::{
    entry::{Deprecation, DocLink, Visibility as IrVisibility},
    vocab::{ReferenceKind, RelSpan},
};

use super::docs;
use crate::RaId;

/// A canonical path key, e.g. `"my_crate::Foo::bar"`.
pub(crate) type PathKey = SmolStr;

// ── Pending occurrence — pre-seal occurrence fact ─────────────────────────────

/// The endpoint of a pending occurrence.
///
/// After `seal()`, a `Local` endpoint resolves to an `IntroId` via the
/// canonical-path → `IntroId` reverse map.  A `Foreign` endpoint already
/// carries a stable external path that can be used to look up the target in
/// the foreign package.
#[derive(Debug, Clone)]
pub(crate) enum PendingTarget {
    /// Same-package entry; key is the `RaId` (canonical path string).
    Local(RaId),
    /// Cross-package entry; key is `"crate_name::path::to::Item"`.
    Foreign(String),
}

/// A single pre-seal occurrence fact collected during the IR walk.
///
/// The owner is a local `RaId`.  The target is a `PendingTarget` (local or
/// foreign).  Both are resolved to `IntroId` / `StableRef` post-seal.
#[derive(Debug, Clone)]
pub(crate) struct PendingOcc {
    /// The entry that contains this reference (the function, method, …).
    pub(crate) owner: RaId,
    /// The referenced symbol.
    pub(crate) target: PendingTarget,
    /// The category of the reference.
    pub(crate) kind: ReferenceKind,
    /// Source-text span relative to the owner's span start, if available.
    pub(crate) span: Option<RelSpan>,
}

/// Per-crate lowering state.
pub(crate) struct LowerCtx<'db> {
    pub(crate) db: &'db RootDatabase,
    pub(crate) sema: Semantics<'db, RootDatabase>,
    pub(crate) krate: Crate,
    pub(crate) display: DisplayTarget,
    pub(crate) document_private: bool,

    /// Canonical path string → `RaId` (same string; kept for cache hit test).
    pub(crate) path_cache: FxHashMap<PathKey, RaId>,

    /// All public paths per def (re-export aliases, incl. globs).
    pub(crate) alias_cache: FxHashMap<PathKey, FxHashSet<Vec<String>>>,

    /// Pre-seal occurrence facts collected during the crate walk.
    ///
    /// Populated by `item::record_body_occurrences` while lowering function
    /// bodies.  Resolved to `Occurrence` / `Relation` values post-seal by the
    /// caller (see `ra::mod::lower_workspace_with_occurrences`).
    pub(crate) occurrence_buf: Vec<PendingOcc>,

    /// Debug-assertion set: every `RaId` emitted via `check_unique` is
    /// recorded here.  This is a *check* only — it is never used as input to
    /// id construction (that would introduce order-dependency).  The set is
    /// compiled away in release builds.
    #[cfg(debug_assertions)]
    emitted: FxHashSet<RaId>,
}

impl<'db> LowerCtx<'db> {
    pub(crate) fn new(db: &'db RootDatabase, krate: Crate, document_private: bool) -> Self {
        let display = krate.to_display_target(db);
        LowerCtx {
            sema: Semantics::new(db),
            db,
            krate,
            display,
            document_private,
            path_cache: FxHashMap::default(),
            alias_cache: FxHashMap::default(),
            occurrence_buf: Vec::new(),
            #[cfg(debug_assertions)]
            emitted: FxHashSet::default(),
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

    /// Build a `RaId` for an item that is a child of `parent`.
    ///
    /// This is the primary id-construction method for all **nested** items:
    /// impl members, trait members, fields, variants, and parameters.  It is
    /// also safe to use for module-level type-namespace items (modules, structs,
    /// enums, traits, type aliases), since `{module_id}::{name}` reproduces
    /// the canonical path exactly.
    ///
    /// **Uniqueness guarantee** — Rust forbids two items with the same name in
    /// the same impl/trait/struct/enum, so `{parent}::{name}` is unique within
    /// its parent scope.  Across scopes, uniqueness is guaranteed because
    /// `parent` itself is unique (each impl block has a distinct string derived
    /// from its self type and trait).
    ///
    /// **No counter** — order-independent by construction.
    pub(crate) fn child_id(&self, parent: &RaId, name: &str) -> RaId {
        RaId::from(format!("{parent}::{name}").as_str())
    }

    /// Debug-assert that `id` has not been emitted before in this session.
    ///
    /// Call this in debug builds immediately before every `Lowering::declare`
    /// invocation so that future regressions surface as immediate panics rather
    /// than cryptic `LoweringError::Duplicate` errors 3000 entries later.
    ///
    /// The set is never consulted for id *construction* — it is a check only.
    #[inline]
    pub(crate) fn check_unique(&mut self, id: &RaId) {
        #[cfg(debug_assertions)]
        {
            assert!(
                self.emitted.insert(id.clone()),
                "duplicate RaId emitted: {id}"
            );
        }
        let _ = id; // suppress unused warning in release
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

        let attrs = docs::symbol_attrs(self, def);

        Some(SymbolParts {
            name,
            visibility,
            documentation: documentation.unwrap_or_default(),
            aliases,
            deprecation,
            doc_links: doc_links.unwrap_or_default(),
            cfg,
            attrs,
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
    /// Normalized attribute tokens (§6.2 key 8 / §8.5).
    pub(crate) attrs: Box<[nudox_ir::entry::AttrTok]>,
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
            attrs: self.attrs,
            cfg: self.cfg,
        }
    }
}
