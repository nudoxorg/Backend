//! Decode the cage producer's NdIrF1 IR stream and derive the reference set.

use std::collections::HashMap;

use crate::server::registry::blob::creation::BlobBuilder;

/// Decode the cage producer's IR stream bytes, stage them onto `builder`, and
/// return the symbol identifier list for the emit/facets path.
///
/// # Wire format
///
/// The producer writes a postcard-framed `ir-stream` protocol (SMOLVM-PLAN
/// §6.1/§6.2) onto its stdout. Each frame is `[u32 LE length][postcard bytes]`.
/// [`ir_vcs::protocol::StreamReceiver`] handles framing, ordering enforcement,
/// and emitted-count validation.
///
/// # Sections staged onto `builder`
///
/// - **IR section** (`builder.set_ir`): postcard-serialized
///   `Vec<ir_vcs::wire::OwnedEntryPayload>` — one entry per `Symbols` batch entry,
///   in stream order. This is what the IR-plane apply/checkout machinery reads.
/// - **References section** (`builder.set_references`): a [`ReferenceSet`]
///   derived from the `Bodies` frames in the stream (INDEX-PLAN §5.1).
///
/// # References derivation (INDEX-PLAN §5.1)
///
/// The `Occurrences` frames carry opaque producer-specific bytes with no
/// public decoder in `ir` or `ir-vcs` (the format is intentionally producer-
/// private and the host records them opaquely — see `stream::StreamedRecording`).
/// The `Bodies` frames, however, carry fully decoded [`ir::body::BodyEmbed`] values
/// with oracle-resolved call targets ([`ir::OracleCall`]) and type mentions
/// ([`ir::OracleTypeMention`]) — both carry a [`ir::change::StableRef`] that
/// identifies the target symbol across packages.
///
/// Policy (documented here, not a §5.1 blocker):
/// - We extract *oracle-resolved* references only (`Confidence >= Index`),
///   since tree-sitter call-name facts have no stable cross-package target.
/// - The `Reference::span_start`/`span_end` are the `RelSpan` bounds relative
///   to the owning entry's declaration span start (not absolute file offsets).
///   This is correct for the use case: the reference store holds relative
///   spans and the caller reconstructs absolute ones from the symbol's
///   `span_start` when needed.
/// - `RefTarget`: a cross-package target (`StableRef.package` ≠ owning entry's
///   package name) maps to `RefTarget::External { path: intro_hex, dependency:
///   "ecosystem:name" }`; a same-package target maps to
///   `RefTarget::Local(intro_hex)`. Using `intro_hex` as the `path` is
///   intentional: we don't have source file paths for target symbols (only for
///   source symbols from `SymbolWire::source_path`); the `intro_hex` is the
///   durable, wire-stable identity that the IR-plane apply/checkout machinery
///   resolves to a symbol path on demand.
///
/// # Aborted / truncated / undecodable streams (W1)
///
/// Only a `Finish` frame is a legitimate end of stream — a well-behaved
/// producer sends one even for a genuinely empty package (see
/// [`ir_vcs::protocol::StreamFrame`]'s ordering rules). Every other way the
/// stream can end is a producer/host breakage and is recorded in the
/// returned [`IrIngestOutcome::degraded_reason`] rather than silently
/// treated as success:
///
/// - a missing/malformed `Hello` handshake;
/// - an explicit `Abort` frame;
/// - a clean EOF that arrives *without* a prior `Finish` (a truncated
///   stream — e.g. the cage or the vsock transport died mid-write);
/// - a frame that fails to decode (framing/postcard corruption);
/// - a host-side failure to serialize the recovered IR or reference
///   sections.
///
/// Whatever was recovered before the break is still staged onto `builder`
/// (partial-data capture for diagnostics) — the caller decides whether to
/// use it, but must not report the job as a clean `Stored` success when
/// `degraded_reason` is `Some`.
pub(super) fn ingest_ir_bytes(builder: &mut BlobBuilder, ir_bytes: &[u8]) -> IrIngestOutcome {
    use ir::change::{IntroId, PackageLineageId};
    use ir_vcs::protocol::{BodyWire, Received, StreamReceiver};

    let mut rx = StreamReceiver::new(std::io::Cursor::new(ir_bytes));

    // Handshake: the Hello frame carries the job key and producer id.
    // A missing or malformed Hello means the producer wrote nothing usable.
    if let Err(err) = rx.accept() {
        tracing::warn!(error = %err, "IR stream has no Hello frame; treating as producer breakage");
        attach_empty_ir_sections(builder);
        return IrIngestOutcome {
            identifiers: Vec::new(),
            occurrences: Vec::new(),
            owning_package: None,
            degraded_reason: Some(format!("no Hello frame: {err}")),
        };
    }

    let mut payloads: Vec<ir_vcs::wire::OwnedEntryPayload> = Vec::new();
    let mut identifiers: Vec<String> = Vec::new();
    // IntroId → source_path from the Symbols batches: used to group
    // oracle-derived references by their owning entry's source file.
    let mut intro_to_path: HashMap<IntroId, String> = HashMap::new();
    // Owning package name (ecosystem:name) from the first Symbol — needed to
    // distinguish Local vs External RefTarget. We derive it lazily from the
    // StableRef on the first WireEntry; if no symbols arrive it stays None
    // and all targets default to External (the conservative choice).
    let mut owning_pkg_key: Option<String> = None;
    // The same owning package, kept typed (not just the display string above)
    // so the usage-query scope can be bound to the exact `PackageLineageId`
    // the wire `StableRef`s carry — captured once, from the same first
    // WireEntry, alongside `owning_pkg_key`.
    let mut owning_package: Option<PackageLineageId> = None;
    // Bodies frames: accumulated across all batches for post-loop reference
    // extraction (they can arrive interleaved with Symbols).
    let mut bodies: Vec<BodyWire> = Vec::new();
    // Set only on a genuine producer/host breakage (see doc comment above);
    // `None` all the way through a clean `Finish` means honest success —
    // including a legitimate zero-symbol package.
    let mut degraded_reason: Option<String> = None;

    loop {
        match rx.recv() {
            Ok(Some(Received::Symbols(batch))) => {
                for entry in batch {
                    // Record source_path for this intro for reference grouping.
                    let intro = entry.stable.intro;
                    let src_path = entry.payload.symbol.source_path.clone();
                    intro_to_path.entry(intro).or_insert_with(|| src_path);

                    // Capture the owning package key (ecosystem:name) once.
                    if owning_pkg_key.is_none() {
                        let pkg = &entry.stable.package;
                        owning_pkg_key =
                            Some(format!("{}:{}", pkg.ecosystem.as_str(), pkg.name.as_str()));
                        owning_package = Some(pkg.clone());
                    }

                    identifiers.push(entry.payload.symbol.name.clone());
                    payloads.push(entry.payload);
                }
            }
            Ok(Some(Received::Bodies(batch))) => {
                bodies.extend(batch);
            }
            Ok(Some(Received::Links { .. }))
            | Ok(Some(Received::SourceDigest { .. }))
            | Ok(Some(Received::Progress { .. }))
            | Ok(Some(Received::Occurrences(_))) => {
                // Occurrences bytes are opaque (producer-private format, no public
                // decoder in ir/ir-vcs) — accumulated in stream.rs as raw bytes
                // for provenance; not decoded here. SourceDigest provenance is
                // similarly out of scope for the reference slot.
            }
            Ok(Some(Received::Finish { .. })) => {
                // The only legitimate end of stream — including for a
                // producer that honestly emitted zero symbols.
                break;
            }
            Ok(None) => {
                // Clean EOF *without* ever seeing Finish or Abort: the
                // transport closed mid-protocol. Never a legitimate empty
                // signal — a well-behaved producer always sends Finish.
                let reason = format!(
                    "IR stream ended without a Finish frame ({} identifiers recovered before truncation)",
                    identifiers.len()
                );
                tracing::warn!(reason, "IR stream truncated");
                degraded_reason = Some(reason);
                break;
            }
            Ok(Some(Received::Abort { failure, message })) => {
                let reason = format!("producer aborted ({failure:?}): {message}");
                tracing::warn!(
                    ?failure,
                    message,
                    identifiers = identifiers.len(),
                    "IR stream producer aborted"
                );
                degraded_reason = Some(reason);
                break;
            }
            Err(err) => {
                let reason = format!("IR stream decode error: {err}");
                tracing::warn!(
                    error = %err,
                    identifiers = identifiers.len(),
                    "IR stream decode error"
                );
                degraded_reason = Some(reason);
                break;
            }
        }
    }

    // Serialize the collected (possibly partial) payloads as the IR blob
    // section — preserved for diagnostics even when `degraded_reason` is
    // set; the caller is responsible for failing the job in that case.
    match postcard::to_allocvec(&payloads) {
        Ok(ir_blob) => {
            if let Err(err) = builder.set_ir(bytes::Bytes::from(ir_blob)) {
                tracing::warn!(error = %err, "set_ir failed; attaching empty IR section");
                attach_empty_ir_sections(builder);
                return IrIngestOutcome {
                    identifiers,
                    occurrences: Vec::new(),
                    owning_package: None,
                    degraded_reason: Some(
                        degraded_reason.unwrap_or_else(|| format!("set_ir failed: {err}")),
                    ),
                };
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "IR payload serialization failed; attaching empty IR section");
            attach_empty_ir_sections(builder);
            return IrIngestOutcome {
                identifiers,
                occurrences: Vec::new(),
                owning_package: None,
                degraded_reason: Some(
                    degraded_reason
                        .unwrap_or_else(|| format!("IR payload serialization failed: {err}")),
                ),
            };
        }
    }

    // ── Derive the ReferenceSet from the Bodies frames (INDEX-PLAN §5.1) ──────
    //
    // For each BodyWire, look up the owning entry's source file via
    // `intro_to_path`, then extract oracle-resolved references from
    // `BodyEmbed::Present(BodyFacts)`. Oracle calls and type mentions both
    // carry a StableRef target with Confidence >= Index.
    //
    // Policy: only emit references with a resolved `target` (OracleCall.target
    // = Some) and at confidence >= Index (already implied by the oracle tier,
    // but explicit for clarity). Tree-sitter call-name facts on the
    // `TreesitterBody` side have no cross-package stable target and are
    // skipped (they contribute call-site counts, not cross-reference graph
    // edges, per §5.1 merge rule 3 / Confidence::GRAPH_FLOOR).
    let bodies_ref_set =
        build_reference_set_from_bodies(&bodies, &intro_to_path, owning_pkg_key.as_deref());
    // ── Unresolved cross-package edges from the Symbols batch (C2 fix) ───────
    // `bodies_ref_set` above is blind to an unlinked `Ref::Foreign` or an
    // `UnknownType::UnresolvedExternal` mention — see `build_reference_set_
    // from_payloads`'s doc comment for exactly why and how this closes the
    // host-divergence gap against `build_reference_set_from_table`.
    let payload_ref_set = build_reference_set_from_payloads(&payloads);
    let ref_set = merge_reference_sets(bodies_ref_set, payload_ref_set);
    tracing::debug!(
        by_file = ref_set.by_file.len(),
        total_refs = ref_set
            .by_file
            .iter()
            .map(|f| f.references.len())
            .sum::<usize>(),
        "reference set derived from Bodies frames + unresolved Symbols-batch edges"
    );

    if let Err(err) = builder.set_references(&ref_set) {
        tracing::warn!(error = %err, "set_references failed; attaching empty reference set");
        let empty_refs = crate::server::registry::blob::ReferenceSet {
            by_file: Vec::new(),
        };
        let _ = builder.set_references(&empty_refs);
        // A dropped reference set is a silent loss of real data (the cross-
        // reference graph for this snapshot), not a cosmetic degrade — W1
        // applies here too, unless the stream was already flagged degraded.
        degraded_reason.get_or_insert_with(|| format!("set_references failed: {err}"));
    }

    // ── Derive usage-query occurrences from the same Bodies frames ───────────
    // The same oracle-resolved facts that feed `ref_set` above also carry
    // everything `ir::vocab::Occurrence` needs (owner, target, kind,
    // confidence, span) — `build_occurrences_from_bodies` projects them the
    // same way `build_reference_set_from_bodies` projects them into
    // `Reference`s, just without the by-file grouping. The caller (the Linux
    // compile-phase driver in `indexing/mod.rs`, which holds `stores`) uses
    // these plus `owning_package` to build this package's `IrView` and load it
    // into `SourceStores::usage_backend`. The opaque `Occurrences` stream frame
    // is intentionally not consulted (see the module docs): it has no public
    // decoder and nothing in this codebase writes one.
    let occurrences = build_occurrences_from_bodies(&bodies);

    IrIngestOutcome {
        identifiers,
        occurrences,
        owning_package,
        degraded_reason,
    }
}

/// The result of decoding one producer NdIrF1 stream (W1).
///
/// `degraded_reason` is `None` only when the stream ran cleanly to a
/// `Finish` frame and every recovered section serialized successfully —
/// including the legitimate case of a producer that honestly emits zero
/// symbols. Any other outcome (missing Hello, `Abort`, truncation, decode
/// error, or a host-side (de)serialization failure) sets it to a
/// human-readable reason; the caller must fail the job rather than report a
/// clean `Stored` success in that case.
pub(super) struct IrIngestOutcome {
    pub(super) identifiers: Vec<String>,
    /// Usage-query occurrence facts projected from the stream's `Bodies`
    /// frames (see [`build_occurrences_from_bodies`]) — empty on every
    /// degraded outcome above, alongside a real (possibly partial) list on a
    /// clean `Finish`. Consumed by the caller together with
    /// [`Self::owning_package`] to load this package's usage-query scope.
    pub(super) occurrences: Vec<(ir::change::IntroId, ir::vocab::Occurrence)>,
    /// The package lineage captured from the first `WireEntry`'s `StableRef`
    /// (`None` if the stream broke before any `Symbols` batch arrived, or
    /// legitimately emitted zero symbols). The caller needs this to bind the
    /// usage-query `IrView` to the correct package identity.
    pub(super) owning_package: Option<ir::change::PackageLineageId>,
    pub(super) degraded_reason: Option<String>,
}

/// Derive a [`ReferenceSet`] from the accumulated `Bodies` frames.
///
/// See `ingest_ir_bytes` for the policy rationale. This is a pure function
/// (no I/O) so it can be unit-tested independently.
///
/// # Policy
/// - `OracleCall` with `target: Some(stable_ref)` → `Reference` with
///   `kind = stable_ref.kind as u8`, span from `rel_span`.
/// - `OracleTypeMention` → `Reference` with kind discriminant for
///   `ReferenceKind::TypeReference` (2).
/// - `RefTarget::Local(intro_hex)` when `stable_ref.package` matches
///   `owning_pkg` (same-package reference). Otherwise
///   `RefTarget::External { path: intro_hex, dependency: "eco:name" }`.
/// - References are grouped by the owning entry's `source_path`; entries
///   whose `intro` is not in `intro_to_path` are grouped under the sentinel
///   path `"<unknown>"` (body arrived without a matching Symbol frame).
fn build_reference_set_from_bodies(
    bodies: &[ir_vcs::protocol::BodyWire],
    intro_to_path: &HashMap<ir::change::IntroId, String>,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, Reference, ReferenceSet};
    use ir::body::BodyEmbed;
    use ir::vocab::ReferenceKind;
    use smol_str::SmolStr;

    // Per-file accumulator: file_path → Vec<Reference>.
    let mut by_file: HashMap<SmolStr, Vec<Reference>> = HashMap::new();

    for bw in bodies {
        let src_path = intro_to_path
            .get(&bw.intro)
            .map(|s| SmolStr::from(s.as_str()))
            .unwrap_or_else(|| SmolStr::new("<unknown>"));

        let refs = by_file.entry(src_path).or_default();

        if let BodyEmbed::Present(facts) = &bw.body {
            // Oracle calls: only those with a resolved target.
            for call in &facts.oracle.calls {
                let Some(ref stable) = call.target else {
                    continue;
                };
                // Policy: emit only graph-worthy references (Confidence >= Index).
                if !call.confidence.is_graph_worthy() {
                    continue;
                }
                let target = make_ref_target(stable, owning_pkg);
                refs.push(Reference {
                    target,
                    span_start: call.rel_span.start as u64,
                    span_end: call.rel_span.end as u64,
                    kind: call.kind as u8,
                });
            }

            // Oracle type mentions: all resolved (no target-less form).
            for mention in &facts.oracle.type_mentions {
                let target = make_ref_target(&mention.ty, owning_pkg);
                refs.push(Reference {
                    target,
                    span_start: mention.rel_span.start as u64,
                    span_end: mention.rel_span.end as u64,
                    kind: ReferenceKind::TypeReference as u8,
                });
            }
        }
    }

    // Discard empty file buckets (entries with Absent bodies contribute nothing).
    let file_refs: Vec<FileReferences> = by_file
        .into_iter()
        .filter(|(_, refs)| !refs.is_empty())
        .map(|(path, references)| FileReferences { path, references })
        .collect();

    ReferenceSet { by_file: file_refs }
}

/// Derive a [`ReferenceSet`] of *unresolved* cross-package edges from the
/// accumulated `Symbols`-batch payloads — the cage/Linux-path fix for the
/// host-divergence [`build_reference_set_from_table`]'s doc comment describes
/// (adversarial audit C2).
///
/// # Why this exists (the gap `build_reference_set_from_bodies` cannot close)
///
/// `build_reference_set_from_bodies` only ever sees resolved facts: an
/// unlinked `Ref::Foreign` (`target: None`) is dropped by `seal`'s
/// occurrence-resolution loop before an `OracleCall`/`OracleTypeMention` is
/// ever constructed, and `UnknownType::UnresolvedExternal` has no `Bodies`-
/// frame representation at all — both are structurally absent from the
/// `Bodies` stream, regardless of how much of it is harvested. The data these
/// two facts actually live in is the **declaration surface** carried on the
/// `Symbols` batch's `OwnedEntryPayload.kind: KindWire` — field types, impl
/// targets, generic bounds, and so on — which is exactly what
/// [`ir_vcs::wire::TypeRefWire::ForeignUnlinked`] and
/// [`ir_vcs::wire::TypeRefWire::UnresolvedExternal`] now carry across the
/// wire (they raise from the identical `Ref::Foreign`/`UnknownType` the
/// in-process/table path's `for_each_ref`/`for_each_unknown` walk reads
/// straight off the live `Entry` — see `ir_vcs::raise::raise_type_ref`).
/// `KindWire::for_each_type_ref` is the wire-side twin of that walk.
///
/// # Scope: unresolved edges only, not full parity
///
/// This function does **not** also harvest `TypeRefWire::Same`/`Foreign`
/// (already-resolved) declaration-position edges. That is a pre-existing,
/// separate divergence from the one this closes: the table path's
/// declaration-position walk and the bodies path's body-occurrence walk draw
/// from disjoint fact sources even in the resolved case (a field's declared
/// type vs. a function body's call sites), and reconciling *that* is a wider
/// change than the C2 fix this function is scoped to. Restricting to the two
/// unresolved variants keeps this a precise, reviewable close of exactly the
/// divergence the parity test below asserts.
fn build_reference_set_from_payloads(
    payloads: &[ir_vcs::wire::OwnedEntryPayload],
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, Reference, ReferenceSet};
    use ir::vocab::ReferenceKind;
    use ir_vcs::wire::{TypeMention, TypeRefWire};
    use smol_str::SmolStr;

    let mut by_file: HashMap<SmolStr, Vec<Reference>> = HashMap::new();

    for payload in payloads {
        let refs = by_file
            .entry(SmolStr::from(payload.symbol.source_path.as_str()))
            .or_default();

        // `TypeMention::Ref(TypeRefWire::UnresolvedExternal(_))` (a reference-
        // position slot) and `TypeMention::UnresolvedExternalValue(_)` (a
        // value-position `TypeWire::UnresolvedExternal`) carry the identical
        // fact — a producer's unresolved-external spelling — and become the
        // same `External` edge; they're kept as separate match arms (rather
        // than an or-pattern) only because they bind `&String` vs `&str`.
        payload.kind.for_each_type_mention(|mention| match mention {
            TypeMention::Ref(TypeRefWire::ForeignUnlinked(key)) => refs.push(Reference {
                target: external_ref_target_from_key(key),
                span_start: 0,
                span_end: 0,
                kind: ReferenceKind::TypeReference as u8,
            }),
            TypeMention::Ref(TypeRefWire::UnresolvedExternal(name)) => refs.push(Reference {
                target: crate::server::registry::blob::RefTarget::External {
                    path: name.clone(),
                    dependency: UNRESOLVED_EXTERNAL_DEPENDENCY.to_owned(),
                },
                span_start: 0,
                span_end: 0,
                kind: ReferenceKind::TypeReference as u8,
            }),
            TypeMention::UnresolvedExternalValue(name) => refs.push(Reference {
                target: crate::server::registry::blob::RefTarget::External {
                    path: name.to_owned(),
                    dependency: UNRESOLVED_EXTERNAL_DEPENDENCY.to_owned(),
                },
                span_start: 0,
                span_end: 0,
                kind: ReferenceKind::TypeReference as u8,
            }),
            // Already-resolved edges: out of scope here, see the doc comment.
            TypeMention::Ref(TypeRefWire::Same(_) | TypeRefWire::Foreign(_)) => {}
        });
    }

    let file_refs: Vec<FileReferences> = by_file
        .into_iter()
        .filter(|(_, refs)| !refs.is_empty())
        .map(|(path, references)| FileReferences { path, references })
        .collect();

    ReferenceSet { by_file: file_refs }
}

/// Merge two [`ReferenceSet`](crate::server::registry::blob::ReferenceSet)s
/// computed from disjoint fact sources over the same package (bodies-derived
/// and payload-derived edges), unioning references per file path.
fn merge_reference_sets(
    a: crate::server::registry::blob::ReferenceSet,
    b: crate::server::registry::blob::ReferenceSet,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, Reference, ReferenceSet};
    use smol_str::SmolStr;

    let mut by_file: HashMap<SmolStr, Vec<Reference>> = HashMap::new();
    for f in a.by_file.into_iter().chain(b.by_file) {
        by_file.entry(f.path).or_default().extend(f.references);
    }

    ReferenceSet {
        by_file: by_file
            .into_iter()
            .map(|(path, references)| FileReferences { path, references })
            .collect(),
    }
}

/// Derive usage-query [`ir::vocab::Occurrence`]s from the accumulated `Bodies`
/// frames — the same source data [`build_reference_set_from_bodies`] projects
/// into `Reference`s, projected instead into the `(owner, Occurrence)` shape
/// [`ir::view::IrView::add_occurrence`] takes.
///
/// # Policy (mirrors `build_reference_set_from_bodies` exactly)
///
/// - `OracleCall` contributes an occurrence only when it has a resolved
///   `target` and its confidence is graph-worthy (`>= Confidence::Index`);
///   its own reported `kind`/`confidence`/`rel_span` are carried through
///   unchanged — an `Occurrence` has a confidence field to record exactly
///   this, unlike the blob `Reference` wire shape.
/// - `OracleTypeMention` has no confidence field of its own (every mention the
///   oracle records is already a resolved fact, never a partial one), so it is
///   recorded at `Confidence::Oracle` — the tier that name reflects — with
///   `ReferenceKind::TypeReference`.
/// - The owner is `bw.intro` (the entry the body belongs to) in both cases.
///
/// Tree-sitter-only call facts (`BodyEmbed::Present(_).treesitter`) carry no
/// stable cross-package target and are skipped, same as the reference-set
/// projection.
fn build_occurrences_from_bodies(
    bodies: &[ir_vcs::protocol::BodyWire],
) -> Vec<(ir::change::IntroId, ir::vocab::Occurrence)> {
    use ir::body::BodyEmbed;
    use ir::vocab::{Confidence, Occurrence, ReferenceKind};

    let mut occurrences = Vec::new();

    for bw in bodies {
        let BodyEmbed::Present(facts) = &bw.body else {
            continue;
        };

        for call in &facts.oracle.calls {
            let Some(ref target) = call.target else {
                continue;
            };
            if !call.confidence.is_graph_worthy() {
                continue;
            }
            occurrences.push((
                bw.intro,
                Occurrence::new(target.clone(), call.kind, call.confidence, call.rel_span),
            ));
        }

        for mention in &facts.oracle.type_mentions {
            occurrences.push((
                bw.intro,
                Occurrence::new(
                    mention.ty.clone(),
                    ReferenceKind::TypeReference,
                    Confidence::Oracle,
                    mention.rel_span,
                ),
            ));
        }
    }

    occurrences
}

/// Map a [`ir::change::StableRef`] to a [`crate::server::registry::blob::RefTarget`].
///
/// The `path` component is the target symbol's `IntroId` in hex — the durable,
/// wire-stable identity the IR plane uses. Source file paths for target symbols
/// are not available at this stage (the host would need to look them up from the
/// IR store, which is an async operation not available in the blocking decode
/// path). The `intro_hex` is sufficient for cross-reference graph edges; the
/// IR graph adapter resolves it to a file path on demand.
fn make_ref_target(
    stable: &ir::change::StableRef,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::RefTarget {
    use crate::server::registry::blob::RefTarget;

    let intro_hex = stable.intro.to_hex();
    let target_pkg_key = format!(
        "{}:{}",
        stable.package.ecosystem.as_str(),
        stable.package.name.as_str()
    );

    if owning_pkg == Some(target_pkg_key.as_str()) {
        RefTarget::Local(intro_hex)
    } else {
        RefTarget::External {
            path: intro_hex,
            dependency: target_pkg_key,
        }
    }
}

/// Attach an empty-but-valid IR payload section to `builder` (the postcard
/// encoding of an empty `Vec<OwnedEntryPayload>`) so `finalize()` does not fail
/// with `MissingIrSection`.
///
/// Split out from [`attach_empty_ir_sections`] because the in-process compile
/// path ([`super::super::compile_inprocess`]) needs an empty IR section *alongside a
/// real reference section*: there is no forward semantic→wire encoder in the
/// workspace to rebuild a faithful `Vec<OwnedEntryPayload>` from the sealed
/// table, and nothing decodes this section as payloads yet (only its
/// byte-integrity is audited — see `crate::server::save::blobs`), so it stays
/// empty-but-valid while references carry the real graph.
pub(in crate::server::coordination) fn set_empty_ir_section(builder: &mut BlobBuilder) {
    let empty_payloads: Vec<ir_vcs::wire::OwnedEntryPayload> = Vec::new();
    let ir_blob = postcard::to_allocvec(&empty_payloads).unwrap_or_default();
    let _ = builder.set_ir(bytes::Bytes::from(ir_blob));
}

/// Attach an empty IR section and an empty reference section to `builder` so
/// that `finalize()` does not fail with `MissingIrSection` or
/// `MissingReferencesSection`. Used when the stream is empty or undecodable.
pub(in crate::server::coordination) fn attach_empty_ir_sections(builder: &mut BlobBuilder) {
    set_empty_ir_section(builder);
    let empty_refs = crate::server::registry::blob::ReferenceSet {
        by_file: Vec::new(),
    };
    let _ = builder.set_references(&empty_refs);
}

/// Derive a [`crate::server::registry::blob::ReferenceSet`] directly from a
/// sealed [`ir::apply::PristineIntroTable`] — the in-process (macOS) analogue
/// of [`build_reference_set_from_bodies`].
///
/// # Why this exists (macOS vs cage, `// reconcile:`)
///
/// The cage path recovers references from the producer's `Bodies` stream frames
/// (oracle call / type-mention facts with real spans). The in-process producer
/// entrypoint (`nudox_languages::produce`) returns only the *sealed table*:
/// there is no second `Bodies` channel, and no forward semantic→wire encoder in
/// the workspace to rebuild the NdIrF1 `Symbols`/`Bodies` stream from it (so the
/// cage's [`ingest_ir_bytes`] byte-decode path cannot be reused — see
/// [`super::super::compile_inprocess`]). References are therefore recovered from the
/// one artifact the in-process path *does* have: the sealed table's own
/// cross-symbol `Ref` edges — every type nominal a declaration mentions in its
/// signature / fields / impl-of, already lowered by `seal` to `Ref::Intro`
/// (same-package) or `Ref::Foreign` (cross-package).
///
/// It reuses the exact [`RefTarget`](crate::server::registry::blob::RefTarget) /
/// [`Reference`](crate::server::registry::blob::Reference) /
/// [`ReferenceSet`](crate::server::registry::blob::ReferenceSet) blob vocabulary
/// and the [`make_ref_target`] mapping the cage path uses, so both hosts land in
/// one on-disk reference format that `save::blobs`'s `ReferenceSet::decode` audit
/// and the future reverse-`occ` index read identically.
///
/// # Spans
/// The sealed declaration table carries no per-reference occurrence span (that
/// lives in the `Bodies` facts the in-process path never sees), so every
/// reference is recorded with a degenerate relative span `0..0`: the *edge*
/// (who mentions whom) is exact; the intra-declaration offset is not available.
///
/// # Structural edges are excluded
/// [`ir::entry::Entry::for_each_ref`] also visits the `Node` parent/children
/// tree edges; those are structural containment, not usage, so any `Ref::Intro`
/// naming this entry's own parent or a child is dropped. What remains is exactly
/// the type-nominal usage graph.
pub(in crate::server::coordination) fn build_reference_set_from_table(
    table: &ir::apply::PristineIntroTable,
    source_root: &std::path::Path,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, RefTarget, Reference, ReferenceSet};
    use ir::index::Ref;
    use ir::vocab::ReferenceKind;
    use smol_str::SmolStr;
    use std::collections::HashSet;

    let mut by_file: HashMap<SmolStr, Vec<Reference>> = HashMap::new();

    for (intro, entry) in table.iter() {
        // Node tree edges (parent + children) are structural containment, not
        // usage — collect them so a `Ref::Intro` naming one is skipped.
        let mut structural: HashSet<ir::change::IntroId> = HashSet::new();
        if let Some(parent) = table.parent_of(intro) {
            structural.insert(parent);
        }
        structural.extend(table.children_of(intro).iter().copied());

        let refs = by_file
            .entry(reference_source_path(entry, source_root))
            .or_default();

        entry.for_each_ref(|raw| match raw {
            Ref::Intro(id) => {
                if structural.contains(id) {
                    return;
                }
                refs.push(Reference {
                    target: RefTarget::Local(id.to_hex()),
                    span_start: 0,
                    span_end: 0,
                    kind: ReferenceKind::TypeReference as u8,
                });
            }
            Ref::Foreign { key, target } => {
                let target = match target {
                    Some(stable) => make_ref_target(stable, owning_pkg),
                    None => external_ref_target_from_key(key),
                };
                refs.push(Reference {
                    target,
                    span_start: 0,
                    span_end: 0,
                    kind: ReferenceKind::TypeReference as u8,
                });
            }
            // A `Ref::Local` surviving into a sealed table is a seal bug
            // (`SealReport::unmapped_local`), not a resolvable edge — skip it
            // rather than emit a reference to a dropped arena index.
            Ref::Local(_) => {}
        });

        // Cross-package type mentions a producer could not lower into a
        // `Ref::Foreign` are carried as `UnknownType::UnresolvedExternal` — a
        // `Type::Unknown`, which holds no `RawRef` and so is invisible to
        // `for_each_ref` above. TypeScript, Python and Clang emit exactly these
        // for their cross-package types (Rust/Go/Java/C# reach the ref set via
        // `Ref::Foreign` instead). Harvest them as `External` edges so both
        // encodings land in the reference set and `Target::Usages` sees them.
        //
        // `path` is the producer's spelling — the durable join key a
        // corpus-side link pass matches against sibling packages' intro tables,
        // mirroring the `intro_hex`/`key.path` the `Ref::Foreign` arm emits.
        // `dependency` is the honest "package not named" sentinel: the producer
        // reached for `UnresolvedExternal` precisely because it could not name
        // the owning package, and guessing one from the spelling is what that
        // variant exists to avoid. A resolver treats it as "search broadly".
        //
        // `UnresolvedLocalName` is deliberately excluded: it resolves *within*
        // this package (the import graph is one pass short), so an `External`
        // edge would misroute it — that closure is a within-package resolution
        // pass, not this cross-package harvest.
        entry.for_each_unknown(|reason| {
            if let ir::kinds::UnknownType::UnresolvedExternal { name } = reason {
                refs.push(Reference {
                    target: RefTarget::External {
                        path: name.clone(),
                        dependency: UNRESOLVED_EXTERNAL_DEPENDENCY.to_owned(),
                    },
                    span_start: 0,
                    span_end: 0,
                    kind: ReferenceKind::TypeReference as u8,
                });
            }
        });
    }

    let by_file: Vec<FileReferences> = by_file
        .into_iter()
        .filter(|(_, refs)| !refs.is_empty())
        .map(|(path, references)| FileReferences { path, references })
        .collect();

    ReferenceSet { by_file }
}

/// The `dependency` slot for an `External` edge harvested from
/// [`ir::kinds::UnknownType::UnresolvedExternal`]: the producer named a
/// cross-package type but could **not** name the package that owns it, so there
/// is no honest `ecosystem:name` to record (unlike the `ForeignKey` path, which
/// carries one). A corpus-side link pass reads this sentinel as "owner unknown —
/// match `path` against every sibling", rather than filtering by dependency
/// first. Distinct, greppable, and never a real `ecosystem:name`.
const UNRESOLVED_EXTERNAL_DEPENDENCY: &str = "<unresolved-external>";

/// Build an `External` [`RefTarget`](crate::server::registry::blob::RefTarget)
/// for a named-but-unlinked cross-package reference.
///
/// The in-process path seals against [`ir::foreign::Unlinked`], so every foreign
/// ref arrives with `target: None` but a fully-named [`ir::foreign::ForeignKey`].
/// `dependency` is `ecosystem:name` when the producer could name the owning
/// package, else the bare ecosystem tag (`Namespace`/`Universe` origins that
/// decline to guess a package). `path` is the target's canonical path in its own
/// language's spelling — the durable join key a corpus-side resolver matches on,
/// mirroring the `intro_hex` `path` the linked cage path emits.
fn external_ref_target_from_key(
    key: &ir::foreign::ForeignKey,
) -> crate::server::registry::blob::RefTarget {
    let dependency = match key.origin.lineage() {
        Some(lineage) => format!("{}:{}", lineage.ecosystem.as_str(), lineage.name.as_str()),
        None => key.origin.ecosystem().as_str().to_owned(),
    };
    crate::server::registry::blob::RefTarget::External {
        path: key.path.to_string(),
        dependency,
    }
}

/// The in-package-relative source path a reference is grouped under: the entry's
/// declaration source with `source_root` stripped (so it matches the
/// [`FileEntry::path`] keys the builder stages), falling back to the raw path,
/// or the `"<unknown>"` sentinel for a synthesized / unlocated entry (the same
/// sentinel the cage path uses for a body without a matching symbol).
fn reference_source_path(
    entry: &ir::entry::Entry,
    source_root: &std::path::Path,
) -> smol_str::SmolStr {
    let source = &entry.sym().source;
    if source.as_os_str().is_empty() {
        return smol_str::SmolStr::new("<unknown>");
    }
    let rel = source.strip_prefix(source_root).unwrap_or(source);
    smol_str::SmolStr::from(rel.to_string_lossy().as_ref())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::identity_toolchain;
    use super::*;
    use ir_vcs::protocol::{
        BodyWire, FailureKindWire, FrameWriter, IR_STREAM_VERSION, ProducerId, StreamFrame,
    };

    // ── W1: ingest_ir_bytes must distinguish producer breakage from a
    // genuinely empty package ──────────────────────────────────────────────

    fn test_package() -> heart::PackageId {
        heart::PackageId::from_uuid(uuid::Uuid::from_bytes([7u8; 16]))
    }

    fn hello_frame() -> StreamFrame {
        StreamFrame::Hello {
            version: IR_STREAM_VERSION,
            job: heart::content::JobKey::derive(b"producer", b"toolchain", b"source", b"deplock"),
            producer: ProducerId::from_static("test-producer"),
        }
    }

    fn encode_frames(frames: &[StreamFrame]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = FrameWriter::new(&mut buf);
        for f in frames {
            w.write_frame(f).expect("encode frame");
        }
        buf
    }

    /// A well-behaved producer that honestly emits zero symbols still closes
    /// with `Finish`. This must succeed cleanly — it is the case the fix must
    /// NOT break.
    #[test]
    fn w1_clean_finish_with_zero_symbols_is_not_degraded() {
        let bytes = encode_frames(&[
            hello_frame(),
            StreamFrame::Finish {
                emitted: 0,
                producer_digest: heart::content::ContentHash::from_bytes([0u8; 32]),
            },
        ]);
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        let outcome = ingest_ir_bytes(&mut builder, &bytes);
        assert_eq!(outcome.degraded_reason, None);
        assert!(outcome.identifiers.is_empty());
    }

    /// Failing-first case (documents the pre-fix bug, now asserts the fix):
    /// a stream that ends right after `Hello` — no `Finish`, no `Abort` —
    /// is a truncated producer, not a legitimate empty package.
    #[test]
    fn w1_truncated_after_hello_is_degraded() {
        let bytes = encode_frames(&[hello_frame()]);
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        let outcome = ingest_ir_bytes(&mut builder, &bytes);
        assert!(
            outcome.degraded_reason.is_some(),
            "a stream truncated before Finish must be reported as degraded, not silent success"
        );
    }

    /// An explicit `Abort` frame is a producer-signaled failure.
    #[test]
    fn w1_abort_frame_is_degraded() {
        let bytes = encode_frames(&[
            hello_frame(),
            StreamFrame::Abort {
                failure: FailureKindWire::Internal,
                message: "producer crashed mid-parse".to_owned(),
            },
        ]);
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        let outcome = ingest_ir_bytes(&mut builder, &bytes);
        let reason = outcome.degraded_reason.expect("Abort must degrade the job");
        assert!(reason.contains("aborted"), "reason: {reason}");
    }

    /// A corrupted postcard payload after a valid Hello must degrade, not
    /// silently truncate to "whatever decoded before the corruption".
    #[test]
    fn w1_corrupt_frame_after_hello_is_degraded() {
        let mut bytes = encode_frames(&[hello_frame()]);
        // Append a frame whose declared length is 4 bytes of bytes that are
        // not a valid postcard-encoded `StreamFrame`.
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        let outcome = ingest_ir_bytes(&mut builder, &bytes);
        let reason = outcome
            .degraded_reason
            .expect("corrupt frame must degrade the job");
        assert!(reason.contains("decode error"), "reason: {reason}");
    }

    /// A stream with no valid Hello at all (garbage from byte 0) must
    /// degrade rather than silently attach an empty-but-"successful" IR
    /// section.
    #[test]
    fn w1_missing_hello_is_degraded() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02];
        let mut builder = BlobBuilder::new(
            test_package(),
            identity_toolchain(crate::ecosystem::Language::Rust),
        );
        let outcome = ingest_ir_bytes(&mut builder, &bytes);
        assert!(outcome.degraded_reason.is_some());
        assert!(outcome.identifiers.is_empty());
    }

    // ── In-process path: real reference edges flow from the sealed table ──
    //
    // The end-to-end sanity check for the macOS/dev fix: a sealed
    // `PristineIntroTable` (what `nudox_languages::produce` returns in-process)
    // with real cross-symbol references must yield a NON-EMPTY `ReferenceSet`
    // with real Local *and* External edges — the section that was previously
    // attached empty, blinding `Target::Usages` for every macOS-indexed package.
    //
    // Filterable in isolation (`coordination::indexing::ir_stream::tests::
    // in_process_sealed_table_yields_real_reference_edges`); it never touches
    // `index::pack`, so the `--features server` zstd duplicate-symbol clash
    // never runs.
    #[test]
    fn in_process_sealed_table_yields_real_reference_edges() {
        use crate::server::registry::blob::RefTarget;
        use ir::build::{
            EcosystemId, Impl, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Type, Visibility,
        };
        use ir::foreign::{ForeignKey, Unlinked};
        use ir::index::Ref;

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::from("src/lib.rs"),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
        let core =
            PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"));

        // `struct Widget;` + `impl Clone for Widget` — the impl's `self_ty`
        // references the same-package `Widget` (→ a `Ref::Intro`, i.e. a Local
        // edge post-seal), and its `of` names a cross-package trait (→ a
        // `Ref::Foreign`, i.e. an External edge under `Unlinked`).
        let mut low: Lowering<&'static str> =
            Lowering::new(PackageId::path("fixture"), sym("fixture"));
        let self_ref = low.refer::<Record>("Widget");
        low.declare("Widget", None, sym("Widget"), Record::builder().build());
        let of = low.nominal_import(ForeignKey::in_package(
            core.clone(),
            "core::clone::Clone",
            "Clone",
        ));
        low.declare(
            "impl#clone",
            None,
            sym("impl Clone for Widget"),
            Impl::builder()
                .of(of)
                .self_ty(Type::Nominal(self_ref.into_raw()))
                .build(),
        );

        let table = low
            .finish()
            .expect("lowering must succeed")
            .seal(&lineage, &Unlinked)
            .table;

        let ref_set =
            build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));

        let edges: Vec<&crate::server::registry::blob::Reference> = ref_set
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .collect();

        assert!(
            !edges.is_empty(),
            "a sealed table with cross-symbol type references must yield a \
             non-empty reference set — this is the section the in-process path \
             used to leave empty, blinding Target::Usages on macOS"
        );
        assert!(
            edges
                .iter()
                .any(|r| matches!(&r.target, RefTarget::Local(_))),
            "the impl's self_ty referencing same-package `Widget` must be a \
             Local edge; got {edges:?}"
        );
        assert!(
            edges.iter().any(|r| matches!(
                &r.target,
                RefTarget::External { dependency, .. } if dependency == "rust-sysroot:core"
            )),
            "the impl's foreign `Clone` trait must be an External edge naming \
             its owning package; got {edges:?}"
        );
        // And the whole set must round-trip the on-disk codec the audit uses.
        let encoded = ref_set.encode().expect("reference set must encode");
        let decoded = crate::server::registry::blob::ReferenceSet::decode(&encoded)
            .expect("reference set must decode (matches save::blobs audit)");
        assert_eq!(decoded.by_file.len(), ref_set.by_file.len());
    }

    // ── reviewer adversarial tests for the in-process reference seam ──

    #[test]
    fn adversarial_table_with_no_type_references_yields_empty_but_valid_set() {
        // The empty-vs-degraded boundary (W1): a package whose declarations
        // name no other types (a bare fieldless struct, no impl) is honest
        // ABSENCE, not breakage. build_reference_set_from_table must return an
        // empty set that still encodes/decodes, so the caller reports a clean
        // success — NOT a degraded job, and NOT a false "has usages".
        use ir::build::{
            EcosystemId, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Visibility,
        };
        use ir::foreign::Unlinked;

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::from("src/lib.rs"),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
        let mut low: Lowering<&'static str> =
            Lowering::new(PackageId::path("fixture"), sym("fixture"));
        low.declare("Lonely", None, sym("Lonely"), Record::builder().build());
        let table = low
            .finish()
            .expect("lowering must succeed")
            .seal(&lineage, &Unlinked)
            .table;

        let ref_set =
            build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));
        let edges = ref_set
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .count();
        assert_eq!(
            edges, 0,
            "a package that references no other types must yield zero usage edges (honest absence)"
        );
        let encoded = ref_set.encode().expect("empty set still encodes");
        let decoded = crate::server::registry::blob::ReferenceSet::decode(&encoded)
            .expect("empty set decodes");
        assert!(decoded.by_file.iter().all(|f| f.references.is_empty()));
    }

    #[test]
    fn unresolved_external_type_mentions_become_external_ref_edges() {
        // The gap this closes: TypeScript / Python / Clang carry a cross-package
        // type they could not lower as `UnknownType::UnresolvedExternal` — a
        // `Type::Unknown` with no `RawRef`, invisible to the `Ref` walk that
        // harvests the other languages' `Ref::Foreign`. It must still surface as
        // an `External` usage edge, keyed on the producer's spelling, with the
        // honest owner-unknown dependency sentinel. And `UnresolvedLocalName` —
        // which resolves *within* the package — must NOT be harvested as
        // external (that is a separate within-package resolution pass).
        use crate::server::registry::blob::RefTarget;
        use ir::build::{
            Alias, EcosystemId, Lowering, PackageId, PackageLineageId, PackageName, Symbol, Type,
            Visibility,
        };
        use ir::foreign::Unlinked;
        use ir::kinds::UnknownType;

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::from("src/lib.rs"),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("fixture"));
        let mut low: Lowering<&'static str> =
            Lowering::new(PackageId::path("fixture"), sym("fixture"));
        // A cross-package type the producer could not name an owning package for.
        low.declare(
            "External",
            None,
            sym("External"),
            Alias::builder()
                .target(Type::Unknown(UnknownType::UnresolvedExternal {
                    name: "numpy.ndarray".to_owned(),
                }))
                .build(),
        );
        // A bare name that resolves WITHIN this package — must be ignored here.
        low.declare(
            "Local",
            None,
            sym("Local"),
            Alias::builder()
                .target(Type::Unknown(UnknownType::UnresolvedLocalName {
                    name: "Widget".to_owned(),
                }))
                .build(),
        );
        let table = low
            .finish()
            .expect("lowering must succeed")
            .seal(&lineage, &Unlinked)
            .table;

        let ref_set =
            build_reference_set_from_table(&table, std::path::Path::new(""), Some("npm:fixture"));
        let externals: Vec<(String, String)> = ref_set
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .filter_map(|r| match &r.target {
                RefTarget::External { path, dependency } => {
                    Some((path.clone(), dependency.clone()))
                }
                RefTarget::Local(_) => None,
            })
            .collect();

        // Exactly the external mention is harvested — with its exact spelling and
        // the owner-unknown sentinel — and the within-package name is not.
        assert_eq!(
            externals,
            vec![(
                "numpy.ndarray".to_owned(),
                super::UNRESOLVED_EXTERNAL_DEPENDENCY.to_owned()
            )],
            "the unresolved-external type mention must become exactly one External \
             edge; the unresolved-local name must not appear"
        );
    }

    #[test]
    fn adversarial_reference_edges_carry_exact_target_and_degenerate_spans() {
        // Tighter contract than the sanity test: the External edge's `path`
        // must be the foreign key's canonical path (the durable resolver join
        // key), every edge's span must be the documented degenerate 0..0 (the
        // sealed table carries no occurrence span), and encode/decode must
        // preserve the exact edge count (no silent drop in the codec).
        use crate::server::registry::blob::RefTarget;
        use ir::build::{
            EcosystemId, Impl, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Type, Visibility,
        };
        use ir::foreign::{ForeignKey, Unlinked};

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::from("src/lib.rs"),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
        let core =
            PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"));
        let mut low: Lowering<&'static str> =
            Lowering::new(PackageId::path("fixture"), sym("fixture"));
        let self_ref = low.refer::<Record>("Widget");
        low.declare("Widget", None, sym("Widget"), Record::builder().build());
        let of = low.nominal_import(ForeignKey::in_package(
            core.clone(),
            "core::clone::Clone",
            "Clone",
        ));
        low.declare(
            "impl#clone",
            None,
            sym("impl Clone for Widget"),
            Impl::builder()
                .of(of)
                .self_ty(Type::Nominal(self_ref.into_raw()))
                .build(),
        );
        let table = low
            .finish()
            .expect("lowering must succeed")
            .seal(&lineage, &Unlinked)
            .table;

        let ref_set =
            build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));
        let edges: Vec<&crate::server::registry::blob::Reference> = ref_set
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .collect();

        // Exactly one Local (self_ty → same-package Widget) and one External
        // (foreign Clone trait) — no phantom duplicates from the Node tree.
        assert_eq!(
            edges
                .iter()
                .filter(|r| matches!(r.target, RefTarget::Local(_)))
                .count(),
            1,
            "exactly one Local edge; got {edges:?}"
        );
        let external: Vec<_> = edges
            .iter()
            .filter_map(|r| match &r.target {
                RefTarget::External { path, dependency } => {
                    Some((path.clone(), dependency.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            external.len(),
            1,
            "exactly one External edge; got {edges:?}"
        );
        assert_eq!(
            external[0],
            (
                "core::clone::Clone".to_owned(),
                "rust-sysroot:core".to_owned()
            ),
            "External edge must carry the foreign key's canonical path + owning package"
        );
        // Documented degeneracy: no per-reference span from a sealed table.
        assert!(
            edges.iter().all(|r| r.span_start == 0 && r.span_end == 0),
            "sealed-table references have degenerate 0..0 spans"
        );
        // Codec must preserve every edge.
        let round =
            crate::server::registry::blob::ReferenceSet::decode(&ref_set.encode().expect("encode"))
                .expect("decode");
        assert_eq!(
            round
                .by_file
                .iter()
                .map(|f| f.references.len())
                .sum::<usize>(),
            edges.len(),
            "encode/decode must preserve the exact edge count"
        );
    }

    // ── build_occurrences_from_bodies: the cage-path usage-query projection ──
    //
    // The same Bodies-frame oracle facts `build_reference_set_from_bodies`
    // projects into `Reference`s (for the reference-set blob section) must
    // also project into `ir::vocab::Occurrence`s (for the usage-query scope
    // `indexing/mod.rs` loads via `load_usage_scope`). These tests exercise
    // that second projection directly, in isolation from the stream decode.

    fn occ_pkg() -> ir::change::PackageLineageId {
        ir::change::PackageLineageId::new(
            ir::change::EcosystemId::new("cargo"),
            ir::change::PackageName::new("fixture"),
        )
    }

    fn occ_intro(n: u8) -> ir::change::IntroId {
        ir::change::IntroId::from_raw([n; 32])
    }

    fn oracle_body(oracle: ir::body::OracleBody) -> ir::body::BodyEmbed {
        ir::body::BodyEmbed::Present(ir::body::BodyFacts {
            language: ir::body::Language::Rust,
            tree: ir::body::TreesitterBody::default(),
            oracle,
            merge: ir::body::BodyMergeNote::oracle_only(),
        })
    }

    /// A caller's body with a graph-worthy oracle call to a callee target
    /// projects into exactly one `Occurrence`, owned by the caller, with the
    /// call's exact kind/confidence/span preserved — the content a `/usages`
    /// query on the callee must return.
    #[test]
    fn caller_calling_callee_projects_one_occurrence_owned_by_caller() {
        use ir::body::{OracleBody, OracleCall};
        use ir::change::StableRef;
        use ir::vocab::{Confidence, ReferenceKind, RelSpan};

        let caller = occ_intro(2);
        let callee = StableRef::new(occ_pkg(), occ_intro(1));

        let bodies = vec![BodyWire {
            intro: caller,
            body: oracle_body(OracleBody {
                calls: vec![OracleCall {
                    target: Some(callee.clone()),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Index,
                    rel_span: RelSpan::new(4, 9),
                }],
                type_mentions: Vec::new(),
                reads_writes: Vec::new(),
            }),
        }];

        let occurrences = build_occurrences_from_bodies(&bodies);
        assert_eq!(occurrences.len(), 1, "one call, one occurrence");
        let (owner, occurrence) = &occurrences[0];
        assert_eq!(*owner, caller, "occurrence must be owned by the caller");
        assert_eq!(occurrence.target, callee, "target must be the callee");
        assert_eq!(occurrence.kind, ReferenceKind::FunctionCall);
        assert_eq!(occurrence.confidence, Confidence::Index);
        assert_eq!(occurrence.span, RelSpan::new(4, 9));
    }

    /// A call with no resolved target (tree-sitter name-only fact riding in
    /// `OracleCall.target: None`) contributes no occurrence — there is no
    /// stable cross-package identity to key a usages query on.
    #[test]
    fn unresolved_call_target_is_skipped() {
        use ir::body::{OracleBody, OracleCall};
        use ir::vocab::{Confidence, ReferenceKind, RelSpan};

        let bodies = vec![BodyWire {
            intro: occ_intro(2),
            body: oracle_body(OracleBody {
                calls: vec![OracleCall {
                    target: None,
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Oracle,
                    rel_span: RelSpan::new(0, 3),
                }],
                type_mentions: Vec::new(),
                reads_writes: Vec::new(),
            }),
        }];

        assert!(build_occurrences_from_bodies(&bodies).is_empty());
    }

    /// A call resolved below the graph floor (`Confidence::Suffix`) is
    /// excluded — same policy as the reference-set projection, and the same
    /// floor `ReversePositionIndex::build` enforces on the read side.
    #[test]
    fn below_floor_confidence_call_is_skipped() {
        use ir::body::{OracleBody, OracleCall};
        use ir::change::StableRef;
        use ir::vocab::{Confidence, ReferenceKind, RelSpan};

        let bodies = vec![BodyWire {
            intro: occ_intro(2),
            body: oracle_body(OracleBody {
                calls: vec![OracleCall {
                    target: Some(StableRef::new(occ_pkg(), occ_intro(1))),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Suffix,
                    rel_span: RelSpan::new(0, 3),
                }],
                type_mentions: Vec::new(),
                reads_writes: Vec::new(),
            }),
        }];

        assert!(
            build_occurrences_from_bodies(&bodies).is_empty(),
            "Suffix confidence is below the graph floor and must not project"
        );
    }

    /// A type mention (no confidence field of its own — every mention the
    /// oracle records is already resolved) projects at `Confidence::Oracle`
    /// with `ReferenceKind::TypeReference`.
    #[test]
    fn type_mention_projects_at_oracle_confidence() {
        use ir::body::{OracleBody, OracleTypeMention};
        use ir::change::StableRef;
        use ir::vocab::{Confidence, ReferenceKind, RelSpan};

        let owner = occ_intro(5);
        let ty = StableRef::new(occ_pkg(), occ_intro(6));

        let bodies = vec![BodyWire {
            intro: owner,
            body: oracle_body(OracleBody {
                calls: Vec::new(),
                type_mentions: vec![OracleTypeMention {
                    ty: ty.clone(),
                    rel_span: RelSpan::new(2, 8),
                }],
                reads_writes: Vec::new(),
            }),
        }];

        let occurrences = build_occurrences_from_bodies(&bodies);
        assert_eq!(occurrences.len(), 1);
        let (owner_out, occurrence) = &occurrences[0];
        assert_eq!(*owner_out, owner);
        assert_eq!(occurrence.target, ty);
        assert_eq!(occurrence.kind, ReferenceKind::TypeReference);
        assert_eq!(occurrence.confidence, Confidence::Oracle);
        assert_eq!(occurrence.span, RelSpan::new(2, 8));
    }

    /// An `Absent` body contributes nothing (mirrors the reference-set
    /// projection's handling of entries with no body facts at all).
    #[test]
    fn absent_body_contributes_no_occurrences() {
        let bodies = vec![BodyWire {
            intro: occ_intro(2),
            body: ir::body::BodyEmbed::Absent,
        }];
        assert!(build_occurrences_from_bodies(&bodies).is_empty());
    }

    // ── Host-divergence parity fix (adversarial audit C2) ────────────────────
    //
    // `build_reference_set_from_table` harvests External edges from two
    // Entry-level sources: an unlinked `Ref::Foreign { target: None, .. }`
    // via `for_each_ref`, and `UnknownType::UnresolvedExternal` via
    // `for_each_unknown`. Both used to be structurally absent from the wire:
    // `ir_vcs::wire::TypeRefWire` had only `Same(IntroId)`/`Foreign(StableRef)`
    // (`ir/vcs/wire.rs`), so a `Ref::Foreign{target:None}` collapsed into a
    // lossy synthetic same-package intro on raise, and `UnknownType::
    // UnresolvedExternal` had no wire slot at all.
    //
    // The fix: `TypeRefWire` gained `ForeignUnlinked(ForeignKey)` and
    // `UnresolvedExternal(String)` tail variants (plus `TypeWire::
    // UnresolvedExternal` for the value-position case), `ir_vcs::raise::
    // raise_type_ref`/`raise_type_wire` now populate them from the exact
    // `Ref::Foreign`/`UnknownType` data the table path already reads, and
    // `KindWire::for_each_type_ref` walks every `TypeRefWire` slot a
    // `Symbols`-batch payload carries. `build_reference_set_from_payloads`
    // (this module) harvests the two new variants into the same `External`
    // edges `build_reference_set_from_table` produces, reusing
    // `external_ref_target_from_key` / `UNRESOLVED_EXTERNAL_DEPENDENCY`.
    //
    // This test proves parity end-to-end: the SAME sealed table, run through
    // (a) the table path directly, and (b) a genuine `ir_vcs::raise::
    // raise_view` encode of that table followed by a postcard wire-byte
    // round trip and `build_reference_set_from_payloads`, must yield the
    // same External edge set. If this ever regresses, either the encode
    // (`raise_type_ref`/`raise_type_wire`) or the harvest
    // (`build_reference_set_from_payloads`/`KindWire::for_each_type_ref`)
    // has drifted out of sync with the table path.
    #[test]
    fn table_path_and_payload_harvest_reach_parity_on_unresolved_cross_package_refs() {
        use crate::server::registry::blob::RefTarget;
        use ir::build::{
            Alias, EcosystemId, Impl, Lowering, PackageId, PackageLineageId, PackageName, Record,
            Symbol, Type, Visibility,
        };
        use ir::foreign::{ForeignKey, Unlinked};
        use ir::kinds::UnknownType;
        use ir_vcs::wire::OwnedEntryPayload;

        fn sym(name: &str) -> Symbol {
            Symbol {
                name: name.to_owned(),
                visibility: Visibility::Public,
                documentation: String::new(),
                source: std::path::PathBuf::from("src/lib.rs"),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            }
        }

        // Same package shape as `in_process_sealed_table_yields_real_reference_edges`
        // (an unlinked `Ref::Foreign` via an `impl ... for` clause) plus an
        // `UnknownType::UnresolvedExternal` mention, same as
        // `unresolved_external_type_mentions_become_external_ref_edges` — both
        // gaps the table path already closed and the payload-harvest fix now
        // closes on the wire path too.
        let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
        let core =
            PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"));
        let mut low: Lowering<&'static str> =
            Lowering::new(PackageId::path("fixture"), sym("fixture"));
        let self_ref = low.refer::<Record>("Widget");
        low.declare("Widget", None, sym("Widget"), Record::builder().build());
        let of = low.nominal_import(ForeignKey::in_package(
            core.clone(),
            "core::clone::Clone",
            "Clone",
        ));
        low.declare(
            "impl#clone",
            None,
            sym("impl Clone for Widget"),
            Impl::builder()
                .of(of)
                .self_ty(Type::Nominal(self_ref.into_raw()))
                .build(),
        );
        low.declare(
            "External",
            None,
            sym("External"),
            Alias::builder()
                .target(Type::Unknown(UnknownType::UnresolvedExternal {
                    name: "numpy.ndarray".to_owned(),
                }))
                .build(),
        );

        let table = low
            .finish()
            .expect("lowering must succeed")
            .seal(&lineage, &Unlinked)
            .table;

        // ── Table path (macOS / in-process) ──────────────────────────────────
        let table_refs =
            build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));
        let mut table_externals: Vec<(String, String)> = table_refs
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .filter_map(|r| match &r.target {
                RefTarget::External { path, dependency } => {
                    Some((path.clone(), dependency.clone()))
                }
                RefTarget::Local(_) => None,
            })
            .collect();
        table_externals.sort();
        assert_eq!(
            table_externals.len(),
            2,
            "the table path must see both the unlinked Ref::Foreign (Clone) and \
             the UnresolvedExternal mention (numpy.ndarray) as External edges; \
             got {table_externals:?}"
        );

        // ── Payload-harvest path (cage / Linux): genuine encode → wire bytes →
        // decode → harvest ───────────────────────────────────────────────────
        //
        // `raise_view` is the same in-repo encoder (`ir_vcs::raise`) that
        // turns a sealed `Entry` into wire `OwnedEntryPayload`s for local-store
        // persistence — the same `raise_type_ref`/`raise_type_wire` this test
        // is proving now carry the unresolved cases. The out-of-repo cage
        // guest producer is a separate binary (see `cage.rs`'s module docs),
        // but it constructs the identical wire types from the identical
        // sealed-table data; this is the genuine in-repo half of that
        // round trip, plus a real postcard encode/decode proving the new
        // variants actually survive the wire, not just the in-memory raise.
        let view = ir::view::IrView::new(table);
        let payload_table = ir_vcs::raise::raise_view(&view);
        let payloads: Vec<OwnedEntryPayload> = payload_table
            .live_entries()
            .map(|(_, payload)| {
                let bytes = postcard::to_allocvec(payload).expect("payload postcard-encodes");
                postcard::from_bytes(&bytes).expect("payload postcard-decodes")
            })
            .collect();

        let payload_refs = build_reference_set_from_payloads(&payloads);
        let mut payload_externals: Vec<(String, String)> = payload_refs
            .by_file
            .iter()
            .flat_map(|f| f.references.iter())
            .filter_map(|r| match &r.target {
                RefTarget::External { path, dependency } => {
                    Some((path.clone(), dependency.clone()))
                }
                RefTarget::Local(_) => None,
            })
            .collect();
        payload_externals.sort();

        assert_eq!(
            table_externals, payload_externals,
            "the table path and the payload-harvest (genuine encode → wire → \
             decode) path must report the SAME External edge set for the same \
             package — this is the C2 host-divergence parity fix"
        );
    }
}
