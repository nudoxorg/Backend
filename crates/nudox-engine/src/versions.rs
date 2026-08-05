//! The version plane: how the engine holds more than one generation of a
//! package, and how a caller asks which generations exist.
//!
//! # Why this lives in the engine and not in the store
//!
//! `nudox_store::corpus::Corpus` is a `HashMap<PackageLineageId, Arc<PackageView>>`.
//! `PackageLineageId` is *deliberately* version-free — it names the lineage
//! (`cargo:axum`), which is exactly the identity that has to stay constant
//! across releases for `IntroId` continuity to mean anything. The consequence
//! is that the corpus has room for exactly one resident `PackageView` per
//! package: inserting a second generation of `cargo:axum` replaces the first.
//!
//! That is not a bug in the store. A `Corpus` is "the world as it currently
//! is" — the thing search, `open_symbol` and Trustfall queries resolve
//! against — and there is only one *current* world. What was missing is a
//! second structure holding the generations that are loaded but not current,
//! so that a timeline has something to walk and a dropdown has something to
//! list.
//!
//! [`VersionRegistry`] is that structure. It keeps every loaded
//! `Arc<PackageView>` for a lineage and designates one of them as current;
//! the corpus always holds a clone of whichever `Arc` is current. The two are
//! kept in step by [`VersionRegistry::record`] (load path) and
//! [`crate::EngineHandle::select_version`] (user path), which are the only two
//! places the designation moves.
//!
//! # Locking
//!
//! `std::sync::RwLock`, not `tokio::sync::RwLock`. The registry holds a
//! handful of `Arc` clones and a few short strings per package, every operation
//! on it is a map lookup and a `Vec` scan, and the guard is never held across
//! an `.await`. Paying for an async lock here would buy nothing and would force
//! [`crate::EngineHandle::versions`] — a pure in-memory read that a GUI wants
//! during layout — to become a channel round-trip.
//!
//! That is the justification for `versions()` being a plain sync getter rather
//! than following the bounded-`flume` convention of `search`/`open_symbol`:
//! those stream results that must be *computed*, progressively, and can be
//! superseded. A version list is neither. It is already resident, it is
//! bounded by the number of generations the caller itself asked to load, and
//! there is nothing for a `Gen` to guard.
//!
//! # Ordering
//!
//! Versions are strings, and the engine refuses to depend on a semver crate
//! for something this small, so ordering is decided by [`VersionOrder`]:
//!
//! * A version that parses as dotted-numeric core plus optional pre-release
//!   (`1.2.3`, `v0.8.9`, `2.0.0-rc.1`, `1.0.0+build7`) orders by that structure,
//!   with the semver rules that a release outranks its own pre-releases and
//!   that numeric pre-release identifiers outrank nothing alphabetic.
//! * Anything else (`main`, `2024-06-01a`, `latest`) is [`VersionOrder::Opaque`]
//!   and sorts *below* every parseable version, ordered lexically among
//!   themselves. Sorting the unknown below the known is the conservative
//!   choice: an unparseable string never displaces a real release from the top
//!   of the dropdown.
//! * Exact ties are broken by load sequence, so the ordering is total and
//!   stable regardless of which order the producers happened to finish in.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use nudox_ir::change::PackageLineageId;
use nudox_store::package::PackageView;

use crate::{
    runtime::EngineHandle,
    wire::{Gen, SharedStr, VersionEvent, VersionList, VersionRow},
};

// ---------------------------------------------------------------------------
// Version ordering
// ---------------------------------------------------------------------------

/// A pre-release segment.
///
/// The variant order is load-bearing: semver §11.4.3 says numeric identifiers
/// always have lower precedence than non-numeric ones, and `derive(Ord)` on a
/// fieldless-ordered enum gives exactly that by listing `Num` first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreSeg {
    /// A purely numeric identifier, compared numerically (`rc.9 < rc.10`).
    Num(u64),
    /// An alphanumeric identifier, compared lexically.
    Alpha(String),
}

/// The pre-release component of a version, ordered so that a release outranks
/// its own pre-releases.
///
/// `Option<Vec<PreSeg>>` would be the obvious encoding, but `Option`'s derived
/// order puts `None` *below* `Some`, which would make `1.0.0` sort below
/// `1.0.0-alpha` — the exact inversion of the rule. The leading `u8` is an
/// explicit rank that fixes the direction: `0` for a pre-release, `1` for a
/// release.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PreOrder(u8, Vec<PreSeg>);

/// A comparable version key derived from the caller's version string.
///
/// `Opaque` is listed first so that `derive(Ord)` places every unparseable
/// version below every parseable one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum VersionOrder {
    /// The string did not parse as a dotted-numeric version.
    Opaque(String),
    /// A parsed version: numeric core plus pre-release rank.
    Semver {
        /// The dotted-numeric core, zero-padded to at least four components so
        /// that `1.2` and `1.2.0` compare equal instead of the shorter one
        /// sorting below on `Vec`'s prefix rule.
        core: Vec<u64>,
        /// Release-vs-pre-release rank and pre-release identifiers.
        pre: PreOrder,
    },
}

impl VersionOrder {
    /// Derive an ordering key from a raw version string.
    ///
    /// Tolerant on input by design — this runs on strings the caller typed
    /// into a `PackageVersionSpec`, and refusing to order a version because it
    /// carried a `v` prefix would be a worse outcome than ordering it.
    fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        // A leading `v` is a display convention (`v1.2.3`), not part of the
        // version, and stripping it is what lets tag-shaped strings order with
        // manifest-shaped ones.
        let body = trimmed.strip_prefix('v').unwrap_or(trimmed);
        // Build metadata is explicitly ignored for precedence (semver §10).
        let body = body.split('+').next().unwrap_or(body);

        let (core_str, pre_str) = match body.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (body, None),
        };

        if core_str.is_empty() {
            return Self::Opaque(trimmed.to_owned());
        }

        let mut core = Vec::new();
        for seg in core_str.split('.') {
            match seg.parse::<u64>() {
                Ok(n) => core.push(n),
                // One non-numeric core component makes the whole string
                // opaque. Partial parsing ("1.2.x" as 1.2) would invent
                // precision the string does not have.
                Err(_) => return Self::Opaque(trimmed.to_owned()),
            }
        }
        while core.len() < 4 {
            core.push(0);
        }

        let pre = match pre_str {
            None => PreOrder(1, Vec::new()),
            Some(p) => PreOrder(
                0,
                p.split('.')
                    .map(|seg| match seg.parse::<u64>() {
                        Ok(n) => PreSeg::Num(n),
                        Err(_) => PreSeg::Alpha(seg.to_owned()),
                    })
                    .collect(),
            ),
        };

        Self::Semver { core, pre }
    }
}

// ---------------------------------------------------------------------------
// Loaded generations
// ---------------------------------------------------------------------------

/// The version string used when an `IrSource` supplies no version at all.
///
/// `LoadEvent::Discovered` carries `version: Option<String>`; both in-tree
/// sources always fill it, but the field is optional at the trait level so a
/// third-party source may not. Rather than drop such a generation on the floor
/// or invent a number for it, it is labelled and sorted as opaque — visible in
/// the dropdown, never mistaken for a release.
pub(crate) const UNVERSIONED: &str = "unversioned";

/// One loaded generation of one package.
struct LoadedVersion {
    /// The caller's version string, verbatim.
    version: SharedStr,
    /// Derived ordering key.
    order: VersionOrder,
    /// Monotonic load sequence, used only to break exact `order` ties so the
    /// sort is total and stable.
    seq: u64,
    /// The sealed package view for this generation.
    package: Arc<PackageView>,
}

/// Every loaded generation of one lineage.
struct LineageVersions {
    /// Kept sorted newest-first at all times, so reads never sort.
    versions: Vec<LoadedVersion>,
    /// The version string of the generation that is (or is becoming) resident
    /// in the `Corpus`.
    current: SharedStr,
    /// True once a caller has explicitly chosen a version.
    ///
    /// After an explicit choice, a later-arriving newer generation is recorded
    /// but does **not** steal the designation. A dropdown selection that gets
    /// silently reverted because an unrelated package finished producing is a
    /// worse failure than showing a version that is not the newest.
    pinned: bool,
}

impl LineageVersions {
    /// Whether the `current` designation still names a generation we hold.
    ///
    /// It always should through the paths in this module; the check exists so
    /// that a designation orphaned by a future edit degrades to "fall back to
    /// newest" rather than to a lineage whose current version cannot be read.
    fn has_current(&self) -> bool {
        self.versions.iter().any(|v| v.version == self.current)
    }
}

// ---------------------------------------------------------------------------
// VersionSlice
// ---------------------------------------------------------------------------

/// One generation handed to the timeline builder.
///
/// Crate-private: it names `Arc<PackageView>`, a `nudox-store` type, which must
/// not cross the §L0 seam into `lindsey`. The GUI sees [`VersionRow`] instead.
pub(crate) struct VersionSlice {
    /// The version string for this generation.
    pub(crate) version: SharedStr,
    /// The sealed package view.
    pub(crate) package: Arc<PackageView>,
    /// Whether this is the generation the corpus serves.
    pub(crate) is_current: bool,
}

// ---------------------------------------------------------------------------
// VersionRegistry
// ---------------------------------------------------------------------------

/// Every loaded generation of every package, and which one is current.
///
/// Lives in `EngineInner` beside the `Corpus`; the corpus holds the current
/// generation of each lineage, this holds all of them.
pub(crate) struct VersionRegistry {
    inner: RwLock<RegistryInner>,
}

struct RegistryInner {
    lineages: HashMap<PackageLineageId, LineageVersions>,
    next_seq: u64,
}

impl VersionRegistry {
    /// An empty registry.
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                lineages: HashMap::new(),
                next_seq: 0,
            }),
        }
    }

    /// Record a freshly produced generation.
    ///
    /// Returns `Some(package)` when the caller must now insert that package
    /// into the `Corpus` — that is, when this record moved (or established) the
    /// current designation. Returns `None` when the generation was recorded but
    /// an existing generation stays current, in which case the corpus is
    /// already correct and must not be touched.
    ///
    /// Returning the work to do rather than doing it is deliberate: the corpus
    /// write is `async` and this method is not, and threading a runtime handle
    /// into the registry to hide one `await` would couple two things that have
    /// no reason to know about each other.
    ///
    /// Recording a version string that is already present **replaces** it. That
    /// is the honest behaviour for a reload: two `PackageView`s claiming to be
    /// `1.2.3` are not two generations, they are one generation produced twice,
    /// and keeping both would put a duplicate row in the dropdown.
    pub(crate) fn record(
        &self,
        lineage: &PackageLineageId,
        version: Option<String>,
        package: Arc<PackageView>,
    ) -> Option<Arc<PackageView>> {
        let version: SharedStr = version
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| UNVERSIONED.to_owned())
            .into();
        let order = VersionOrder::parse(&version);

        let mut guard = self
            .inner
            .write()
            .expect("version registry lock is never held across a panic");
        let seq = guard.next_seq;
        guard.next_seq += 1;

        let entry = guard
            .lineages
            .entry(lineage.clone())
            .or_insert_with(|| LineageVersions {
                versions: Vec::new(),
                // Provisional; corrected immediately below once the first
                // generation is actually pushed.
                current: version.clone(),
                pinned: false,
            });

        let record = LoadedVersion {
            version: version.clone(),
            order,
            seq,
            package: Arc::clone(&package),
        };

        match entry.versions.iter().position(|v| v.version == version) {
            Some(idx) => entry.versions[idx] = record,
            None => entry.versions.push(record),
        }

        // Newest-first. `sort_by` (stable) over a comparator that already
        // includes `seq` makes the result independent of the pre-sort order.
        entry.versions.sort_by(|a, b| {
            b.order
                .cmp(&a.order)
                .then_with(|| b.seq.cmp(&a.seq))
                .then_with(|| a.version.as_bytes().cmp(b.version.as_bytes()))
        });

        // Decide whether the corpus needs repointing.
        //
        // Two cases repoint: the designation is stale (the version it names is
        // gone, or this *is* that version and it was just replaced by a fresh
        // build), and the unpinned case where a newer generation has arrived.
        let newest = entry.versions[0].version.clone();

        let new_current = if entry.has_current() && entry.pinned {
            entry.current.clone()
        } else {
            newest
        };

        let repoint = new_current != entry.current
            // The current generation was rebuilt in place: same string, new
            // `PackageView`. The corpus is holding a stale `Arc` and must be
            // refreshed even though the designation did not move.
            || new_current == version;

        entry.current = new_current.clone();

        if repoint {
            entry
                .versions
                .iter()
                .find(|v| v.version == new_current)
                .map(|v| Arc::clone(&v.package))
        } else {
            None
        }
    }

    /// The GUI-facing list of generations for `lineage`, newest first.
    ///
    /// An unknown lineage yields an empty list rather than an error: "not
    /// loaded" and "loaded with zero versions" are the same observable state
    /// and the caller renders both as an empty dropdown.
    pub(crate) fn versions(&self, lineage: &PackageLineageId) -> VersionList {
        let guard = self
            .inner
            .read()
            .expect("version registry lock is never held across a panic");

        let rows: Vec<VersionRow> = guard
            .lineages
            .get(lineage)
            .map(|entry| {
                entry
                    .versions
                    .iter()
                    .map(|v| VersionRow {
                        version: v.version.clone(),
                        is_current: v.version == entry.current,
                        symbol_count: v.package.view().table().len() as u64,
                    })
                    .collect()
            })
            .unwrap_or_default();

        VersionList {
            package: lineage.clone(),
            versions: rows.into(),
        }
    }

    /// Every generation of `lineage`, **oldest first**, for timeline walking.
    ///
    /// Oldest-first because a timeline is classified by comparing each version
    /// against the one before it, and "before" only means anything in
    /// chronological order. The rows are reversed to newest-first at the wire
    /// boundary, in [`crate::timeline::build`].
    pub(crate) fn slices(&self, lineage: &PackageLineageId) -> Vec<VersionSlice> {
        let guard = self
            .inner
            .read()
            .expect("version registry lock is never held across a panic");

        let Some(entry) = guard.lineages.get(lineage) else {
            return Vec::new();
        };

        entry
            .versions
            .iter()
            .rev() // stored newest-first
            .map(|v| VersionSlice {
                version: v.version.clone(),
                package: Arc::clone(&v.package),
                is_current: v.version == entry.current,
            })
            .collect()
    }

    /// Designate `version` as current for `lineage`.
    ///
    /// Returns the `PackageView` the caller must install in the `Corpus`, or
    /// `None` if that version is not loaded (in which case nothing changed).
    /// Selecting the version that is already current still returns `Some` — an
    /// idempotent re-install is cheap and it means the caller can treat the
    /// return as "here is the authoritative view for what you asked for"
    /// without a special case.
    ///
    /// Marks the lineage pinned, so a later-arriving newer generation will not
    /// silently take the designation back.
    pub(crate) fn set_current(
        &self,
        lineage: &PackageLineageId,
        version: &str,
    ) -> Option<Arc<PackageView>> {
        let mut guard = self
            .inner
            .write()
            .expect("version registry lock is never held across a panic");

        let entry = guard.lineages.get_mut(lineage)?;

        // Take everything needed out of the shared borrow before mutating, so
        // the lookup and the designation change never overlap. `canonical` is
        // the stored `SharedStr` rather than the caller's `&str` — same text,
        // but it shares the allocation every row already holds.
        let (package, canonical) = {
            let found = entry.versions.iter().find(|v| &*v.version == version)?;
            (Arc::clone(&found.package), found.version.clone())
        };

        entry.current = canonical;
        entry.pinned = true;
        Some(package)
    }
}

impl Default for VersionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// EngineHandle surface
// ---------------------------------------------------------------------------

impl EngineHandle {
    /// Every loaded generation of `package`, newest first.
    ///
    /// # Why this is synchronous
    ///
    /// Every other `EngineHandle` query returns a bounded `flume` receiver
    /// because it starts work that streams, can be superseded, and needs a
    /// `Gen` to guard it. None of that applies here: the version list is
    /// already resident in memory (see the module docs on locking), it is
    /// bounded by the number of generations the caller itself asked to load,
    /// and it cannot be partially available. A channel would add a task hop
    /// and a generation token to protect data that was never in flight.
    ///
    /// # What it returns before loading finishes
    ///
    /// Generations appear here as they finish producing, in whatever order the
    /// producers complete. A GUI that renders a dropdown at startup will see it
    /// fill in; subscribing to [`EngineHandle::packages`] and re-reading this
    /// on each `PackageLoadEvent::Loaded` is the intended pattern.
    ///
    /// An unloaded package yields an empty [`VersionList`], not an error.
    pub fn versions(&self, package: &PackageLineageId) -> VersionList {
        self.inner.versions.versions(package)
    }

    /// Switch which generation of `package` the corpus serves.
    ///
    /// # What actually changes
    ///
    /// The `Corpus` holds one resident `PackageView` per lineage, and every
    /// version-free path — `open_symbol`, `search`, `query` — resolves against
    /// it. Selecting a version replaces that resident view, so those paths
    /// begin answering from the selected generation with no change to their
    /// signatures. `SymbolKey` stays valid across the switch, because
    /// `IntroId` is stable across versions: the same key opens the same symbol
    /// in whichever generation is resident, which is precisely what makes a
    /// version dropdown meaningful rather than a navigation reset.
    ///
    /// # Ordering, and the window between the two
    ///
    /// The registry is updated **synchronously**, before this returns, so an
    /// immediately following [`EngineHandle::versions`] reports the new
    /// selection. The corpus write is `async` (it takes the corpus lock) and
    /// completes on the runtime; [`VersionEvent::Switched`] announces that it
    /// has landed. In the window between the two, `versions()` reports the new
    /// current while `open_symbol` still answers from the old one.
    ///
    /// That ordering is chosen rather than tolerated. The engine is the source
    /// of truth for *what is selected*, and a dropdown that visibly snapped
    /// back to the old value for a frame — which is what deferring the registry
    /// update would produce — is a worse artefact than one stale page that is
    /// about to be re-issued anyway.
    ///
    /// The caller should bump its `Gen` and re-issue open streams on
    /// `Switched`. Streams opened under the old `Gen` are not cancelled by this
    /// call: they are answering a question that was legitimately asked, and
    /// §9.3's guard is the caller dropping their `StreamHandle`, not the engine
    /// second-guessing them.
    ///
    /// # Channel capacity
    ///
    /// Capacity 1, `try_send`-safe: exactly one event is ever produced and the
    /// receiver then closes. This stream has no Appendix C entry because it has
    /// no backpressure story to have — there is nothing to fall behind on.
    pub fn select_version(
        &self,
        package: PackageLineageId,
        version: impl Into<String>,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent> {
        // Two steps, not `version.into().into()`: `SharedStr` has `From<&str>`
        // *and* `From<String>`, so a chained conversion leaves the intermediate
        // type ambiguous.
        let owned: String = version.into();
        let version = SharedStr::from(owned);
        let (tx, rx) = flume::bounded::<VersionEvent>(1);

        let Some(target) = self.inner.versions.set_current(&package, &version) else {
            // Capacity 1 and nothing has been sent yet, so `try_send` cannot
            // fail for a live receiver; if the receiver was already dropped
            // there is nobody to tell.
            let _ = tx.try_send(VersionEvent::NotLoaded {
                generation,
                package,
                version,
            });
            return rx;
        };

        let corpus = self.corpus();
        self.spawn(async move {
            corpus.insert(target).await;
            let _ = tx
                .send_async(VersionEvent::Switched {
                    generation,
                    package,
                    version,
                })
                .await;
        });

        rx
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{lineage, package_with};

    fn ord(s: &str) -> VersionOrder {
        VersionOrder::parse(s)
    }

    #[test]
    fn numeric_cores_order_numerically_not_lexically() {
        // The bug a lexical sort produces: "0.10.0" < "0.9.0".
        assert!(ord("0.10.0") > ord("0.9.0"));
        assert!(ord("1.2.10") > ord("1.2.9"));
    }

    #[test]
    fn missing_core_components_pad_to_equality() {
        assert_eq!(ord("1.2"), ord("1.2.0"));
        assert_eq!(ord("1"), ord("1.0.0.0"));
    }

    #[test]
    fn release_outranks_its_own_prereleases() {
        assert!(ord("1.0.0") > ord("1.0.0-rc.1"));
        assert!(ord("1.0.0") > ord("1.0.0-alpha"));
        assert!(ord("1.0.0-rc.1") > ord("0.9.9"));
    }

    #[test]
    fn prerelease_identifiers_follow_semver_precedence() {
        // Numeric identifiers compare numerically...
        assert!(ord("1.0.0-rc.10") > ord("1.0.0-rc.9"));
        // ...and rank below alphanumeric ones.
        assert!(ord("1.0.0-1") < ord("1.0.0-alpha"));
    }

    #[test]
    fn build_metadata_is_ignored_and_v_prefix_stripped() {
        assert_eq!(ord("1.2.3+build9"), ord("1.2.3"));
        assert_eq!(ord("v1.2.3"), ord("1.2.3"));
    }

    #[test]
    fn opaque_versions_sort_below_every_parseable_one() {
        assert!(ord("main") < ord("0.0.0"));
        assert!(ord("latest") < ord("0.0.1-alpha"));
        assert!(ord(UNVERSIONED) < ord("0.0.0"));
        // ...and lexically among themselves.
        assert!(ord("beta") < ord("main"));
    }

    #[test]
    fn record_orders_newest_first_regardless_of_arrival_order() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");

        // Deliberately out of order.
        reg.record(&lid, Some("0.8.1".into()), package_with(&lid, &[]));
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]));
        reg.record(&lid, Some("0.10.0".into()), package_with(&lid, &[]));

        let list = reg.versions(&lid);
        let seen: Vec<&str> = list.versions.iter().map(|v| &*v.version).collect();
        assert_eq!(seen, vec!["0.10.0", "0.8.1", "0.7.9"]);
    }

    #[test]
    fn newest_generation_becomes_current_while_unpinned() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");

        // First generation always establishes the designation.
        assert!(
            reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]))
                .is_some()
        );
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.7.9");

        // A newer one takes it, and the corpus must be repointed.
        assert!(
            reg.record(&lid, Some("0.8.1".into()), package_with(&lid, &[]))
                .is_some()
        );
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.8.1");

        // An older late arrival does not, and needs no corpus write.
        assert!(
            reg.record(&lid, Some("0.5.0".into()), package_with(&lid, &[]))
                .is_none()
        );
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.8.1");
    }

    #[test]
    fn explicit_selection_survives_a_later_newer_generation() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]));
        reg.record(&lid, Some("0.8.1".into()), package_with(&lid, &[]));

        assert!(reg.set_current(&lid, "0.7.9").is_some());
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.7.9");

        // A newer generation lands afterwards; the pin must hold, and no
        // corpus repoint may be requested.
        assert!(
            reg.record(&lid, Some("0.9.0".into()), package_with(&lid, &[]))
                .is_none()
        );
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.7.9");
        // ...but the new generation is still listed.
        assert_eq!(reg.versions(&lid).len(), 3);
    }

    #[test]
    fn selecting_an_absent_version_changes_nothing() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]));

        assert!(reg.set_current(&lid, "9.9.9").is_none());
        assert_eq!(&*reg.versions(&lid).current().unwrap().version, "0.7.9");
    }

    #[test]
    fn rebuilding_the_current_version_replaces_it_and_repoints() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]));

        // Same version string, freshly produced view: one generation, not two,
        // and the corpus is holding a stale Arc so it must be repointed.
        assert!(
            reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]))
                .is_some()
        );
        assert_eq!(reg.versions(&lid).len(), 1);
    }

    #[test]
    fn absent_version_string_is_recorded_as_unversioned() {
        let reg = VersionRegistry::new();
        let lid = lineage("mystery");
        reg.record(&lid, None, package_with(&lid, &[]));

        let list = reg.versions(&lid);
        assert_eq!(list.len(), 1);
        assert_eq!(&*list.versions[0].version, UNVERSIONED);
    }

    #[test]
    fn unknown_lineage_yields_an_empty_list_not_an_error() {
        let reg = VersionRegistry::new();
        let list = reg.versions(&lineage("never-loaded"));
        assert!(list.is_empty());
        assert!(list.current().is_none());
    }

    #[test]
    fn slices_are_oldest_first_and_mark_the_current_generation() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[]));
        reg.record(&lid, Some("0.8.1".into()), package_with(&lid, &[]));

        let slices = reg.slices(&lid);
        let seen: Vec<&str> = slices.iter().map(|s| &*s.version).collect();
        assert_eq!(seen, vec!["0.7.9", "0.8.1"]);
        assert!(!slices[0].is_current);
        assert!(slices[1].is_current);
    }

    #[test]
    fn symbol_count_is_per_generation() {
        let reg = VersionRegistry::new();
        let lid = lineage("axum");
        reg.record(&lid, Some("0.7.9".into()), package_with(&lid, &[(1, "a")]));
        reg.record(
            &lid,
            Some("0.8.1".into()),
            package_with(&lid, &[(1, "a"), (2, "b")]),
        );

        let list = reg.versions(&lid);
        // Newest first.
        assert_eq!(list.versions[0].symbol_count, 2);
        assert_eq!(list.versions[1].symbol_count, 1);
    }
}
