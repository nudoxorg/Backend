//! Builds the `SymbolHead` — the first event on every `DocEvent` stream.
//!
//! # Breadcrumb construction
//!
//! The breadcrumb is the ancestor chain of `intro`, root-first.  We walk
//! `view.parent_of(intro)` upward until we hit `None`, then reverse.
//! Each ancestor becomes a `CrumbRef { key, label }`.
//!
//! # L18: the physical chain can repeat the package name
//!
//! `view.parent_of` walks the *defining-module* (physical) tree, which is
//! not always the tree a reader can name. Real crates routinely have a
//! private submodule that shares a name with the crate itself and re-export
//! its public items at the crate root — `memchr`'s `Memchr` iterator is
//! literally declared inside `mod memchr` (`src/memchr.rs`) and re-exported
//! via `pub use crate::memchr::Memchr` at the crate root. Walking the
//! physical chain naively therefore produces the breadcrumb `memchr › memchr`
//! (crate root, then the private submodule) for a symbol docs.rs shows as
//! plain `memchr::Memchr`.
//!
//! `Symbol::aliases` already carries the producer-resolved re-export path
//! (see `ctx.rs::collect_aliases` in the Rust producer) — it is exactly the
//! canonical, reader-facing path the physical chain is missing.
//! [`shortest_alias_module_path`] prefers the shortest such alias over the
//! physical chain, but **only ever drops physical ancestors** — every
//! resulting crumb is still one of `intro`'s real ancestors with a real,
//! navigable `IntroId`; nothing is fabricated.  When an entry has no alias
//! (the overwhelming majority), the physical chain is used unchanged.
//!
//! # One-walk guarantee for section_plan
//!
//! `section_plan` is obtained by calling `plan::section_plan`, which calls
//! `walk::walk_doc` — the same function that `sections::sections` calls.
//! The two are guaranteed structurally identical (see `walk.rs` docs).

use crate::store::package::PackageView;
use nudox_ir::{
    change::IntroId,
    entry::{CfgExpr, Entry},
    kind::KindDiscriminant,
    view::IrView,
};

use crate::wire::{
    CrumbRef, KindTag, Provenance, SectionPlan, SharedStr, SigToken, SourceLocation, SymbolHead,
    SymbolKey, Visibility,
};

/// Canonical symbol metadata shared by the full GUI head and compact MCP
/// projection. It intentionally contains no documentation-section plan.
pub(crate) struct SymbolProjection {
    pub(crate) key: SymbolKey,
    pub(crate) name: SharedStr,
    pub(crate) breadcrumb: Vec<CrumbRef>,
    pub(crate) signature: Vec<SigToken>,
    pub(crate) kind: KindTag,
    pub(crate) visibility: Visibility,
    pub(crate) provenance: Provenance,
    pub(crate) deprecation: Option<SharedStr>,
    pub(crate) cfg: Option<SharedStr>,
    pub(crate) source: SourceLocation,
    pub(crate) source_excerpt: Option<SharedStr>,
}

impl SymbolProjection {
    fn into_head(self, section_plan: Vec<SectionPlan>) -> SymbolHead {
        SymbolHead {
            key: self.key,
            name: self.name,
            breadcrumb: self.breadcrumb,
            signature: self.signature,
            kind: self.kind,
            visibility: self.visibility,
            provenance: self.provenance,
            deprecation: self.deprecation,
            cfg: self.cfg,
            section_plan,
            source: self.source,
            source_excerpt: self.source_excerpt,
        }
    }
}

// ---------------------------------------------------------------------------
// Source-location helpers
// ---------------------------------------------------------------------------

/// Project an entry's source location onto the wire.
///
/// # What this used to be
///
/// It used to read `Symbol::source` and `Symbol::span` directly and decide
/// absence by testing the path against `""` and `"."`. That test was a *second*
/// implementation of the sentinel decoding (the GUI had a third), it could not
/// distinguish "producer records nothing" from "this entry is synthesized", and
/// it published byte offsets under a name the GUI then had to label `bytes
/// 8880–9435` because they were not navigable.
///
/// The decoding now happens once, in `nudox_ir`, at the point the entry is
/// built — so this function has no policy left in it, which is the point.
fn source_location(entry: &Entry) -> SourceLocation {
    SourceLocation::from_ir(entry.location())
}

// ---------------------------------------------------------------------------
// Cfg rendering
// ---------------------------------------------------------------------------

/// Render `Symbol::cfg` into the exact badge text the GUI shows next to kind
/// and visibility.
///
/// This is the one field that tells a reader "this API might not exist in
/// *your* build" — docs.rs surfaces the same fact as a feature badge on
/// cfg-gated items, and rendering nothing here would be a real regression
/// against that bar. Wrapped in `cfg(...)` so the badge reads as the literal
/// attribute a reader would write to enable the item themselves.
fn cfg_badge(expr: &CfgExpr) -> SharedStr {
    SharedStr::from(format!("cfg({})", render_cfg_predicate(expr)).as_str())
}

/// Render one `CfgExpr` node as Rust `#[cfg(...)]` surface syntax.
///
/// Recurses through `All`/`Any`/`Not` so a compound predicate like
/// `all(feature = "std", not(windows))` reaches the reader whole, rather than
/// being flattened or truncated to its first clause.
fn render_cfg_predicate(expr: &CfgExpr) -> String {
    match expr {
        CfgExpr::All(inner) => format!("all({})", join_predicates(inner)),
        CfgExpr::Any(inner) => format!("any({})", join_predicates(inner)),
        CfgExpr::Not(inner) => format!("not({})", render_cfg_predicate(inner)),
        CfgExpr::Feature(f) => format!("feature = \"{f}\""),
        CfgExpr::TargetOs(os) => format!("target_os = \"{os}\""),
        CfgExpr::TargetArch(arch) => format!("target_arch = \"{arch}\""),
        CfgExpr::Other(raw) => raw.clone(),
    }
}

fn join_predicates(inner: &[CfgExpr]) -> String {
    inner
        .iter()
        .map(render_cfg_predicate)
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Breadcrumb: alias-preferred ancestor projection (L18)
// ---------------------------------------------------------------------------

/// Project `ancestors` (root-first, physical parent chain, excluding the
/// entry itself) down to the shortest re-export alias in `aliases`, if one
/// exists and is actually shorter than the physical chain.
///
/// Each alias is a `"::"`-joined path such as `"memchr::Memchr"`; the final
/// segment names the item at the re-export site (not necessarily an
/// ancestor — it may be a renamed `pub use foo as bar`), so it is dropped
/// before matching. The remaining segments are matched *in order* against
/// `ancestors`' labels to select a subsequence — this is why the result can
/// only ever **omit** physical ancestors, never invent one: every returned
/// `(IntroId, String)` pair is copied verbatim from `ancestors`, so its key
/// still resolves to a real, navigable entry.
///
/// Returns `None` when no alias improves on the physical chain (including
/// the common case of no aliases at all), in which case the caller keeps
/// the physical breadcrumb unchanged.
///
/// # Promoted, not duplicated (§4.9)
///
/// This is the one place the alias-shortening decision is made. `projection`
/// uses it to build the breadcrumb; [`public_path`] uses the exact same
/// decision (via [`shortened_ancestors`]) to render the address scheme's
/// `public_path` string, so the two can never disagree about which alias
/// won.
pub(crate) fn shortest_alias_module_path(
    aliases: &[String],
    ancestors: &[(IntroId, String)],
) -> Option<Vec<(IntroId, String)>> {
    let mut best: Option<Vec<&str>> = None;
    for alias in aliases {
        let mut segs: Vec<&str> = alias.split("::").collect();
        // Drop the trailing segment: it names the item at the re-export
        // site, not an ancestor module.
        segs.pop();
        if segs.is_empty() {
            continue;
        }
        // Only a *strict* improvement over the physical chain is worth
        // taking — otherwise we would replace a correct chain with an
        // equally-long (or longer) one for no reason.
        let is_shorter = best
            .as_ref()
            .map_or(segs.len() < ancestors.len(), |b| segs.len() < b.len());
        if is_shorter {
            best = Some(segs);
        }
    }

    let wanted = best?;

    // Match `wanted`'s segment names against `ancestors`, in order. Each
    // match advances the cursor so segments are consumed once, preserving
    // the physical ordering even when names repeat (e.g. the crate name
    // appearing at both the root and a same-named private submodule).
    let mut projected = Vec::with_capacity(wanted.len());
    let mut cursor = ancestors.iter();
    for seg in &wanted {
        // If the alias names a module that is not actually on the physical
        // path, do not fabricate a crumb for it — abandon the projection
        // and let the caller fall back to the physical chain.
        let found = cursor.by_ref().find(|(_, label)| label == seg)?;
        projected.push(found.clone());
    }
    Some(projected)
}

/// Walk `intro`'s root-first physical ancestor chain (excluding `intro`
/// itself), then apply L18's alias-shortening.
///
/// Returns the ancestors to use for display alongside whether an alias
/// shortcut was actually taken — `public_path` needs the latter to know
/// whether it has anything to say beyond the physical path (see its docs).
pub(crate) fn shortened_ancestors(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
) -> (Vec<(IntroId, String)>, bool) {
    let mut ancestors: Vec<(IntroId, String)> = Vec::new();
    let mut cursor = view.parent_of(intro);
    while let Some(parent_id) = cursor {
        let label = view
            .entry(parent_id)
            .map_or_else(|| "?".to_owned(), |e| e.sym().name.clone());
        ancestors.push((parent_id, label));
        cursor = view.parent_of(parent_id);
    }
    ancestors.reverse(); // root-first

    // L18: prefer the shortest re-export alias over the raw physical chain,
    // when one exists and is actually shorter. See `shortest_alias_module_path`
    // for why this can only drop ancestors, never invent one.
    match shortest_alias_module_path(&entry.sym().aliases, &ancestors) {
        // Only a *module* prefix may be shortened away. `shortest_alias_module_path`
        // matches the alias's segments against ancestor *labels* in order, which
        // on its own is happy to drop a trait, impl or record ancestor — and a
        // path that skips a type level names nothing.
        //
        // Found on real serde 1.0.196: the associated type
        // `serde::de::IntoDeserializer::Deserializer` carries an alias whose
        // module prefix is `serde`, so the projection dropped
        // `de::IntoDeserializer` wholesale and produced the public path
        // `serde::Deserializer` — which is the *trait* `serde::de::Deserializer`,
        // a different declaration entirely. Address resolution then reported
        // `serde::Deserializer` as ambiguous between the two, and a display-only
        // consumer would simply have shown the wrong path.
        Some(projected) if only_modules_dropped(&ancestors, &projected, view) => (projected, true),
        _ => (ancestors, false),
    }
}

/// True when every ancestor `projected` drops relative to `ancestors` is a
/// module.
///
/// Dropping a module is what re-export shortening is *for* — `serde::de::Foo`
/// published as `serde::Foo`. Dropping a trait, impl, record or enum is never
/// valid: those are not path prefixes a caller can omit, so the shortened path
/// would name a different declaration or none at all.
fn only_modules_dropped(
    ancestors: &[(IntroId, String)],
    projected: &[(IntroId, String)],
    view: &IrView,
) -> bool {
    let kept: std::collections::HashSet<IntroId> = projected.iter().map(|(id, _)| *id).collect();
    ancestors
        .iter()
        .filter(|(id, _)| !kept.contains(id))
        .all(|(id, _)| {
            view.entry(*id)
                .and_then(|e| e.kind().discriminant())
                .is_some_and(|d| d == KindDiscriminant::Module)
        })
}

/// The address scheme's `public_path` (§4.9 of docs/MCP-SURFACE-PLAN.md): the
/// shortest path by which `intro` is reachable from the package root through
/// re-exports/aliases, rendered with `style`'s separator.
///
/// `chunk::head::projection`'s alias-shortening (`shortest_alias_module_path`)
/// is *promoted* here to be the `public_path` producer rather than
/// reimplemented — it is exactly the computation this function needs, it was
/// simply never given a name of its own or a leaf segment appended.
///
/// Returns `None` when no alias improves on the physical chain (the common
/// case: most declarations are reachable only via their one physical path,
/// in which case `public_path` carries no information `physical_path`
/// doesn't already have — callers should treat `None` as "same as physical",
/// not as an error).
pub(crate) fn public_path(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    style: nudox_ir::reflect::PathStyle,
) -> Option<String> {
    let (ancestors, took_shortcut) = shortened_ancestors(intro, entry, view);
    if !took_shortcut {
        return None;
    }
    let mut parts: Vec<String> = ancestors.into_iter().map(|(_, label)| label).collect();
    parts.push(entry.sym().name.clone());
    Some(parts.join(style.separator()))
}

/// Build the `SymbolHead` for `entry` at `intro`.
///
/// # Arguments
///
/// * `intro`   — stable identity of the entry (also used as the `key`).
/// * `entry`   — the declaration entry we are building a head for.
/// * `view`    — the `IrView` for this package (used for breadcrumb walk).
/// * `package` — the `PackageView` (used for provenance, signature, and plan).
pub(crate) fn projection(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
) -> SymbolProjection {
    // ── Stable key ────────────────────────────────────────────────────────────

    let key = SymbolKey::new(package.lineage().clone(), intro);

    // ── Breadcrumb ────────────────────────────────────────────────────────────
    //
    // See `shortened_ancestors` for the walk + L18 alias-shortening; shared
    // with `public_path` so the two producers can never disagree about which
    // alias won.
    let (ancestors, _took_alias_shortcut) = shortened_ancestors(intro, entry, view);

    let breadcrumb: Vec<CrumbRef> = ancestors
        .into_iter()
        .map(|(id, label)| CrumbRef {
            key: SymbolKey::new(package.lineage().clone(), id),
            label: SharedStr::from(label.as_str()),
        })
        .collect();

    // ── Signature ─────────────────────────────────────────────────────────────

    let signature = super::signature::tokens(entry, package);

    // ── Kind ──────────────────────────────────────────────────────────────────

    let kind = entry
        .kind()
        .discriminant()
        .map_or(KindTag::Unknown(0), KindTag::Known);

    // ── Visibility ────────────────────────────────────────────────────────────

    let visibility = entry.sym().visibility;

    // ── Provenance ────────────────────────────────────────────────────────────

    let provenance = store_provenance_to_wire(package.provenance());

    // ── Deprecation ───────────────────────────────────────────────────────────
    //
    // `Symbol::deprecation` is `Option<Deprecation>` where `Deprecation` has a
    // `note: Option<String>` field.  We render the note text when present, or
    // fall back to an empty marker when the struct exists but the note is absent.

    let deprecation = entry.sym().deprecation.as_ref().map(|d| {
        d.note.as_deref().map_or_else(
            || {
                d.since.as_deref().map_or_else(
                    || SharedStr::from("Deprecated"),
                    |since| SharedStr::from(format!("Deprecated since {since}").as_str()),
                )
            },
            SharedStr::from,
        )
    });

    // ── Cfg ───────────────────────────────────────────────────────────────────

    let cfg = entry.sym().cfg.as_ref().map(cfg_badge);

    // ── Source location ───────────────────────────────────────────────────────

    let source = source_location(entry);
    let source_excerpt = package.view().source(intro).map(SharedStr::from);

    SymbolProjection {
        key,
        name: SharedStr::from(entry.sym().name.as_str()),
        breadcrumb,
        signature,
        kind,
        visibility,
        provenance,
        deprecation,
        cfg,
        source,
        source_excerpt,
    }
}

/// Build a full-page head from the canonical projection and the plan produced
/// by the page's single documentation walk.
pub fn head_with_plan(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
    section_plan: Vec<SectionPlan>,
) -> SymbolHead {
    projection(intro, entry, view, package).into_head(section_plan)
}

/// Build a standalone full-page head.
///
/// Full document assembly should use [`head_with_plan`] with the plan returned
/// by its existing documentation walk. This wrapper remains for focused head
/// consumers and tests that do not already own that walk.
pub fn head(intro: IntroId, entry: &Entry, view: &IrView, package: &PackageView) -> SymbolHead {
    let section_plan = super::plan::section_plan(intro, entry, view, package);
    head_with_plan(intro, entry, view, package, section_plan)
}

/// Convert `crate::store::package::Provenance` to the wire `Provenance`.
///
/// The store has a smaller provenance vocabulary than the wire (which includes
/// `SyncedLocal`, `Remote`, and `Stale` for distributed corpus scenarios).
/// Both store variants map to `TrustedLocal` — local IR produced in this
/// session is always trusted.  Future store variants are handled by `_ =>
/// TrustedLocal` so that adding a new store tier never silently panics.
fn store_provenance_to_wire(p: crate::store::package::Provenance) -> Provenance {
    // Local IR produced in this session is always trusted. The store currently
    // has only trusted tiers (`TrustedLocal`, `SnapshotLocal`); future tiers
    // fold into `TrustedLocal` here rather than panicking, so there is no
    // per-variant match arm.
    let _ = p;
    Provenance::TrustedLocal
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        package::{PackageView, Provenance as StoreProvenance},
        source::fixtures::build_rich_view,
    };

    #[test]
    fn head_key_matches_intro() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.key.intro,
                intro,
                "head key.intro must equal the intro for '{}'",
                entry.sym().name
            );
            assert_eq!(
                &h.key.package,
                pkg.lineage(),
                "head key.package must equal the lineage"
            );
        }
    }

    #[test]
    fn head_signature_non_empty() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(
                !h.signature.is_empty(),
                "signature must not be empty for '{}'",
                entry.sym().name
            );
        }
    }

    #[test]
    fn breadcrumb_root_has_no_ancestors() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        // The root entry (the one with no parent) should have an empty breadcrumb.
        let root = pkg
            .view()
            .entries()
            .find(|(id, _)| pkg.view().parent_of(*id).is_none());
        if let Some((intro, entry)) = root {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(h.breadcrumb.is_empty(), "root must have empty breadcrumb");
        }
    }

    #[test]
    fn breadcrumb_child_is_non_empty_when_parent_exists() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        // A child entry (one that has a parent) must have a non-empty breadcrumb.
        let child = pkg
            .view()
            .entries()
            .find(|(id, _)| pkg.view().parent_of(*id).is_some());
        if let Some((intro, entry)) = child {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(
                !h.breadcrumb.is_empty(),
                "child must have non-empty breadcrumb for '{}'",
                entry.sym().name
            );
        }
    }

    /// Build a package shaped exactly like the real `memchr` bug (L18): a
    /// crate-root module and a private submodule that share the crate's
    /// name, with the target leaf declared inside the submodule.
    ///
    /// `reexport_alias` mirrors `Symbol::aliases` as the Rust producer would
    /// populate it for a `pub use crate::memchr::Memchr;` re-export at the
    /// crate root — pass `&[]` to get the *unfixed* physical shape (no
    /// alias available) and `&["memchr::Memchr"]` to get the shape after a
    /// producer resolves the re-export.
    ///
    /// Returns `(package, root_id, submodule_id, leaf_id)`.
    fn package_with_private_reexport_submodule(
        reexport_alias: &[&str],
    ) -> (
        crate::store::package::PackageView,
        nudox_ir::change::IntroId,
        nudox_ir::change::IntroId,
        nudox_ir::change::IntroId,
    ) {
        use nudox_ir::{
            apply::PristineIntroTable,
            change::{EcosystemId, IntroId, PackageLineageId, PackageName},
            entry::{Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::Module,
        };

        fn module_sym(name: &str, aliases: &[&str]) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::new(),
                span: 0..0,
                aliases: aliases
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let root_id = IntroId::from_raw([11u8; 32]);
        let submod_id = IntroId::from_raw([12u8; 32]);
        let leaf_id = IntroId::from_raw([13u8; 32]);

        let mut table = PristineIntroTable::new();
        table.insert_live(
            root_id,
            Entry::new(
                module_sym("memchr", &[]),
                Node::build(None::<RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            submod_id,
            Entry::new(
                module_sym("memchr", &[]),
                Node::build(None::<RawRef>, []),
                Kind::Module(Module),
            ),
            Some(root_id),
        );
        table.insert_live(
            leaf_id,
            Entry::new(
                module_sym("Memchr", reexport_alias),
                Node::build(None::<RawRef>, []),
                Kind::Module(Module),
            ),
            Some(submod_id),
        );

        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("memchr"));
        let view = IrView::with_package(lineage, table);
        (
            PackageView::build(view, StoreProvenance::TrustedLocal),
            root_id,
            submod_id,
            leaf_id,
        )
    }

    /// Alias shortening may drop a **module** prefix and nothing else.
    ///
    /// Regression guard for a real defect found on serde 1.0.196. The
    /// associated type `serde::de::IntoDeserializer::Deserializer` carries an
    /// alias whose module prefix is `serde`, and `shortest_alias_module_path`
    /// matches alias segments against ancestor *labels* in order — with no
    /// check on what it is dropping. It therefore discarded the trait
    /// `IntoDeserializer` along with the module `de` and produced the public
    /// path `serde::Deserializer`, which is the *trait* `serde::de::Deserializer`
    /// — a different declaration.
    ///
    /// The visible symptom was address resolution reporting `serde::Deserializer`
    /// as ambiguous between an `alias` and a `trait`; the quieter symptom was
    /// `get_symbol` displaying a path that names nothing.
    ///
    /// A trait ancestor is not a path prefix a caller may omit, so the
    /// projection must be abandoned and the physical chain kept.
    #[test]
    fn alias_shortening_never_drops_a_non_module_ancestor() {
        use nudox_ir::{
            apply::PristineIntroTable,
            change::{EcosystemId, IntroId, PackageLineageId, PackageName},
            entry::{Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::{Module, Trait},
        };

        fn sym(name: &str, aliases: &[&str]) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::new(),
                span: 0..0,
                aliases: aliases
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        // serde (module) -> de (module) -> IntoDeserializer (TRAIT) -> Deserializer
        let root = IntroId::from_raw([21u8; 32]);
        let de = IntroId::from_raw([22u8; 32]);
        let tr = IntroId::from_raw([23u8; 32]);
        let leaf = IntroId::from_raw([24u8; 32]);

        let mut table = PristineIntroTable::new();
        for (id, name, parent, kind) in [
            (root, "serde", None, Kind::Module(Module)),
            (de, "de", Some(root), Kind::Module(Module)),
            (
                tr,
                "IntoDeserializer",
                Some(de),
                Kind::Trait(Trait::builder().build()),
            ),
        ] {
            table.insert_live(
                id,
                Entry::new(sym(name, &[]), Node::build(None::<RawRef>, []), kind),
                parent,
            );
        }
        table.insert_live(
            leaf,
            Entry::new(
                // The alias that triggered the real bug.
                sym("Deserializer", &["serde::Deserializer"]),
                Node::build(None::<RawRef>, []),
                Kind::Module(Module),
            ),
            Some(tr),
        );

        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("serde"));
        let view = IrView::with_package(lineage, table);
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);
        let entry = pkg.view().entry(leaf).expect("leaf entry must exist");

        let (ancestors, took_shortcut) = shortened_ancestors(leaf, entry, pkg.view());
        let labels: Vec<String> = ancestors.into_iter().map(|(_, l)| l).collect();

        assert!(
            !took_shortcut,
            "the projection drops the trait `IntoDeserializer`, so it must be \
             abandoned; got shortened ancestors {labels:?}"
        );
        assert_eq!(
            labels,
            vec![
                "serde".to_string(),
                "de".to_string(),
                "IntoDeserializer".to_string()
            ],
            "the physical chain must survive intact"
        );
        assert_eq!(
            public_path(
                leaf,
                entry,
                pkg.view(),
                nudox_ir::reflect::PathStyle::DoubleColon
            ),
            None,
            "no alias improves on the physical chain here, so public_path must \
             be None rather than a path that names a different declaration"
        );
    }

    /// L18: a private submodule that shares its name with the crate itself
    /// must not leave the breadcrumb repeating that name — the shortest
    /// re-export alias must win over the raw physical chain.
    ///
    /// This is the synthetic, deterministic counterpart to
    /// `real_memchr_memchr_breadcrumb_has_no_repeated_segment` below: it
    /// mirrors the real bug exactly (`memchr::Memchr` is declared inside the
    /// private module `memchr::memchr`, source file `memchr.rs`, and
    /// re-exported at the crate root), so it does not depend on a real
    /// crate checkout being present to catch a regression.
    #[test]
    fn breadcrumb_prefers_shorter_alias_over_repeated_private_submodule() {
        let (pkg, root_id, _submod_id, leaf_id) =
            package_with_private_reexport_submodule(&["memchr::Memchr"]);
        let entry = pkg.view().entry(leaf_id).expect("leaf entry must exist");

        let h = head(leaf_id, entry, pkg.view(), &pkg);

        let labels: Vec<String> = h.breadcrumb.iter().map(|c| c.label.to_string()).collect();
        assert_eq!(
            labels,
            vec!["memchr".to_string()],
            "alias-projected breadcrumb must collapse the repeated private \
             submodule, got {labels:?}"
        );
        // The surviving crumb must still point at a real ancestor (the
        // crate root), never a fabricated id.
        assert_eq!(
            h.breadcrumb[0].key.intro, root_id,
            "surviving crumb must be the real crate-root IntroId"
        );
    }

    /// Without an alias, the physical chain is all we have — the fix must
    /// not touch it. This is the regression guard for the common case
    /// (the overwhelming majority of entries carry no `aliases` at all):
    /// `shortest_alias_module_path` must return `None` and leave the
    /// physical ancestors exactly as `view.parent_of` produced them.
    #[test]
    fn breadcrumb_keeps_physical_chain_when_entry_has_no_alias() {
        let (pkg, _root_id, submod_id, leaf_id) = package_with_private_reexport_submodule(&[]);
        let entry = pkg.view().entry(leaf_id).expect("leaf entry must exist");

        let h = head(leaf_id, entry, pkg.view(), &pkg);

        let labels: Vec<String> = h.breadcrumb.iter().map(|c| c.label.to_string()).collect();
        assert_eq!(
            labels,
            vec!["memchr".to_string(), "memchr".to_string()],
            "with no alias, the physical two-level chain must be preserved \
             unchanged, got {labels:?}"
        );
        assert_eq!(h.breadcrumb[1].key.intro, submod_id);
    }

    /// End-to-end regression against the **real** `memchr` crate (L18):
    /// drives the actual Rust producer over `result/memchr-2.8.3`, the
    /// exact case the limitations ledger used to demonstrate the bug, rather
    /// than a fixture tailored to pass.
    ///
    /// `struct Memchr<'h>` is declared in the private module `memchr::memchr`
    /// (`src/memchr.rs`) and re-exported at the crate root via
    /// `pub use crate::memchr::{..., Memchr, ...};` (`src/lib.rs`). Before
    /// the fix, `head()`'s physical-chain walk produced the breadcrumb
    /// `["memchr", "memchr"]` — the crate name twice — which, combined with
    /// the page title "Memchr", is exactly the L18 evidence's
    /// `memchr › memchr › memchr`.
    ///
    /// # Running
    ///
    /// ```text
    /// cargo test -p nudox-engine --lib \
    ///   chunk::head::tests::real_memchr_memchr_breadcrumb_has_no_repeated_segment \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
    fn real_memchr_memchr_breadcrumb_has_no_repeated_segment() {
        use crate::store::source::producer::PackageDescriptor;
        use nudox_languages::produce;
        use nudox_languages::rust::RustProducer;

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../result/memchr-2.8.3")
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from("/nonexistent"));

        if !root.join("Cargo.toml").is_file() {
            eprintln!(
                "SKIP: no memchr checkout at {}. \
                 Run: scripts/fetch-real-crate.sh memchr 2.8.3",
                root.display()
            );
            return;
        }

        // `direct_repo: false` matches `ProducerRegistry::with_rust_pilot`,
        // the constructor the real app uses — the same settings that
        // produced the L18 screenshot evidence.
        let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
        let table = produce(
            &RustProducer { direct_repo: false },
            &descriptor.source,
            &descriptor.lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .expect("memchr must lower without error for a real checkout")
        .table;

        let view = IrView::with_package(descriptor.lineage, table);
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        let memchr_struct = pkg
            .view()
            .entries()
            .find(|(_, e)| e.sym().name == "Memchr")
            .map(|(id, _)| id)
            .expect("real memchr must declare a `Memchr` struct");
        let entry = pkg.view().entry(memchr_struct).expect("entry must exist");

        let h = head(memchr_struct, entry, pkg.view(), &pkg);
        let labels: Vec<String> = h.breadcrumb.iter().map(|c| c.label.to_string()).collect();
        eprintln!(
            "Memchr breadcrumb: {labels:?} (aliases: {:?})",
            entry.sym().aliases
        );

        let mut seen = std::collections::HashSet::new();
        for label in &labels {
            assert!(
                seen.insert(label.clone()),
                "breadcrumb must not repeat a segment — this is exactly the \
                 L18 bug (`memchr › memchr`): {labels:?}"
            );
        }
    }

    #[test]
    fn section_plan_matches_sections() {
        use super::super::sections::sections;

        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            let sects = sections(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.section_plan.len(),
                sects.len(),
                "head.section_plan must match sections for '{}'",
                entry.sym().name
            );
            for (p, s) in h.section_plan.iter().zip(sects.iter()) {
                assert_eq!(
                    p.id,
                    s.section_id(),
                    "plan/section id mismatch for '{}'",
                    entry.sym().name
                );
            }
        }
    }

    /// Helper that builds a minimal `Entry` — just enough to exercise
    /// `source_location`.  We use a module kind because `Module` is a unit
    /// struct and requires no extra data.
    fn minimal_entry(source: &str, span: std::ops::Range<usize>) -> nudox_ir::entry::Entry {
        use nudox_ir::{
            entry::{Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::Module,
        };
        let sym = Symbol {
            name: "test_sym".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from(source),
            span,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        nudox_ir::entry::Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
    }

    /// A single-entry package whose root symbol carries `cfg`, for exercising
    /// `head()`'s cfg wiring without a real producer.
    fn package_with_cfg(cfg: Option<CfgExpr>) -> crate::store::package::PackageView {
        use nudox_ir::{
            apply::PristineIntroTable,
            change::{EcosystemId, IntroId, PackageLineageId, PackageName},
            entry::{Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::Module,
        };

        let sym = Symbol {
            name: "gated".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg,
        };
        let entry =
            nudox_ir::entry::Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module));

        let root_id = IntroId::from_raw([7u8; 32]);
        let mut table = PristineIntroTable::new();
        table.insert_live(root_id, entry, None);

        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("cfg-pkg"));
        let view = IrView::with_package(lineage, table);
        PackageView::build(view, StoreProvenance::TrustedLocal)
    }

    /// A symbol carrying a `cfg` must reach `SymbolHead.cfg` populated, and
    /// rendered as Rust `#[cfg(...)]` surface syntax — this is the entire
    /// point of the field (LD-8-adjacent: "can I use this as I built my
    /// crate?"). A recursive predicate is used so this cannot pass by
    /// accidentally stringifying only the outermost variant.
    #[test]
    fn head_cfg_carries_a_populated_predicate() {
        let pkg = package_with_cfg(Some(CfgExpr::All(Box::new([
            CfgExpr::Feature("std".to_owned()),
            CfgExpr::Not(Box::new(CfgExpr::TargetOs("windows".to_owned()))),
        ]))));
        let root_id = IntroId::from_raw([7u8; 32]);
        let entry = pkg.view().entry(root_id).expect("root entry must exist");

        let h = head(root_id, entry, pkg.view(), &pkg);

        assert_eq!(
            h.cfg.as_deref(),
            Some("cfg(all(feature = \"std\", not(target_os = \"windows\")))"),
            "head.cfg must carry the full rendered predicate, not just a marker"
        );
    }

    /// A symbol with no `cfg` must reach `SymbolHead.cfg` as `None` — this is
    /// the round-trip counterpart to the test above: the field must not
    /// fabricate a badge for unconditional items.
    #[test]
    fn head_cfg_is_none_when_symbol_has_none() {
        let pkg = package_with_cfg(None);
        let root_id = IntroId::from_raw([7u8; 32]);
        let entry = pkg.view().entry(root_id).expect("root entry must exist");

        let h = head(root_id, entry, pkg.view(), &pkg);

        assert!(
            h.cfg.is_none(),
            "an unconditional symbol must not carry a fabricated cfg badge"
        );
    }

    /// An entry built from the legacy `(source, span)` pair carries a file and
    /// a byte range and **no line information** — so it reaches the wire as
    /// `BytesOnly`, which the GUI must not render as a link. This is the
    /// invariant that keeps `Declared` meaningful: it is unreachable from a
    /// producer that has not computed lines.
    #[test]
    fn a_producer_that_records_only_bytes_cannot_produce_a_jump_target() {
        let entry = minimal_entry("src/lib.rs", 10..42);
        let loc = source_location(&entry);
        assert_eq!(
            loc,
            SourceLocation::BytesOnly {
                file: SharedStr::from("src/lib.rs"),
                bytes: [10, 42],
            }
        );
        assert_eq!(
            loc.jump_target(),
            None,
            "byte offsets must never be formatted as a navigable location"
        );
    }

    /// The `0..0` sentinel must arrive at the GUI as a *named* absence, not as
    /// "present at byte 0" and not as an untyped `None` the reader cannot act
    /// on. Naming the reason is what turns a blank Source panel into a filed
    /// bug against a specific producer.
    #[test]
    fn the_zero_span_sentinel_reaches_the_wire_as_a_named_reason() {
        let entry = minimal_entry("", 0..0);
        assert_eq!(
            source_location(&entry),
            SourceLocation::Unlocated {
                reason: crate::wire::UnlocatedReason::ProducerRecordsNoLocation,
            }
        );
    }

    /// The rich fixture uses `PathBuf::new()` for every symbol, so every head
    /// must report the producer-gap reason — confirming we neither fabricate a
    /// path nor quietly downgrade the gap to a generic absence.
    #[test]
    fn head_source_names_the_producer_gap_when_the_fixture_records_no_paths() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.source,
                SourceLocation::Unlocated {
                    reason: crate::wire::UnlocatedReason::ProducerRecordsNoLocation,
                },
                "fixture records no paths — head for '{}' must say so by name",
                entry.sym().name
            );
            assert_eq!(h.source.jump_target(), None);
        }
    }

    #[test]
    fn provenance_is_trusted_local_for_fixture() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.provenance,
                crate::wire::Provenance::TrustedLocal,
                "fixture provenance must be TrustedLocal"
            );
        }
    }

    #[test]
    fn kind_tag_known_for_all_fixture_entries() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(
                matches!(h.kind, KindTag::Known(_)),
                "all fixture entries should have Known kind, got {:?} for '{}'",
                h.kind,
                entry.sym().name
            );
        }
    }
}
