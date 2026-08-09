//! Sealing — the three-phase pass that assigns every arena entry its
//! content-addressed [`IntroId`] and materializes a [`PristineIntroTable`].
//!
//! Build-time entries are addressed by arena-local [`EntryIndex`]; sealing
//! lowers that to the durable, cross-generation [`IntroId`] identity by hashing
//! each declaration's `(package, kind, ancestor-path, leaf-name,
//! disambiguator)`. The disambiguator is selected per §4.3 and — unlike
//! `workspace/ir` — its skeleton payload includes generics/wheres/negativity,
//! so distinct overloads and impls never collide.
//!
//! `seal` consumes the package: sealing moves each [`Entry`] into the table
//! (nudox-ir's `Entry` is intentionally not `Clone`), which is the natural
//! once-per-generation lifecycle.
//!
//! # Three-phase structure
//!
//! The fix for the P-A identity gap (bare same-package nominals colliding)
//! requires knowing each entry's [`IntroId`] *before* we build skeletons that
//! reference other entries by local index. This is solved by one additional
//! pre-pass:
//!
//! 1. **Path ids** — mint a `path_id` for every entry using
//!    `Disambiguator::None`. This depends only on `(kind, ancestor-path,
//!    leaf-name)` — no refs, no disambiguation payload — so it is
//!    ref-independent and declaration-order-independent. For a unique entry its
//!    `path_id` *is* its final `IntroId`; for colliding entries it is a
//!    temporary key used only as a resolver inside the skeleton encoder.
//!
//! 2. **Disambiguate** — count collisions and build skeletons exactly as
//!    before, except the [`Skeleton`] encoder resolves `Ref::Local(idx)` to the
//!    entry's `path_id` via the map from phase 1. The resolved local and its
//!    final intro encode byte-identically (`0x01` + 32 bytes), so skeletons are
//!    stable across sealing.
//!
//! 3. **Mint + lower** — mint the final `IntroId` per entry with its real
//!    disambiguator, then `visit_mut`-lower every `Local` ref to `Intro` and
//!    insert into the [`PristineIntroTable`], exactly as before.
//!
//! # Termination argument
//!
//! Phase 1 reads only `(kind, ancestor-path, leaf-name)` — no skeleton, no
//! refs. Phase 2 reads only the phase-1 output (the `path_ids` map). There
//! is no backward edge; the computation terminates by construction.

use std::{cell::RefCell, collections::HashMap, hash::Hash};

use triomphe::Arc;

use crate::{
    apply::{IntroCollision, PristineIntroTable},
    change::{IntroId, PackageLineageId, StableRef},
    entry::{Entry, EntryInner},
    foreign::{ForeignKey, ForeignResolver, Resolution},
    index::{Ref, UntypedEntryIndex},
    intro::{Disambiguator, bootstrap_intro_id},
    kind::{Kind, KindDiscriminant},
    kinds::{Param, Type},
    skeleton::Skeleton,
    visitor::Visitor,
};

use super::IrPackage;

/// The base collision key: entries sharing `(kind, ancestor-path, leaf-name)`
/// need a disambiguator to stay distinct.
type BaseKey = (u16, Vec<String>, String);

/// A sealed package plus everything `seal` observed while sealing it.
///
/// The report is a **returned field**, not a log line, because a `warn!` stops
/// no caller — and the whole class of defect this type exists to surface is
/// "the pipeline silently did the wrong thing and shipped green".
#[derive(Debug)]
pub struct SealOutcome {
    /// The materialized table.
    pub table: PristineIntroTable,
    /// What sealing observed. See [`SealReport`].
    pub report: SealReport,
}

/// Facts a caller must be able to see after sealing.
///
/// None of these is an error: unloaded dependencies are the normal case, and a
/// forced disambiguation is a producer-quality signal rather than a failure.
/// `seal` stays infallible on purpose — a package that cannot be sealed cannot
/// be *shown*, and degrading to "this reference is named but not linked" is
/// strictly better for a documentation product than degrading to a blank page.
#[derive(Debug, Default)]
pub struct SealReport {
    /// Distinct cross-package keys no resolver could place, with the reason.
    ///
    /// Split by [`Resolution`] rather than collapsed to a count so that
    /// "the dependency is not loaded" (normal) is distinguishable from
    /// "the path names nothing in a package that *is* loaded" (a producer bug —
    /// the referring producer's path grammar and the target's disagree). The
    /// second is the only interesting one and the only one worth chasing.
    pub unlinked: Vec<(Arc<ForeignKey>, Resolution)>,

    /// Cross-package keys that were successfully linked to a `StableRef`.
    pub linked: Vec<(Arc<ForeignKey>, StableRef)>,

    /// Entries whose structural disambiguator proved degenerate and had to be
    /// escalated. Non-empty means the producer erased something load-bearing —
    /// typically parameter types lowered to `Type::Any`, which makes distinct
    /// overloads encode to identical skeletons.
    pub forced: Vec<ForcedDisambiguation>,

    /// The minted `IntroId` of every declaration that needed an escalated
    /// disambiguator, with the tier it landed on. Sorted by `IntroId`.
    ///
    /// # Why this is not derivable from `forced`
    ///
    /// [`SealReport::forced`] counts *groups*, keyed on
    /// `(kind, ancestor-path, leaf-name)`. That is the right shape for
    /// "which producer erased something load-bearing", and it is the wrong
    /// shape for the only question a *consumer* of a key can ask: **"is the
    /// key I am holding fragile?"** A consumer has an `IntroId` and nothing
    /// else — it cannot reconstruct the ancestor path that minted it, because
    /// the id is a hash.
    ///
    /// So this field names the declarations, not the groups. It is the half
    /// that has to survive the `nudox-store` boundary for
    /// `nudox_store::package::PackageView::key_tier` to answer anything, and
    /// the reason a stale key was previously indistinguishable from a deleted
    /// symbol: MCP's `select_version` could disclose that *some* keys churn
    /// and never which ones.
    ///
    /// An `IntroId` **absent** from this list was minted at
    /// [`KeyTier::Structural`] — from the declaration's own content — and is
    /// as stable as that content. Absence is meaningful, which is why this is
    /// the escalated set rather than a row per declaration: a 30k-entry
    /// package would otherwise pay a 30k-row map to record that nothing
    /// happened.
    pub forced_keys: Vec<(IntroId, Escalation)>,

    /// Arena-local references that had no minted `IntroId` and therefore
    /// survived sealing as `Ref::Local`.
    ///
    /// Since the import arena no longer exists, this is provably a bug in
    /// `Lowering`/`seal` rather than a cross-package reference — the case three
    /// separate downstream files each documented as impossible while it shipped
    /// on every real package. Non-empty is always a defect.
    pub unmapped_local: Vec<UntypedEntryIndex>,

    /// Declarations that still collided after every escalation tier and were
    /// therefore **not** inserted. Always a defect; empty in every case the
    /// escalation ladder can reach.
    pub collisions: Vec<IntroCollision>,
}

impl SealReport {
    /// `true` when sealing observed nothing that needs a human.
    ///
    /// Deliberately excludes `unlinked`: a package sealed without its
    /// dependencies is the normal local-first case, not a problem.
    pub fn is_clean(&self) -> bool {
        self.forced.is_empty() && self.unmapped_local.is_empty() && self.collisions.is_empty()
    }
}

/// One declaration group whose structural identity was degenerate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedDisambiguation {
    pub kind: KindDiscriminant,
    pub segments: Vec<String>,
    pub name: String,
    /// How many declarations shared one minted `IntroId` before escalation.
    pub group: usize,
    /// The tier the group had to be escalated to.
    pub escalated_to: Escalation,
}

/// Which fallback tier a colliding group needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escalation {
    /// The declaration's source span separated the group.
    Span,
    /// Spans were identical too (a line-granular oracle, or several
    /// declarations on one line); the ordinal is the terminal guard.
    Ordinal,
}

/// Which disambiguator tier minted a declaration's [`IntroId`], and therefore
/// how much a caller may trust the key to survive the next release.
///
/// # Why a caller needs this and a `bool` will not do
///
/// A key that stops resolving after a version switch is either a *deleted
/// symbol* or a *churned key*, and until this type existed a caller could not
/// tell which — MCP's `select_version` disclosed that the ambiguity exists,
/// which is honest and useless. The three tiers are not "more or less stable"
/// on one axis; they fail for different reasons and a caller reacts to each
/// differently:
///
/// * [`KeyTier::Structural`] gone ⇒ almost certainly deleted or its signature
///   changed. Re-search by name.
/// * [`KeyTier::Span`] gone ⇒ an edit *above* the declaration may have moved
///   it. Re-search by name and path; the symbol is probably still there.
/// * [`KeyTier::Ordinal`] gone ⇒ the producer may simply have emitted the
///   overload set in a different order. Nothing about the source need have
///   changed at all.
///
/// # Deliberately exhaustive
///
/// Per doctrine §3, and for the reason [`Disambiguator`] itself is: adding a
/// tier must break every reader's `match` so each one decides what the new
/// tier means for staleness. A `_` arm here would let a future, less stable
/// tier be reported as if it were `Structural`.
///
/// [`Disambiguator`]: crate::intro::Disambiguator
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyTier {
    /// Minted from the declaration's own content — `Disambiguator::None`,
    /// `FnOverload`, or `TraitImpl`.
    Structural,
    /// Escalated to [`Disambiguator::Span`](crate::intro::Disambiguator::Span):
    /// the key includes byte offsets, so an unrelated edit above the
    /// declaration moves it.
    Span,
    /// Escalated to
    /// [`Disambiguator::Ordinal`](crate::intro::Disambiguator::Ordinal): the
    /// key includes the declaration's index among its colliding siblings, so a
    /// producer reordering its output moves it.
    Ordinal,
}

impl KeyTier {
    /// Whether the key is a function of the declaration's own content alone.
    ///
    /// This is the single question a cache wants answered before it stores a
    /// key across a version boundary. `false` does **not** mean the key *will*
    /// change — most `Span` keys survive most releases — it means the key can
    /// change without the declaration changing, which is precisely the case a
    /// caller cannot detect after the fact.
    pub fn is_content_derived(self) -> bool {
        match self {
            KeyTier::Structural => true,
            KeyTier::Span | KeyTier::Ordinal => false,
        }
    }
}

impl From<Escalation> for KeyTier {
    fn from(e: Escalation) -> Self {
        match e {
            Escalation::Span => KeyTier::Span,
            Escalation::Ordinal => KeyTier::Ordinal,
        }
    }
}

impl<Id: Eq + Hash> IrPackage<Id> {
    /// Seal this package under `lineage`, minting an [`IntroId`] for every
    /// entry and returning the materialized [`PristineIntroTable`].
    /// Consumes `self`.
    ///
    /// `lineage` is the production package identity (ecosystem + name); it is a
    /// parameter for now — the arena's internal `PackageId` is dev-only and
    /// will be replaced by the lineage id when the registry is rewired.
    ///
    /// `imports` places this package's cross-package references. It is a
    /// **required** parameter, not a defaulted one, because an omission is
    /// exactly how the original defect stayed invisible: pass
    /// [`Unlinked`](crate::foreign::Unlinked) to state "nothing has been sealed
    /// alongside this package", and the decision is on the record at the call
    /// site rather than absent from it.
    pub fn seal(
        self,
        lineage: &PackageLineageId,
        imports: &dyn ForeignResolver,
    ) -> SealOutcome {
        // Resolution indices over the arena's export-addressed entries.
        let by_idx: HashMap<UntypedEntryIndex, &Entry> =
            self.entries.iter().map(|(idx, e)| (*idx, e)).collect();
        let pos_of: HashMap<UntypedEntryIndex, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, (idx, _))| (*idx, i))
            .collect();

        let n = self.entries.len();

        // ── Pass 0: ancestor segments, leaf name, kind, parent-idx per entry ──
        let mut segs: Vec<Vec<String>> = Vec::with_capacity(n);
        let mut names: Vec<String> = Vec::with_capacity(n);
        let mut discs: Vec<KindDiscriminant> = Vec::with_capacity(n);
        let mut parent_idxs: Vec<Option<UntypedEntryIndex>> = Vec::with_capacity(n);
        for (_, e) in &self.entries {
            let mut chain = Vec::new();
            let mut cur = e.parent().and_then(|r| r.as_local());
            while let Some(pidx) = cur {
                match by_idx.get(&pidx) {
                    Some(pe) => {
                        chain.push(pe.sym().name.clone());
                        cur = pe.parent().and_then(|r| r.as_local());
                    }
                    None => break,
                }
            }
            chain.reverse();
            segs.push(chain);
            names.push(e.sym().name.clone());
            discs.push(match e.kind() {
                EntryInner::Owned(k) => k.discriminant(),
                // A within-arena forwarding alias is a re-export.
                EntryInner::Reference(_) => KindDiscriminant::Reexport,
            });
            parent_idxs.push(e.parent().and_then(|r| r.as_local()));
        }

        // ── Phase A: path ids — one `IntroId` per entry, ref-free ────────────
        // Mint with `Disambiguator::None` for every entry.  This depends only
        // on `(kind, ancestor-path, leaf-name)` — no refs, no skeleton — so it
        // is declaration-order-independent.  For a unique entry this *is* its
        // final IntroId; for colliding entries it is used purely as a resolver
        // inside the skeleton encoder in Pass 2.
        let path_ids: HashMap<UntypedEntryIndex, IntroId> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, (idx, _))| {
                let seg_refs: Vec<&str> = segs[i].iter().map(String::as_str).collect();
                let id = bootstrap_intro_id(
                    lineage,
                    discs[i],
                    &seg_refs,
                    &names[i],
                    &Disambiguator::None,
                );
                (*idx, id)
            })
            .collect();

        // ── Pass 1: count collisions on the base key ──────────────────────────
        let mut counts: HashMap<BaseKey, u32> = HashMap::new();
        for i in 0..n {
            let key = (discs[i].as_u16(), segs[i].clone(), names[i].clone());
            *counts.entry(key).or_insert(0) += 1;
        }

        // ── Pass 2: select disambiguator + mint final IntroId per entry ───────
        // Skeletons resolve `Ref::Local(idx)` via `path_ids`; a resolved local
        // encodes byte-identically to `Ref::Intro(path_id)`.
        let resolver = |idx: UntypedEntryIndex| path_ids.get(&idx).copied();

        let mut report = SealReport::default();
        let mut intros: Vec<IntroId> = Vec::with_capacity(n);
        for i in 0..n {
            let (_, e) = &self.entries[i];
            let count = *counts
                .get(&(discs[i].as_u16(), segs[i].clone(), names[i].clone()))
                .unwrap_or(&1);

            let disamb = match e.kind() {
                // ≥2 functions at the same key: the full signature skeleton.
                EntryInner::Owned(Kind::Function(f)) if count >= 2 => {
                    let inputs = resolve_param_tys(&f.input_params, &by_idx);
                    let outputs = resolve_param_tys(&f.output_params, &by_idx);
                    let skel = Skeleton::new(&resolver).signature(
                        &inputs,
                        &outputs,
                        &f.generics,
                        &f.wheres,
                    );
                    Disambiguator::FnOverload(skel.into_boxed_slice())
                }
                // Every impl: the (trait, self, generics, wheres, negative, blanket)
                // skeleton — unconditionally, since impls share the `"impl"` name.
                EntryInner::Owned(Kind::Impl(im)) => {
                    let skel = Skeleton::new(&resolver).trait_impl(
                        im.of.as_ref(),
                        &im.self_ty,
                        &im.generics,
                        &im.wheres,
                        im.flags.negative,
                        im.flags.blanket,
                    );
                    Disambiguator::TraitImpl(skel.into_boxed_slice())
                }
                // Any other same-key collision: fall back to the source span.
                _ if count >= 2 => {
                    let span = &e.sym().span;
                    Disambiguator::Span {
                        start: span.start,
                        end: span.end,
                    }
                }
                // Unique: signature-stable None.
                _ => Disambiguator::None,
            };

            let seg_refs: Vec<&str> = segs[i].iter().map(String::as_str).collect();
            intros.push(bootstrap_intro_id(
                lineage, discs[i], &seg_refs, &names[i], &disamb,
            ));
        }

        // ── Pass 2.5: escalate any group that *actually* minted one id ───────
        //
        // Two declarations minting one `IntroId` is an identity failure, not an
        // update: the second is unreachable and the first is gone. That is 28
        // real Gson methods disappearing between `finish` and the GUI.
        //
        // The structural disambiguators above can be degenerate. A `Function`
        // skeleton over parameters that all erased to `Type::Any` carries no
        // discriminating bytes — and, unlike *every other* collision (the
        // `_ if count >= 2` arm), that branch has no fallthrough to `Span`.
        //
        // Escalating **here**, over the ids that were actually minted, is
        // strictly better than adding a blanket `Span` fallthrough to the
        // Function branch: a fallthrough would change the id of every
        // legitimately-distinguished overload set in the corpus for no benefit,
        // while this touches only entries that genuinely collided.
        //
        // Whole groups are re-minted, never just the losers. Keeping the first
        // member at its old id would make identity declaration-order dependent,
        // which this module's contract forbids.
        {
            // 0 = structural (as minted above), 1 = Span, 2 = Ordinal.
            let mut tier: Vec<u8> = vec![0; n];
            // Termination: each round strictly raises the tier of every
            // colliding member, and tier 2 makes preimages distinct within a
            // group by construction (the ordinal differs). The bound guards
            // against a genuine blake3 collision rather than a design gap; it
            // is reported, never silently ignored.
            const MAX_ROUNDS: usize = 8;
            // One report row per colliding *group*, carrying the tier it
            // finally needed — not one row per escalation round. A group whose
            // spans are also identical passes through `Span` on its way to
            // `Ordinal`, and reporting that intermediate step as a separate
            // finding would make the report count escalations instead of
            // defects.
            let mut forced_by_group: HashMap<BaseKey, ForcedDisambiguation> = HashMap::new();
            for _ in 0..MAX_ROUNDS {
                let mut groups: HashMap<IntroId, Vec<usize>> = HashMap::new();
                for (i, id) in intros.iter().enumerate() {
                    groups.entry(*id).or_default().push(i);
                }
                // Deterministic order: group by first member's arena position.
                let mut colliding: Vec<Vec<usize>> =
                    groups.into_values().filter(|m| m.len() >= 2).collect();
                if colliding.is_empty() {
                    break;
                }
                colliding.sort_by_key(|m| m[0]);

                for members in colliding {
                    let next_tier = members.iter().map(|&i| tier[i]).max().unwrap_or(0) + 1;
                    let escalated_to = if next_tier == 1 {
                        Escalation::Span
                    } else {
                        Escalation::Ordinal
                    };
                    let head = members[0];
                    forced_by_group.insert(
                        (discs[head].as_u16(), segs[head].clone(), names[head].clone()),
                        ForcedDisambiguation {
                            kind: discs[head],
                            segments: segs[head].clone(),
                            name: names[head].clone(),
                            group: members.len(),
                            escalated_to,
                        },
                    );

                    for (ordinal, &i) in members.iter().enumerate() {
                        tier[i] = next_tier;
                        let span = &self.entries[i].1.sym().span;
                        let disamb = if next_tier == 1 {
                            Disambiguator::Span {
                                start: span.start,
                                end: span.end,
                            }
                        } else {
                            Disambiguator::Ordinal {
                                span_start: span.start,
                                span_end: span.end,
                                index: ordinal as u32,
                            }
                        };
                        let seg_refs: Vec<&str> =
                            segs[i].iter().map(String::as_str).collect();
                        intros[i] = bootstrap_intro_id(
                            lineage, discs[i], &seg_refs, &names[i], &disamb,
                        );
                    }
                }
            }
            // Per-declaration tiers, derived from the same `tier` vector the
            // loop above maintained — never accumulated alongside it. Doctrine
            // §8: "the count is derived from the repaired values, never
            // accumulated alongside them"; a parallel tally can drift from the
            // ids it describes, and this one is read to decide whether a
            // caller's key is trustworthy.
            report.forced_keys = (0..n)
                .filter_map(|i| match tier[i] {
                    0 => None,
                    1 => Some((intros[i], Escalation::Span)),
                    // Tier 2 is `Ordinal`; the loop above never assigns a
                    // higher one, because `next_tier` is capped by the two
                    // `Escalation` variants and `MAX_ROUNDS` bounds the walk.
                    _ => Some((intros[i], Escalation::Ordinal)),
                })
                .collect();
            // By `IntroId` alone. The tier is not an ordering axis — sorting
            // on it would group the report by severity and make two runs of
            // the same package incomparable line-by-line, which is the one
            // thing a report of this kind has to support.
            report.forced_keys.sort_unstable_by_key(|(intro, _)| *intro);

            report.forced = forced_by_group.into_values().collect();
            // Deterministic reporting order: a report that reorders between
            // runs is not comparable across generations.
            report
                .forced
                .sort_by(|a, b| (a.kind.as_u16(), &a.segments, &a.name).cmp(&(
                    b.kind.as_u16(),
                    &b.segments,
                    &b.name,
                )));
        }

        // Resolve parent IntroIds from the captured parent indices (owned data —
        // no borrow of `self.entries`, so pass 3 may consume it).
        let parents: Vec<Option<IntroId>> = parent_idxs
            .iter()
            .map(|opt| opt.and_then(|pidx| pos_of.get(&pidx)).map(|&pi| intros[pi]))
            .collect();

        // Every arena index → its minted IntroId, for lowering in-body refs.
        let intro_of: HashMap<UntypedEntryIndex, IntroId> =
            pos_of.iter().map(|(idx, &i)| (*idx, intros[i])).collect();

        // `by_idx` is the only binding that borrows `self.entries`; end it here
        // so pass 3 can consume them. (`path_ids`/`resolver` own their data and
        // simply fall out of scope.)
        drop(by_idx);

        // ── Pass 3: lower every in-body `Local` ref → `Intro`, then move in ───
        // One `visit_mut` per entry rewrites EVERY reference at once — kind-body
        // refs (fields/params/variants), `Type` nominals, and the `Node` tree
        // edges — making the table fully content-addressed (self-contained).
        //
        // Cross-package refs are *linked* here rather than rewritten: their key
        // is already complete, so this only fills in the resolved target. The
        // resolver is consulted once per **distinct key**, not once per
        // reference — memchr names `core::clone::Clone` 147 times.
        //
        // `visit_mut` takes a plain `&impl Fn`, so the cache and the report need
        // interior mutability. A `FnMut` visitor would make this a compile-time
        // invariant instead of a runtime one; widening it touches the derive
        // macro and is deliberately left as separate work.
        let cache: RefCell<HashMap<Arc<ForeignKey>, Resolution>> = RefCell::new(HashMap::new());
        let unmapped: RefCell<Vec<UntypedEntryIndex>> = RefCell::new(Vec::new());

        let mut table = PristineIntroTable::new();
        let mut pending: Vec<(IntroId, Entry, Option<IntroId>)> = Vec::with_capacity(n);
        for (i, (_, mut entry)) in self.entries.into_iter().enumerate() {
            entry.visit_mut(&|r| match r {
                Ref::Local(idx) => match intro_of.get(idx) {
                    Some(&intro) => *r = Ref::Intro(intro),
                    // Every export index is in `intro_of` by construction — it
                    // is built from the same `self.entries`. A miss is a
                    // `Lowering`/`seal` bug, and since the import arena is gone
                    // it can no longer be a cross-package reference in disguise.
                    None => unmapped.borrow_mut().push(*idx),
                },
                Ref::Foreign { key, target } => {
                    if target.is_none() {
                        let mut cache = cache.borrow_mut();
                        let resolution = cache
                            .entry(key.clone())
                            .or_insert_with(|| imports.resolve(key))
                            .clone();
                        if let Resolution::Resolved(sr) = resolution {
                            *target = Some(sr);
                        }
                    }
                }
                Ref::Intro(_) => {}
            });
            pending.push((intros[i], entry, parents[i]));
        }

        report.unmapped_local = unmapped.into_inner();
        for (key, resolution) in cache.into_inner() {
            match resolution {
                Resolution::Resolved(sr) => report.linked.push((key, sr)),
                other => report.unlinked.push((key, other)),
            }
        }
        // Deterministic reporting order — a report that reorders between runs
        // is not comparable across generations.
        report.linked.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        report.unlinked.sort_by(|a, b| a.0.path.cmp(&b.0.path));

        for (intro, entry, parent) in pending {
            // Pass 2.5 escalates until every minted id is distinct, so this
            // cannot fail by design. It is `try_` rather than the panicking
            // form because "cannot fail by design" is exactly the claim the
            // previous version of this code made and got wrong — a residual
            // collision is recorded and the package still loads, minus the one
            // declaration, which is what shipped before but is now *visible*.
            if let Err(collision) = table.try_insert_live(intro, entry, parent) {
                report.collisions.push(collision);
            }
        }

        SealOutcome { table, report }
    }
}

/// Resolve each param handle to its declared type (if any) for the signature
/// skeleton. An unresolvable or type-less param contributes `None`.
fn resolve_param_tys(
    params: &[Ref<Param>],
    by_idx: &HashMap<UntypedEntryIndex, &Entry>,
) -> Vec<Option<Type>> {
    params
        .iter()
        .map(|pref| {
            pref.as_local()
                .and_then(|idx| by_idx.get(&idx.raw()))
                .and_then(|e| match e.kind() {
                    EntryInner::Owned(Kind::Param(p)) => p.ty.clone(),
                    _ => None,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{
        build::*,
        foreign::Unlinked,
        change::{EcosystemId, PackageName},
        entry::EntryInner,
        kind::Kind,
        test_helpers::{id_gen, sym},
    };

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"))
    }

    /// End-to-end: seal a package and confirm every entry is materialized with
    /// a parent edge that matches the tree.
    #[test]
    fn seal_materializes_the_tree() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            root.create(id(), sym("Point"), |mut rec| {
                let x = rec.create(id(), sym("x"), |_| {
                    Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                });
                Record::builder().fields([x]).build()
            });
        });

        let table = pkg.seal(&lineage(), &Unlinked).table;
        // root module + Point record + x field = 3 entries.
        assert_eq!(table.len(), 3);
        let roots = table
            .iter()
            .filter(|(id, _)| table.parent_of(*id).is_none())
            .count();
        assert_eq!(roots, 1, "exactly one root (the module)");
    }

    /// The collision fix, end-to-end: two overloaded functions differing only
    /// by a generic bound must seal to DISTINCT IntroIds (they would
    /// collide under workspace/ir's skeleton).
    #[test]
    fn overloads_seal_to_distinct_intros() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            for bound in [Type::Any, Type::Never] {
                root.create(id(), sym("f"), |_| {
                    Function::builder()
                        .generics([GenericParam::Type {
                            name: "T".to_owned(),
                            bounds: [bound].into(),
                            default: None,
                            variance: None,
                        }])
                        .build()
                });
            }
        });

        let table = pkg.seal(&lineage(), &Unlinked).table;
        // 1 module + 2 functions = 3 unique intros (no collision).
        assert_eq!(table.len(), 3, "no IntroId collision among the overloads");
    }

    /// A unique-named function seals with the signature-stable `None`
    /// disambiguator, so its id is stable across a param-type change.
    #[test]
    fn unique_fn_id_is_signature_stable() {
        let mint = |ret: Type| {
            let mut id = id_gen();
            let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
                root.create(id(), sym("solo"), |mut f| {
                    let p = f.create(id(), sym("out"), |_| Param::builder().ty(ret).build());
                    Function::builder().output_params([p]).build()
                });
            });
            let table = pkg.seal(&lineage(), &Unlinked).table;
            table
                .iter()
                .find(|(_, e)| e.sym().name == "solo")
                .map(|(i, _)| i)
                .unwrap()
        };
        assert_eq!(
            mint(Type::I32),
            mint(Type::I64),
            "a unique-named fn keeps its IntroId under a return-type change"
        );
    }

    /// Phase-1 proof: after seal, a record's field reference is LOWERED from an
    /// arena-local index to a content-addressed `Ref::Intro` — the table is
    /// self-contained (one `visit_mut` rewrote every ref).
    #[test]
    fn seal_lowers_in_body_refs_to_intro() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            root.create(id(), sym("Point"), |mut rec| {
                let x = rec.create(id(), sym("x"), |_| {
                    Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                });
                Record::builder().fields([x]).build()
            });
        });

        let table = pkg.seal(&lineage(), &Unlinked).table;
        let (_, rec) = table.iter().find(|(_, e)| e.sym().name == "Point").unwrap();

        match rec.kind() {
            EntryInner::Owned(Kind::Record(r)) => {
                assert_eq!(r.fields.len(), 1);
                match &r.fields[0] {
                    Ref::Intro(fi) => {
                        assert!(table.contains(*fi), "field intro resolves in the table");
                        assert_eq!(table.get(*fi).unwrap().sym().name, "x");
                    }
                    other => panic!("field ref must be lowered to Intro, got {other:?}"),
                }
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    /// P-A phase gate: two impls whose `self_ty` is a bare local nominal (`Bar`
    /// and `Baz` respectively, no generic args) must seal to DISTINCT IntroIds.
    ///
    /// Before the path-id pre-pass both impls produced identical skeletons
    /// (local refs encoded as the same `0x00` placeholder) and therefore
    /// collided onto one IntroId — a silent overwrite in `PristineIntroTable`.
    #[test]
    fn bare_nominal_impls_seal_to_distinct_intros() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            // Declare Bar and Baz as records.
            let bar_ref = root.create(id(), sym("Bar"), |_| Record::builder().build());
            let baz_ref = root.create(id(), sym("Baz"), |_| Record::builder().build());

            // Two impls: `impl Bar` and `impl Baz` (bare self-type, no generics).
            root.create(id(), sym("impl"), |_| {
                Impl::builder()
                    .self_ty(Type::Nominal(bar_ref.into_raw()))
                    .build()
            });
            root.create(id(), sym("impl"), |_| {
                Impl::builder()
                    .self_ty(Type::Nominal(baz_ref.into_raw()))
                    .build()
            });
        });

        let table = pkg.seal(&lineage(), &Unlinked).table;
        // 1 module + Bar + Baz + 2 impls = 5 unique intros.
        assert_eq!(
            table.len(),
            5,
            "both impls must get distinct IntroIds (no silent overwrite)"
        );
    }

    /// Determinism guard: sealing two identically-built packages yields
    /// identical sets of IntroIds.
    #[test]
    fn seal_is_deterministic() {
        let build_pkg = || {
            let mut id = id_gen();
            IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
                let bar_ref = root.create(id(), sym("Bar"), |_| Record::builder().build());
                let baz_ref = root.create(id(), sym("Baz"), |_| Record::builder().build());
                root.create(id(), sym("impl"), |_| {
                    Impl::builder()
                        .self_ty(Type::Nominal(bar_ref.into_raw()))
                        .build()
                });
                root.create(id(), sym("impl"), |_| {
                    Impl::builder()
                        .self_ty(Type::Nominal(baz_ref.into_raw()))
                        .build()
                });
            })
        };

        let mut ids_a: Vec<IntroId> = build_pkg()
            .seal(&lineage(), &Unlinked).table
            .iter()
            .map(|(i, _)| i)
            .collect();
        let mut ids_b: Vec<IntroId> = build_pkg()
            .seal(&lineage(), &Unlinked).table
            .iter()
            .map(|(i, _)| i)
            .collect();
        ids_a.sort();
        ids_b.sort();
        assert_eq!(ids_a, ids_b, "sealing is deterministic");
    }
}
