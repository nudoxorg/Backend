//! P4 — `ApiSurface` projection + three hash classes.
//!
//! [`surface`] is a pure function over a [`PayloadTable`] + [`ExportPolicy`]:
//! it performs the §8.4 reachability BFS to determine which entries are exported,
//! collects their moniker paths (canonical + reexport aliases), and computes the
//! three per-item hash classes (§3.3).
//!
//! # Hash classes (§3.3, §6.2)
//!
//! | Hash | Domain | Purpose |
//! |---|---|---|
//! | `payload_hash` | `nudox.entry.v2` | Already on `OwnedEntryPayload`; exact identity |
//! | `api_surface_hash` | `nudox.apisurface.v1` | S-marked F1 fields, moniker-class excluded; gates semver/graph |
//! | `embed_hash` | `nudox.embed.v1` | name + moniker + docs + rendered sig; gates re-embedding |
//!
//! Acceptance test V-1 (§16.2): a doc-only edit moves `embed_hash` + `payload_hash`
//! but NOT `api_surface_hash`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use ir::change::{ContentBlake3, IntroId, StableRef};
use crate::wire::PayloadTable;
use ir::entry::Visibility;
use crate::wire::{
    AttrTok, AutoFact, AutoState, AutoTrait, CfgExpr, FnSigFlags, GenericParamWire, KindWire,
    OwnedEntryPayload, PrimitiveWire, RecordForm, SelfKind, Sealed, TraitFlags,
    TypeRefWire, TypeWire, VariantForm, WherePredWire, WidthWire,
};

use crate::semver::config::ConfigId;

// ---------------------------------------------------------------------------
// ExportPolicy (§8.4)
// ---------------------------------------------------------------------------

/// Policy controlling which entries are considered part of the exported API surface.
///
/// Default: `include_doc_hidden = false`, `visibility_floor = Public`,
/// `reexport_depth_limit = 32`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportPolicy {
    /// If `true`, entries marked `doc(hidden)` are included in the surface
    /// (used for the sealed-trait detection algorithm, §8.9).
    pub include_doc_hidden: bool,
    /// The minimum visibility an entry must have to be considered exported.
    pub visibility_floor: Visibility,
    /// Maximum depth through re-export chains before treating the target as a
    /// boundary (foreign) item. Guards against cycles.
    pub reexport_depth_limit: u8,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            include_doc_hidden: false,
            visibility_floor: Visibility::Public,
            reexport_depth_limit: 32,
        }
    }
}

// ---------------------------------------------------------------------------
// MonikerPath (§8.4)
// ---------------------------------------------------------------------------

/// The canonical import path of an exported item: root-to-leaf segment list.
///
/// E.g. `["my_crate", "io", "Error"]`.
/// Used as a surface-level key (presence lints join on this).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct MonikerPath(pub Vec<SmolStr>);

impl MonikerPath {
    pub fn new(segments: Vec<SmolStr>) -> Self {
        Self(segments)
    }

    /// Push one segment, returning a new `MonikerPath`. Accepts anything
    /// string-like (`&str`, `String`, `&SmolStr`) for ergonomic call sites.
    pub fn push(&self, segment: impl AsRef<str>) -> Self {
        let mut v = self.0.clone();
        v.push(SmolStr::new(segment.as_ref()));
        Self(v)
    }

    pub fn segments(&self) -> &[SmolStr] {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// ApiItem (§8.4)
// ---------------------------------------------------------------------------

/// The surface-projected view of one exported entry.
///
/// Contains the kind body, attrs, visibility, cfg, and the three hash classes.
/// Moniker paths are stored separately in `ApiSurface::monikers`.
#[derive(Clone, Debug)]
pub struct ApiItem {
    /// The entry's parent intro in the declaration tree (`None` for roots).
    /// Retained so evolution lints can join a trait/record to its child items
    /// (trait methods, fields) without re-reading the table (§8.2, A-8/9/10).
    pub parent: Option<IntroId>,
    /// Visibility at the point of the entry.
    pub visibility: Visibility,
    /// Normalized attribute tokens (non_exhaustive, must_use, doc_hidden, repr, …).
    pub attrs: Vec<AttrTok>,
    /// Cfg predicate guarding this entry.
    pub cfg: Option<CfgExpr>,
    /// Whether the entry is deprecated.
    pub deprecated: bool,
    /// Kind body reference (cloned from the table).
    pub kind: KindWire,
    /// The `api_surface_hash` for this item (§3.3 S-marked fields, moniker-class excluded).
    pub api_surface_hash: ContentBlake3,
    /// The `embed_hash` for this item (name + moniker + doc + rendered sig).
    pub embed_hash: ContentBlake3,
    /// The `payload_hash` from `OwnedEntryPayload` (exact identity).
    pub payload_hash: ContentBlake3,
}

// ---------------------------------------------------------------------------
// ApiSurface (§8.4)
// ---------------------------------------------------------------------------

/// The projected API surface of a package at one generation + config.
///
/// This is a pure value derived from `surface(table, policy)`. It is not
/// serialized into the store; the store holds the `PayloadTable` and this
/// is recomputed on demand (or cached by `api_surface_hash`).
#[derive(Clone, Debug)]
pub struct ApiSurface {
    /// The config under which this surface was projected.
    pub config: ConfigId,
    /// Exported items keyed by durable `IntroId`.
    pub items: BTreeMap<IntroId, ApiItem>,
    /// Import-path → item mapping (canonical + re-export alias paths).
    pub monikers: BTreeMap<MonikerPath, IntroId>,
    /// Foreign re-export targets encountered during BFS (Pack C food).
    pub boundary: BTreeMap<MonikerPath, StableRef>,
}

// ---------------------------------------------------------------------------
// surface() — the §8.4 reachability BFS
// ---------------------------------------------------------------------------

/// Project the exported `ApiSurface` from a materialized `PayloadTable`.
///
/// This is the §8.4 pure function: `surface(table, policy) -> ApiSurface`.
///
/// # Algorithm
///
/// 1. **Roots** = entries whose parent is `None` (module-level declarations in
///    the root module, or the root module itself).
/// 2. **BFS** over child and re-export edges:
///    - A child entry is exported iff its parent is exported, its visibility ≥
///      `policy.visibility_floor`, and it is not `doc(hidden)` (unless
///      `policy.include_doc_hidden`).
///    - A re-export entry (kind 12) that is itself exported points to a target.
///      If the target is in the same package (by `IntroId` lookup), the target
///      is also exported (with an additional moniker path). If the target is
///      foreign (different package), it is recorded in `boundary`.
/// 3. For each exported entry, collect all moniker paths (canonical + aliases
///    via re-export paths).
/// 4. Compute per-item hash classes.
pub fn surface(table: &PayloadTable, policy: &ExportPolicy) -> ApiSurface {
    surface_with_config(table, policy, ConfigId::DEFAULT)
}

/// Same as [`surface`] but attaches a specific `ConfigId` to the result.
pub fn surface_with_config(
    table: &PayloadTable,
    policy: &ExportPolicy,
    config: ConfigId,
) -> ApiSurface {
    // ── Phase 1: collect root entries ────────────────────────────────────────
    // Roots = entries with parent == None in the table.
    let mut roots: Vec<IntroId> = table
        .live_entries()
        .filter(|(id, _)| table.parent_of(*id).is_none())
        .map(|(id, _)| id)
        .collect();
    roots.sort_unstable(); // deterministic BFS order

    // ── Phase 2: BFS ─────────────────────────────────────────────────────────
    // exported: IntroId → canonical moniker path
    let mut exported: BTreeMap<IntroId, MonikerPath> = BTreeMap::new();
    // extra_monikers: additional (re-export) paths for an already-exported item
    let mut extra_monikers: BTreeMap<IntroId, Vec<MonikerPath>> = BTreeMap::new();
    // boundary: MonikerPath → StableRef (foreign targets)
    let mut boundary: BTreeMap<MonikerPath, StableRef> = BTreeMap::new();

    // Work queue: (IntroId, moniker_path_so_far, reexport_depth)
    let mut queue: VecDeque<(IntroId, MonikerPath, u8)> = VecDeque::new();

    for root_id in &roots {
        if let Some(payload) = table.get(*root_id) {
            // Root items are exported iff they pass the visibility + doc_hidden gate.
            if is_exported(payload, policy) {
                let name = SmolStr::new(&payload.symbol.name);
                let path = MonikerPath::new(vec![name]);
                exported.insert(*root_id, path.clone());
                queue.push_back((*root_id, path, 0));
            }
        }
    }

    // BFS: expand children of every exported entry.
    // We also need to expand re-export targets.
    // Build a child index: parent → Vec<child>
    let child_index = build_child_index(table);

    // Visited set to prevent cycles in re-export chains.
    let mut visited_reexports: BTreeSet<IntroId> = BTreeSet::new();

    while let Some((parent_id, parent_path, depth)) = queue.pop_front() {
        let parent_payload = match table.get(parent_id) {
            Some(p) => p,
            None => continue,
        };

        // ── (a) child edges ───────────────────────────────────────────────────
        if let Some(children) = child_index.get(&parent_id) {
            let mut sorted_children = children.clone();
            sorted_children.sort_unstable();

            for child_id in sorted_children {
                if exported.contains_key(&child_id) {
                    // Already exported via another path (e.g. a reexport already
                    // registered it); add extra moniker.
                    let child_payload = match table.get(child_id) {
                        Some(p) => p,
                        None => continue,
                    };
                    let child_path = parent_path.push(&child_payload.symbol.name);
                    extra_monikers.entry(child_id).or_default().push(child_path);
                    continue;
                }

                let child_payload = match table.get(child_id) {
                    Some(p) => p,
                    None => continue,
                };

                if !is_exported(child_payload, policy) {
                    continue;
                }

                let child_path = parent_path.push(&child_payload.symbol.name);
                exported.insert(child_id, child_path.clone());
                queue.push_back((child_id, child_path, depth));
            }
        }

        // ── (b) re-export edges (kind 12) ─────────────────────────────────────
        if let KindWire::Reexport(ref rx) = parent_payload.kind {
            if depth >= policy.reexport_depth_limit {
                // Depth-limit reached: treat as boundary.
                boundary.insert(parent_path.clone(), rx.target.clone());
                continue;
            }

            if visited_reexports.contains(&parent_id) {
                continue;
            }
            visited_reexports.insert(parent_id);

            // Resolve the target.
            let target_intro = rx.target.intro;
            // Same-package check: if the intro exists in this table, it is same-package.
            if table.is_live(target_intro) {
                // Check if already exported via another route.
                if let std::collections::btree_map::Entry::Vacant(e) = exported.entry(target_intro) {
                    // Export the target via this reexport path.
                    if let Some(tgt_payload) = table.get(target_intro)
                        && is_exported(tgt_payload, policy) {
                            let child_path = parent_path.push(&tgt_payload.symbol.name);
                            e.insert(child_path.clone());
                            queue.push_back((target_intro, child_path, depth + 1));
                        }
                } else {
                    // Add the reexport path as an extra moniker.
                    if let Some(tgt_payload) = table.get(target_intro) {
                        let child_path = parent_path.push(&tgt_payload.symbol.name);
                        extra_monikers.entry(target_intro).or_default().push(child_path);
                    }
                }
            } else {
                // Foreign target → boundary.
                boundary.insert(parent_path.clone(), rx.target.clone());
            }
        }
    }

    // ── Phase 3: build monikers map and compute hash classes ─────────────────
    let mut items: BTreeMap<IntroId, ApiItem> = BTreeMap::new();
    let mut monikers: BTreeMap<MonikerPath, IntroId> = BTreeMap::new();

    for (intro_id, canonical_path) in &exported {
        let payload = match table.get(*intro_id) {
            Some(p) => p,
            None => continue,
        };

        // Gather all moniker paths for this item.
        let mut all_paths: Vec<MonikerPath> = vec![canonical_path.clone()];
        if let Some(extras) = extra_monikers.get(intro_id) {
            all_paths.extend(extras.iter().cloned());
        }

        // Compute hash classes.
        let api_surface_hash = compute_api_surface_hash(payload, *intro_id);
        let embed_hash = compute_embed_hash(payload, canonical_path, &all_paths);
        let payload_hash = payload.payload_hash;

        let item = ApiItem {
            parent: table.parent_of(*intro_id),
            visibility: payload.symbol.visibility,
            attrs: payload.symbol.attrs.clone(),
            cfg: payload.symbol.cfg.clone(),
            deprecated: payload.symbol.deprecation.is_some(),
            kind: payload.kind.clone(),
            api_surface_hash,
            embed_hash,
            payload_hash,
        };

        items.insert(*intro_id, item);

        // Register all moniker paths.
        for path in all_paths {
            monikers.insert(path, *intro_id);
        }
    }

    ApiSurface { config, items, monikers, boundary }
}

// ---------------------------------------------------------------------------
// Reachability gate helpers
// ---------------------------------------------------------------------------

/// True iff the entry passes the export policy gate:
/// - visibility ≥ floor
/// - not `doc(hidden)` (unless `include_doc_hidden`)
fn is_exported(payload: &OwnedEntryPayload, policy: &ExportPolicy) -> bool {
    if vis_rank(payload.symbol.visibility) < vis_rank(policy.visibility_floor) {
        return false;
    }
    if !policy.include_doc_hidden && is_doc_hidden(payload) {
        return false;
    }
    true
}

/// Return a numeric rank for visibility comparison (higher = more visible).
/// `Public` is the highest; `Private` is lowest.
fn vis_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Public => 10,
        Visibility::Protected => 8,
        Visibility::Internal => 6,
        Visibility::Package => 5,
        Visibility::Crate => 4,
        Visibility::Private => 0,
    }
}

/// True if the entry carries a `doc_hidden` attribute token.
pub(crate) fn is_doc_hidden(payload: &OwnedEntryPayload) -> bool {
    payload.symbol.attrs.iter().any(|a| a.token == "doc_hidden")
}

/// Build a `parent → Vec<child>` index from the table.
fn build_child_index(table: &PayloadTable) -> BTreeMap<IntroId, Vec<IntroId>> {
    let mut index: BTreeMap<IntroId, Vec<IntroId>> = BTreeMap::new();
    for (id, _) in table.live_entries() {
        if let Some(parent) = table.parent_of(id) {
            index.entry(parent).or_default().push(id);
        }
    }
    index
}

// ---------------------------------------------------------------------------
// Hash class computation
// ---------------------------------------------------------------------------

// Domain tags (frozen per §3.3 / §0.2).
const DOMAIN_SURFACE: &str = "nudox.apisurface.v1";
const DOMAIN_EMBED: &str = "nudox.embed.v1";

/// Compute the `api_surface_hash` for one exported entry (§3.3).
///
/// Covers the S-marked F1 frames (§6.2 S column), **excluding** the moniker-class
/// frames (`name`, `parent` — keys 1 and 6). Canonical order follows the §6.2
/// registry table order.
///
/// The preimage is built from the following fields in order:
/// `vis | kind | cfg? | attrs(sorted) | deprecated? | retgt? | fnsig? |`
/// `gparam* | where* | in* | out* | fieldty? | recform? | recfield* |`
/// `vform? | vdiscr? | super* | tflags? | iof? | ifor? | iflags? |`
/// `cty? | auto* | type? | lfact*`
pub fn api_surface_hash(item: &ApiItem) -> ContentBlake3 {
    item.api_surface_hash
}

/// Compute the `embed_hash` for one exported entry (§3.3).
///
/// Covers: name ‖ moniker paths ‖ doc paragraphs ‖ rendered signature
/// (`in`/`out`/`fieldty`/`type` lines).
pub fn embed_hash(item: &ApiItem) -> ContentBlake3 {
    item.embed_hash
}

/// Return the `payload_hash` from an `ApiItem` (exact identity).
pub fn payload_hash(item: &ApiItem) -> ContentBlake3 {
    item.payload_hash
}

// ---------------------------------------------------------------------------
// Internal: canonical preimage builders
// ---------------------------------------------------------------------------

fn compute_api_surface_hash(payload: &OwnedEntryPayload, _id: IntroId) -> ContentBlake3 {
    let mut buf = Vec::with_capacity(256);

    // 2. vis
    push_vis(&mut buf, payload.symbol.visibility);
    // 3. kind discriminant
    buf.push(payload.kind_disc.as_u16() as u8);
    buf.push((payload.kind_disc.as_u16() >> 8) as u8);
    // 7. cfg
    if let Some(ref cfg) = payload.symbol.cfg {
        push_cfg(&mut buf, cfg);
    }
    // 8. attrs (sorted by token, then arg)
    let mut sorted_attrs = payload.symbol.attrs.clone();
    sorted_attrs.sort_by(|a, b| a.token.cmp(&b.token).then(a.arg.cmp(&b.arg)));
    for attr in &sorted_attrs {
        push_str(&mut buf, &attr.token);
        if let Some(ref arg) = attr.arg {
            push_str(&mut buf, arg);
        }
    }
    // 9. deprecated flag
    buf.push(payload.symbol.deprecation.is_some() as u8);

    // Kind-specific S-marked fields:
    match &payload.kind {
        KindWire::Reexport(rx) => {
            // 13. retgt
            push_stable_ref(&mut buf, &rx.target);
        }
        KindWire::Function(fw) => {
            // 14. fnsig
            push_fnsig(&mut buf, &fw.sig);
            // 15. gparam*
            for gp in fw.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            // 16. where* (sorted)
            let mut sorted_where: Vec<&WherePredWire> = fw.wheres.iter().collect();
            sorted_where.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sorted_where {
                push_where_pred(&mut buf, wp);
            }
            // 17. in*
            for p in fw.input_params.iter() {
                if let Some(ref n) = p.name {
                    push_str(&mut buf, n);
                }
                push_type_ref(&mut buf, &p.ty);
            }
            // 18. out*
            for p in fw.output_params.iter() {
                if let Some(ref n) = p.name {
                    push_str(&mut buf, n);
                }
                push_type_ref(&mut buf, &p.ty);
            }
        }
        KindWire::Field(fw) => {
            // 19. fieldty
            if let Some(ref ty) = fw.ty {
                push_type_ref(&mut buf, ty);
            }
        }
        KindWire::Record(rw) => {
            // 20. recform
            push_record_form(&mut buf, &rw.form);
            // 21. recfield* (ordered)
            for f in rw.fields.iter() {
                buf.extend_from_slice(f.as_bytes());
            }
            // 15. gparam, 16. where
            for gp in rw.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            let mut sw: Vec<&WherePredWire> = rw.wheres.iter().collect();
            sw.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sw {
                push_where_pred(&mut buf, wp);
            }
            // 31. auto (set, sorted)
            let mut auto_sorted: Vec<&AutoFact> = rw.auto.iter().collect();
            auto_sorted.sort_by_key(|a| auto_trait_rank(&a.trait_));
            for af in auto_sorted {
                push_auto_fact(&mut buf, af);
            }
        }
        KindWire::Enum(ew) => {
            // variants listed as children; here we record the generics/where/auto
            for gp in ew.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            let mut sw: Vec<&WherePredWire> = ew.wheres.iter().collect();
            sw.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sw {
                push_where_pred(&mut buf, wp);
            }
            let mut auto_sorted: Vec<&AutoFact> = ew.auto.iter().collect();
            auto_sorted.sort_by_key(|a| auto_trait_rank(&a.trait_));
            for af in auto_sorted {
                push_auto_fact(&mut buf, af);
            }
        }
        KindWire::Variant(vw) => {
            // 22. vform
            push_variant_form(&mut buf, &vw.form);
            // 23. vdiscr
            if let Some(ref d) = vw.discr {
                push_str(&mut buf, d);
            }
        }
        KindWire::Trait(tw) => {
            // 24. super* (set, sorted)
            let mut supers: Vec<&TypeRefWire> = tw.supers.iter().collect();
            supers.sort_by_key(|t| canonical_type_ref(t));
            for s in supers {
                push_type_ref(&mut buf, s);
            }
            // 25. tflags
            push_trait_flags(&mut buf, &tw.flags);
            // 15. gparam, 16. where
            for gp in tw.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            let mut sw: Vec<&WherePredWire> = tw.wheres.iter().collect();
            sw.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sw {
                push_where_pred(&mut buf, wp);
            }
        }
        KindWire::Impl(iw) => {
            // 26. iof
            if let Some(ref tr) = iw.of {
                push_type_ref(&mut buf, tr);
            }
            // 27. ifor
            push_type_expr(&mut buf, &iw.self_ty);
            // 28. iflags
            buf.push(iw.flags.negative as u8);
            buf.push(iw.flags.blanket as u8);
            // 15. gparam, 16. where
            for gp in iw.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            let mut sw: Vec<&WherePredWire> = iw.wheres.iter().collect();
            sw.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sw {
                push_where_pred(&mut buf, wp);
            }
        }
        KindWire::Const(cw) => {
            // 29. cty
            push_type_ref(&mut buf, &cw.ty);
            // cval is NOT S-marked (key 30)
        }
        KindWire::Static(sw) => {
            // 29. cty
            push_type_ref(&mut buf, &sw.ty);
            // mutable flag (S-marked via static-mut-toggle lint A-16)
            buf.push(sw.mutable as u8);
        }
        KindWire::Type(taw) => {
            // 32. type
            push_type_expr(&mut buf, &taw.ty);
            // 15. gparam, 16. where
            for gp in taw.generics.iter() {
                push_generic_param(&mut buf, gp);
            }
            let mut sw: Vec<&WherePredWire> = taw.wheres.iter().collect();
            sw.sort_by_key(|w| canonical_type_expr(&w.target));
            for wp in sw {
                push_where_pred(&mut buf, wp);
            }
            let mut auto_sorted: Vec<&AutoFact> = taw.auto.iter().collect();
            auto_sorted.sort_by_key(|a| auto_trait_rank(&a.trait_));
            for af in auto_sorted {
                push_auto_fact(&mut buf, af);
            }
        }
        KindWire::Module(_) => {
            // Modules carry no kind-specific S-marked fields beyond vis/kind.
        }
        KindWire::Param(p) => {
            // 17. in — a Param entry's (name, type) is S-marked exactly like
            // a function input parameter: name is identity-bearing, type is
            // the primary signature surface.
            if let Some(ref n) = p.name {
                push_str(&mut buf, n);
            }
            push_type_ref(&mut buf, &p.ty);
        }
    }

    ContentBlake3::from_domain(DOMAIN_SURFACE, &buf)
}

fn compute_embed_hash(
    payload: &OwnedEntryPayload,
    // The canonical path is already contained in `all_paths`; kept in the
    // signature for call-site clarity (§3.3 embed preimage = name ‖ moniker ‖ doc ‖ sig).
    _canonical: &MonikerPath,
    all_paths: &[MonikerPath],
) -> ContentBlake3 {
    let mut buf = Vec::with_capacity(256);

    // E-1: name
    push_str(&mut buf, &payload.symbol.name);

    // E-10: alias (all moniker paths, sorted)
    let mut path_strs: Vec<String> = all_paths
        .iter()
        .map(|p| p.0.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::"))
        .collect();
    path_strs.sort();
    for s in &path_strs {
        push_str(&mut buf, s);
    }

    // E-11: doc (paragraphs)
    if let Some(ref doc) = payload.symbol.documentation {
        push_str(&mut buf, doc);
    }

    // E rendered signature: in/out/fieldty/type
    match &payload.kind {
        KindWire::Function(fw) => {
            for p in fw.input_params.iter() {
                push_type_ref(&mut buf, &p.ty);
            }
            for p in fw.output_params.iter() {
                push_type_ref(&mut buf, &p.ty);
            }
        }
        KindWire::Field(fw) => {
            if let Some(ref ty) = fw.ty {
                push_type_ref(&mut buf, ty);
            }
        }
        KindWire::Type(taw) => {
            push_type_expr(&mut buf, &taw.ty);
        }
        _ => {}
    }

    ContentBlake3::from_domain(DOMAIN_EMBED, &buf)
}

// ---------------------------------------------------------------------------
// Canonical byte renderers (frozen, reusing F1 ASCII encoding from §6.2)
// ---------------------------------------------------------------------------

fn push_str(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn push_vis(buf: &mut Vec<u8>, v: Visibility) {
    buf.push(v as u8);
}

fn push_stable_ref(buf: &mut Vec<u8>, sr: &StableRef) {
    sr.encode(buf);
}

fn push_type_ref(buf: &mut Vec<u8>, tr: &TypeRefWire) {
    match tr {
        TypeRefWire::Same(id) => {
            buf.push(0x01);
            buf.extend_from_slice(id.as_bytes());
        }
        TypeRefWire::Foreign(sr) => {
            buf.push(0x02);
            push_stable_ref(buf, sr);
        }
    }
}

/// Canonical type-expression bytes (for sorting and hashing).
pub(crate) fn canonical_type_ref(tr: &TypeRefWire) -> Vec<u8> {
    let mut buf = Vec::new();
    push_type_ref(&mut buf, tr);
    buf
}

fn push_type_expr(buf: &mut Vec<u8>, tw: &TypeWire) {
    match tw {
        TypeWire::SelfType => buf.push(0x01),
        TypeWire::Never => buf.push(0x02),
        TypeWire::Any => buf.push(0x03),
        TypeWire::Primitive(p) => {
            buf.push(0x04);
            push_primitive(buf, p);
        }
        TypeWire::Tuple(refs) => {
            buf.push(0x05);
            buf.extend_from_slice(&(refs.len() as u32).to_le_bytes());
            for r in refs.iter() {
                push_type_ref(buf, r);
            }
        }
        TypeWire::Slice(r) => {
            buf.push(0x06);
            push_type_ref(buf, r);
        }
        TypeWire::Array { ty, length } => {
            buf.push(0x07);
            push_type_ref(buf, ty);
            buf.extend_from_slice(&length.to_le_bytes());
        }
        TypeWire::Union(refs) => {
            buf.push(0x08);
            let mut sorted: Vec<Vec<u8>> = refs.iter().map(canonical_type_ref).collect();
            sorted.sort();
            for s in &sorted {
                buf.extend_from_slice(s);
            }
        }
        TypeWire::Intersection(refs) => {
            buf.push(0x09);
            let mut sorted: Vec<Vec<u8>> = refs.iter().map(canonical_type_ref).collect();
            sorted.sort();
            for s in &sorted {
                buf.extend_from_slice(s);
            }
        }
    }
}

pub(crate) fn canonical_type_expr(tw: &TypeWire) -> Vec<u8> {
    let mut buf = Vec::new();
    push_type_expr(&mut buf, tw);
    buf
}

fn push_primitive(buf: &mut Vec<u8>, p: &PrimitiveWire) {
    match p {
        PrimitiveWire::Integer { signed, width } => {
            buf.push(0x10);
            buf.push(*signed as u8);
            push_width(buf, width);
        }
        PrimitiveWire::Float(w) => {
            buf.push(0x11);
            push_width(buf, w);
        }
        PrimitiveWire::Bool => buf.push(0x12),
        PrimitiveWire::Char => buf.push(0x13),
        PrimitiveWire::Str => buf.push(0x14),
        PrimitiveWire::MutPointer(tr) => {
            buf.push(0x15);
            push_type_ref(buf, tr);
        }
        PrimitiveWire::ConstPointer(tr) => {
            buf.push(0x16);
            push_type_ref(buf, tr);
        }
        PrimitiveWire::Reference { mutable, ty, .. } => {
            // lifetime excluded from skeleton (§4.6)
            buf.push(0x17);
            buf.push(*mutable as u8);
            push_type_ref(buf, ty);
        }
        PrimitiveWire::Builtin(s) => {
            buf.push(0x18);
            push_str(buf, s);
        }
    }
}

fn push_width(buf: &mut Vec<u8>, w: &WidthWire) {
    match w {
        WidthWire::Arch => buf.push(0x00),
        WidthWire::Fixed(n) => {
            buf.push(0x01);
            buf.extend_from_slice(&n.to_le_bytes());
        }
    }
}

fn push_fnsig(buf: &mut Vec<u8>, sig: &FnSigFlags) {
    push_self_kind(buf, &sig.self_kind);
    buf.push(sig.is_async as u8);
    buf.push(sig.is_const as u8);
    buf.push(sig.is_unsafe as u8);
    if let Some(ref abi) = sig.abi {
        buf.push(0x01);
        push_str(buf, abi);
    } else {
        buf.push(0x00);
    }
    buf.push(sig.variadic as u8);
    buf.push(sig.defaulted as u8);
}

fn push_self_kind(buf: &mut Vec<u8>, sk: &SelfKind) {
    match sk {
        SelfKind::None => buf.push(0x00),
        SelfKind::Value => buf.push(0x01),
        SelfKind::Ref => buf.push(0x02),
        SelfKind::RefMut => buf.push(0x03),
        SelfKind::Arbitrary(tr) => {
            buf.push(0x04);
            push_type_ref(buf, tr);
        }
    }
}

fn push_generic_param(buf: &mut Vec<u8>, gp: &GenericParamWire) {
    match gp {
        GenericParamWire::Lifetime { name } => {
            buf.push(0x01);
            push_str(buf, name);
        }
        GenericParamWire::Type { name, bounds, default } => {
            buf.push(0x02);
            push_str(buf, name);
            // bounds sorted
            let mut sorted_bounds: Vec<Vec<u8>> = bounds.iter().map(canonical_type_ref).collect();
            sorted_bounds.sort();
            buf.extend_from_slice(&(sorted_bounds.len() as u32).to_le_bytes());
            for b in &sorted_bounds {
                buf.extend_from_slice(b);
            }
            // default
            if let Some(d) = default {
                buf.push(0x01);
                push_type_expr(buf, d);
            } else {
                buf.push(0x00);
            }
        }
        GenericParamWire::Const { name, ty, default } => {
            buf.push(0x03);
            push_str(buf, name);
            push_type_ref(buf, ty);
            if let Some(d) = default {
                buf.push(0x01);
                push_str(buf, d);
            } else {
                buf.push(0x00);
            }
        }
    }
}

fn push_where_pred(buf: &mut Vec<u8>, wp: &WherePredWire) {
    push_type_expr(buf, &wp.target);
    let mut sorted_bounds: Vec<Vec<u8>> = wp.bounds.iter().map(canonical_type_ref).collect();
    sorted_bounds.sort();
    buf.extend_from_slice(&(sorted_bounds.len() as u32).to_le_bytes());
    for b in &sorted_bounds {
        buf.extend_from_slice(b);
    }
}

fn push_record_form(buf: &mut Vec<u8>, rf: &RecordForm) {
    buf.push(match rf {
        RecordForm::Struct => 0x01,
        RecordForm::Tuple => 0x02,
        RecordForm::Unit => 0x03,
        RecordForm::Union => 0x04,
    });
}

fn push_variant_form(buf: &mut Vec<u8>, vf: &VariantForm) {
    buf.push(match vf {
        VariantForm::Unit => 0x01,
        VariantForm::Tuple => 0x02,
        VariantForm::Struct => 0x03,
    });
}

fn push_trait_flags(buf: &mut Vec<u8>, tf: &TraitFlags) {
    buf.push(tf.is_auto as u8);
    buf.push(tf.is_unsafe as u8);
    buf.push(match tf.dyn_compat {
        crate::wire::TriState::Yes => 0x01,
        crate::wire::TriState::No => 0x02,
        crate::wire::TriState::Unknown => 0x00,
    });
    buf.push(match tf.sealed {
        Sealed::None => 0x00,
        Sealed::PubApi => 0x01,
        Sealed::Full => 0x02,
    });
}

fn push_auto_fact(buf: &mut Vec<u8>, af: &AutoFact) {
    buf.push(auto_trait_rank(&af.trait_));
    buf.push(match af.state {
        AutoState::Yes => 0x01,
        AutoState::No => 0x02,
        AutoState::Cond => 0x03,
    });
}

fn auto_trait_rank(t: &AutoTrait) -> u8 {
    match t {
        AutoTrait::Send => 0,
        AutoTrait::Sync => 1,
        AutoTrait::Unpin => 2,
        AutoTrait::UnwindSafe => 3,
        AutoTrait::RefUnwindSafe => 4,
    }
}

fn push_cfg(buf: &mut Vec<u8>, cfg: &CfgExpr) {
    match cfg {
        CfgExpr::All(subs) => {
            buf.push(0x01);
            buf.extend_from_slice(&(subs.len() as u32).to_le_bytes());
            for s in subs.iter() {
                push_cfg(buf, s);
            }
        }
        CfgExpr::Any(subs) => {
            buf.push(0x02);
            buf.extend_from_slice(&(subs.len() as u32).to_le_bytes());
            for s in subs.iter() {
                push_cfg(buf, s);
            }
        }
        CfgExpr::Not(inner) => {
            buf.push(0x03);
            push_cfg(buf, inner);
        }
        CfgExpr::Feature(s) => {
            buf.push(0x04);
            push_str(buf, s);
        }
        CfgExpr::TargetOs(s) => {
            buf.push(0x05);
            push_str(buf, s);
        }
        CfgExpr::TargetArch(s) => {
            buf.push(0x06);
            push_str(buf, s);
        }
        CfgExpr::Other(s) => {
            buf.push(0x07);
            push_str(buf, s);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ir::change::IntroId;
    use crate::wire::PayloadTable;
    use ir::kind::KindDiscriminant;
    use crate::wire::{
        EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload,
        SymbolWire,
    };
    use ir::entry::Visibility;

    fn make_sym(name: &str, vis: Visibility, doc: Option<&str>) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: vis,
            documentation: doc.map(|s| s.to_string()),
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        }
    }

    fn fn_payload(name: &str) -> OwnedEntryPayload {
        let sym = make_sym(name, Visibility::Public, None);
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn fn_payload_with_doc(name: &str, doc: &str) -> OwnedEntryPayload {
        let sym = make_sym(name, Visibility::Public, Some(doc));
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    /// V-1 acceptance: doc-only edit moves embed_hash + payload_hash but NOT api_surface_hash.
    #[test]
    fn v1_doc_only_edit_gates() {
        let id = intro(1);
        let p_without_doc = fn_payload("foo");
        let p_with_doc = fn_payload_with_doc("foo", "This function does something.");

        let path = MonikerPath::new(vec![SmolStr::new("foo")]);

        let ash_before = compute_api_surface_hash(&p_without_doc, id);
        let ash_after = compute_api_surface_hash(&p_with_doc, id);
        assert_eq!(ash_before, ash_after, "api_surface_hash must not change on doc-only edit");

        let eh_before = compute_embed_hash(&p_without_doc, &path, std::slice::from_ref(&path));
        let eh_after = compute_embed_hash(&p_with_doc, &path, std::slice::from_ref(&path));
        assert_ne!(eh_before, eh_after, "embed_hash must change on doc-only edit");

        assert_ne!(
            p_without_doc.payload_hash, p_with_doc.payload_hash,
            "payload_hash must change on doc-only edit"
        );
    }

    #[test]
    fn surface_exports_pub_items() {
        let mut table = PayloadTable::new();
        let id = intro(1);
        table.insert_live(id, fn_payload("foo"), None);

        let policy = ExportPolicy::default();
        let surf = surface(&table, &policy);

        assert!(surf.items.contains_key(&id));
        let path = MonikerPath::new(vec![SmolStr::new("foo")]);
        assert_eq!(surf.monikers.get(&path), Some(&id));
    }

    #[test]
    fn surface_excludes_private_items() {
        let mut table = PayloadTable::new();
        let id = intro(2);
        let sym = make_sym("bar", Visibility::Private, None);
        let payload = OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        table.insert_live(id, payload, None);

        let surf = surface(&table, &ExportPolicy::default());
        assert!(!surf.items.contains_key(&id), "private item should not be exported");
    }

    #[test]
    fn surface_excludes_doc_hidden_by_default() {
        let mut table = PayloadTable::new();
        let id = intro(3);
        let mut sym = make_sym("hidden_fn", Visibility::Public, None);
        sym.attrs.push(AttrTok { token: "doc_hidden".into(), arg: None });
        let payload = OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        table.insert_live(id, payload, None);

        let surf = surface(&table, &ExportPolicy::default());
        assert!(!surf.items.contains_key(&id), "doc_hidden item should not be exported by default");

        let permissive = ExportPolicy { include_doc_hidden: true, ..Default::default() };
        let surf2 = surface(&table, &permissive);
        assert!(surf2.items.contains_key(&id), "doc_hidden item should be exported with include_doc_hidden=true");
    }

    #[test]
    fn surface_child_items_exported_under_parent() {
        let mut table = PayloadTable::new();
        let mod_id = intro(10);
        let fn_id = intro(11);

        let mod_sym = make_sym("mymod", Visibility::Public, None);
        let mod_payload = OwnedEntryPayload::sealed(
            mod_sym,
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        );
        table.insert_live(mod_id, mod_payload, None);
        table.insert_live(fn_id, fn_payload("inner_fn"), Some(mod_id));

        let surf = surface(&table, &ExportPolicy::default());
        assert!(surf.items.contains_key(&fn_id));
        let path = MonikerPath::new(vec![SmolStr::new("mymod"), SmolStr::new("inner_fn")]);
        assert_eq!(surf.monikers.get(&path), Some(&fn_id));
    }
}
