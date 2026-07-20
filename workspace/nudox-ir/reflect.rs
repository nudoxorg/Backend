//! Pure reflections over an [`crate::view::IrView`]: `exported`, `monikers`,
//! `boundary`, and the [`DepIrProvider`] contract (§5.3 / U-4).
//!
//! There is **no** `ApiSurface` artifact. "What is the public surface?" is not a
//! stored object — it is a pure function of the IR frames plus a policy and a
//! cfg assignment. Callers cache the results as disposable projections keyed by
//! `(channel_tip, policy_id, cfg_id)`; nothing here performs I/O or allocates a
//! sibling store.
//!
//! # Honesty about totality
//!
//! [`ExportPolicy`] and [`CfgAssignment`] are deliberately *minimal but honest*
//! rather than fake-total. The export decision here is visibility + ancestry
//! (an entry is exported iff it and every ancestor are publicly visible under
//! the policy). Language-specific export subtleties (Rust `pub(crate)` re-export
//! chains, TS barrel files) are resolved upstream by the producer/resolve ladder
//! and surface as visibility on the entries; this function does not re-derive
//! them. Where a fact cannot be known it is treated conservatively (excluded),
//! never fabricated.

use nudox_change::{IntroId, PackageLineageId, StableRef};

use crate::symbol::Visibility;
use crate::view::IrView;
use crate::wire::CfgExpr;

// ---------------------------------------------------------------------------
// MonikerPath
// ---------------------------------------------------------------------------

/// A dotted path of names from the package root to a symbol
/// (`"module.submodule.Type.method"`).
///
/// The moniker is derived from the entry's ancestry (parent chain) and the leaf
/// name; it is the stable, human-facing address used in boundary reflections and
/// search projections. Segments are stored root-first.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct MonikerPath {
    segments: Vec<String>,
}

impl MonikerPath {
    /// Construct from root-first segments.
    pub fn from_segments(segments: Vec<String>) -> Self {
        Self { segments }
    }

    /// The path segments, root-first.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Render as a dotted path.
    pub fn dotted(&self) -> String {
        self.segments.join(".")
    }
}

impl std::fmt::Display for MonikerPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.dotted())
    }
}

// ---------------------------------------------------------------------------
// ExportPolicy
// ---------------------------------------------------------------------------

/// The policy deciding which visibilities count as "exported" (part of the
/// public boundary) for a given consumer.
///
/// Minimal but honest: the set of visibility levels that a boundary reflection
/// should surface. The default treats only [`Visibility::Public`] as exported;
/// a crate-internal analysis can widen it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportPolicy {
    /// Visibilities that count as exported (part of the boundary).
    exported_visibilities: Vec<Visibility>,
}

impl ExportPolicy {
    /// The public boundary: only [`Visibility::Public`] symbols are exported.
    pub fn public_only() -> Self {
        Self { exported_visibilities: vec![Visibility::Public] }
    }

    /// A crate-internal view: public and crate-visible symbols are exported.
    pub fn crate_visible() -> Self {
        Self {
            exported_visibilities: vec![
                Visibility::Public,
                Visibility::Crate,
                Visibility::Package,
                Visibility::Internal,
            ],
        }
    }

    /// True if the given visibility is exported under this policy.
    #[inline]
    pub fn permits(&self, visibility: Visibility) -> bool {
        self.exported_visibilities.contains(&visibility)
    }

    /// A stable identifier for this policy, for cache keys. Derived from the
    /// sorted visibility discriminants.
    pub fn policy_id(&self) -> String {
        let mut discs: Vec<u8> = self.exported_visibilities.iter().map(|v| *v as u8).collect();
        discs.sort_unstable();
        discs.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("-")
    }
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self::public_only()
    }
}

// ---------------------------------------------------------------------------
// CfgAssignment
// ---------------------------------------------------------------------------

/// A concrete assignment of cfg predicate atoms, used to evaluate whether a
/// symbol guarded by a [`CfgExpr`] is active.
///
/// Minimal but honest: an entry with no `cfg` is always active; an entry with a
/// `cfg` is active iff the predicate evaluates true under this assignment. An
/// unknown atom evaluates conservatively to `false` (the symbol is excluded
/// rather than fabricated as present).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CfgAssignment {
    /// Enabled `feature = "..."` names.
    features: Vec<String>,
    /// The `target_os` value, if pinned.
    target_os: Option<String>,
    /// The `target_arch` value, if pinned.
    target_arch: Option<String>,
    /// Enabled free-form `other:` atoms.
    others: Vec<String>,
}

impl CfgAssignment {
    /// An assignment with no atoms enabled (only unguarded symbols are active).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Enable a `feature` atom (builder style).
    pub fn with_feature(mut self, name: impl Into<String>) -> Self {
        self.features.push(name.into());
        self
    }

    /// Pin the `target_os` atom (builder style).
    pub fn with_target_os(mut self, value: impl Into<String>) -> Self {
        self.target_os = Some(value.into());
        self
    }

    /// Evaluate a cfg predicate under this assignment. Unknown atoms are
    /// conservatively false.
    pub fn evaluate(&self, expr: &CfgExpr) -> bool {
        match expr {
            CfgExpr::All(children) => children.iter().all(|c| self.evaluate(c)),
            CfgExpr::Any(children) => children.iter().any(|c| self.evaluate(c)),
            CfgExpr::Not(child) => !self.evaluate(child),
            CfgExpr::Feature(name) => self.features.iter().any(|f| f == name),
            CfgExpr::TargetOs(os) => self.target_os.as_deref() == Some(os.as_str()),
            CfgExpr::TargetArch(arch) => self.target_arch.as_deref() == Some(arch.as_str()),
            CfgExpr::Other(atom) => self.others.iter().any(|o| o == atom),
        }
    }

    /// A stable identifier for this assignment, for cache keys.
    pub fn cfg_id(&self) -> String {
        let mut feats = self.features.clone();
        feats.sort();
        let mut others = self.others.clone();
        others.sort();
        format!(
            "f:{}|os:{}|arch:{}|o:{}",
            feats.join(","),
            self.target_os.as_deref().unwrap_or(""),
            self.target_arch.as_deref().unwrap_or(""),
            others.join(","),
        )
    }

    /// True if the entry with this optional cfg guard is active.
    #[inline]
    pub fn is_active(&self, cfg: Option<&CfgExpr>) -> bool {
        cfg.is_none_or(|expr| self.evaluate(expr))
    }
}

// ---------------------------------------------------------------------------
// exported
// ---------------------------------------------------------------------------

/// True if the entry `id` is exported: it is a live entry, its cfg guard is
/// active under `cfg`, its own visibility is permitted by `policy`, and every
/// ancestor up to the root is likewise active and permitted.
///
/// Pure, no I/O. Absent / unknown entries are conservatively not exported.
pub fn exported(ir: &IrView, policy: &ExportPolicy, cfg: &CfgAssignment, id: IntroId) -> bool {
    let mut current = Some(id);
    // Walk the ancestry; every hop must be live, active, and permitted.
    // A short bound guards against a malformed parent cycle.
    let mut guard = 0usize;
    while let Some(intro) = current {
        guard += 1;
        if guard > 1 << 20 {
            return false; // pathological cycle — conservatively excluded
        }
        let Some(payload) = ir.entry(intro) else {
            return false;
        };
        if !cfg.is_active(payload.symbol.cfg.as_ref()) {
            return false;
        }
        if !policy.permits(payload.symbol.visibility) {
            return false;
        }
        current = ir.parent_of(intro);
    }
    true
}

// ---------------------------------------------------------------------------
// monikers
// ---------------------------------------------------------------------------

/// Iterate the `(MonikerPath, IntroId)` of every **exported** entry.
///
/// A non-exported entry never yields a moniker (the graph and search surfaces
/// only address the public boundary). The path is the root-first chain of
/// ancestor names ending in the entry's own name.
pub fn monikers<'a>(
    ir: &'a IrView,
    policy: &'a ExportPolicy,
    cfg: &'a CfgAssignment,
) -> impl Iterator<Item = (MonikerPath, IntroId)> + 'a {
    ir.entries().filter_map(move |(intro, _payload)| {
        if !exported(ir, policy, cfg, intro) {
            return None;
        }
        moniker_path(ir, intro).map(|path| (path, intro))
    })
}

// ---------------------------------------------------------------------------
// boundary
// ---------------------------------------------------------------------------

/// Iterate the `(MonikerPath, StableRef)` of every exported entry — the public
/// boundary as `StableRef`s a dependent can bind against.
///
/// Same gating as [`monikers`]; the difference is the value shape (a
/// cross-package [`StableRef`] rather than the local [`IntroId`]).
pub fn boundary<'a>(
    ir: &'a IrView,
    policy: &'a ExportPolicy,
    cfg: &'a CfgAssignment,
) -> impl Iterator<Item = (MonikerPath, StableRef)> + 'a {
    let package = ir.package().clone();
    monikers(ir, policy, cfg)
        .map(move |(path, intro)| (path, StableRef::new(package.clone(), intro)))
}

/// Build the root-first dotted moniker path for an entry from its ancestry.
/// Returns `None` if any ancestor entry is missing (a malformed view).
fn moniker_path(ir: &IrView, id: IntroId) -> Option<MonikerPath> {
    let mut segments: Vec<String> = Vec::new();
    let mut current = Some(id);
    let mut guard = 0usize;
    while let Some(intro) = current {
        guard += 1;
        if guard > 1 << 20 {
            return None;
        }
        let payload = ir.entry(intro)?;
        segments.push(payload.symbol.name.clone());
        current = ir.parent_of(intro);
    }
    segments.reverse();
    Some(MonikerPath::from_segments(segments))
}

// ---------------------------------------------------------------------------
// DepIrProvider
// ---------------------------------------------------------------------------

/// A dependency was requested but its IR is not available.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DepMissing {
    /// No IR is available for the requested package at the requested pin.
    #[error("dependency IR missing: {package:?} @ {pin}")]
    NotAvailable {
        /// The requested dependency package.
        package: PackageLineageId,
        /// The requested pin (a version / channel-tip token).
        pin: String,
    },
}

/// Serves dependency IR as an [`IrView`] at a pin (§5.3 / U-4).
///
/// Consumers reflect what they need off the returned view (`monikers`,
/// `boundary`); there is no `DepSurfaceProvider` and no pre-baked surface —
/// dependencies are served as *IR*, not as a reified surface artifact.
pub trait DepIrProvider: Send + Sync {
    /// Fetch the IR view for a dependency package at a pin. `pin` is an opaque
    /// version / channel-tip token interpreted by the provider.
    fn ir(&self, package: &PackageLineageId, pin: &str) -> Result<std::sync::Arc<IrView>, DepMissing>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::KindDiscriminant;
    use crate::wire::{EntryPayloadFlags, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire};
    use nudox_change::{EcosystemId, PackageLineageId, PackageName};

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("rust"), PackageName::new("demo"))
    }

    fn intro(seed: &str) -> IntroId {
        IntroId::from_domain("test.intro", seed.as_bytes())
    }

    fn entry(name: &str, visibility: Visibility, cfg: Option<CfgExpr>) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.into(),
            visibility,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 1,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg,
        };
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        )
    }

    #[test]
    fn non_exported_entry_yields_no_moniker() {
        let mut ir = IrView::new(pkg());
        let root = intro("root");
        let priv_child = intro("secret");
        ir.insert_entry(root, entry("demo", Visibility::Public, None), None);
        ir.insert_entry(priv_child, entry("secret", Visibility::Private, None), Some(root));

        let policy = ExportPolicy::public_only();
        let cfg = CfgAssignment::empty();

        assert!(exported(&ir, &policy, &cfg, root));
        assert!(!exported(&ir, &policy, &cfg, priv_child));

        let names: Vec<String> =
            monikers(&ir, &policy, &cfg).map(|(p, _)| p.dotted()).collect();
        assert!(names.contains(&"demo".to_string()));
        assert!(!names.iter().any(|n| n.contains("secret")));
    }

    #[test]
    fn cfg_off_entry_is_excluded() {
        let mut ir = IrView::new(pkg());
        let root = intro("root");
        let gated = intro("gated");
        ir.insert_entry(root, entry("demo", Visibility::Public, None), None);
        ir.insert_entry(
            gated,
            entry("gated", Visibility::Public, Some(CfgExpr::Feature("nightly".into()))),
            Some(root),
        );

        let policy = ExportPolicy::public_only();
        let cfg_off = CfgAssignment::empty();
        let cfg_on = CfgAssignment::empty().with_feature("nightly");

        assert!(!exported(&ir, &policy, &cfg_off, gated));
        assert!(exported(&ir, &policy, &cfg_on, gated));
    }

    #[test]
    fn private_ancestor_hides_public_child() {
        let mut ir = IrView::new(pkg());
        let root = intro("root");
        let hidden_mod = intro("hidden");
        let child = intro("child");
        ir.insert_entry(root, entry("demo", Visibility::Public, None), None);
        ir.insert_entry(hidden_mod, entry("hidden", Visibility::Private, None), Some(root));
        ir.insert_entry(child, entry("child", Visibility::Public, None), Some(hidden_mod));

        let policy = ExportPolicy::public_only();
        let cfg = CfgAssignment::empty();
        // The child is public but sits under a private module → not exported.
        assert!(!exported(&ir, &policy, &cfg, child));
    }

    #[test]
    fn boundary_yields_stable_refs_for_exported() {
        let mut ir = IrView::new(pkg());
        let root = intro("root");
        ir.insert_entry(root, entry("demo", Visibility::Public, None), None);

        let policy = ExportPolicy::public_only();
        let cfg = CfgAssignment::empty();
        let refs: Vec<StableRef> = boundary(&ir, &policy, &cfg).map(|(_, r)| r).collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package, pkg());
        assert_eq!(refs[0].intro, root);
    }
}
