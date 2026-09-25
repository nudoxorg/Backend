//! Decode the cage producer's NdIrF1 IR stream and derive the reference set.

use std::collections::{BTreeMap, HashMap};

use crate::server::registry::blob::creation::BlobBuilder;

use super::generation_root::{StagedSymbol, attach_generation_root};

pub(in crate::server::coordination) use super::generation_root::{
    attach_table_generation_root, prior_entry_keys,
};

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
///   `Vec<ir_vcs::wire::OwnedEntryPayload>` — one entry per `Symbols` batch
///   entry, in stream order. This is what the IR-plane apply/checkout machinery
///   reads.
/// - **References section** (`builder.set_references`): a [`ReferenceSet`]
///   derived from the `Bodies` frames in the stream (INDEX-PLAN §5.1).
///
/// # References derivation (INDEX-PLAN §5.1)
///
/// The `Occurrences` frames carry opaque producer-specific bytes with no
/// public decoder in `ir` or `ir-vcs` (the format is intentionally producer-
/// private and the host records them opaquely — see
/// `stream::StreamedRecording`). The `Bodies` frames, however, carry fully
/// decoded [`ir::body::BodyEmbed`] values with oracle-resolved call targets
/// ([`ir::OracleCall`]) and type mentions ([`ir::OracleTypeMention`]) — both
/// carry a [`ir::change::StableRef`] that identifies the target symbol across
/// packages.
///
/// Policy (documented here, not a §5.1 blocker):
/// - We extract *oracle-resolved* references only (`Confidence >= Index`),
///   since tree-sitter call-name facts have no stable cross-package target.
/// - The `Reference::span_start`/`span_end` are the `RelSpan` bounds relative
///   to the owning entry's declaration span start (not absolute file offsets).
///   This is correct for the use case: the reference store holds relative spans
///   and the caller reconstructs absolute ones from the symbol's `span_start`
///   when needed.
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
/// - a clean EOF that arrives *without* a prior `Finish` (a truncated stream —
///   e.g. the cage or the vsock transport died mid-write);
/// - a frame that fails to decode (framing/postcard corruption);
/// - a host-side failure to serialize the recovered IR or reference sections.
///
/// Whatever was recovered before the break is still staged onto `builder`
/// (partial-data capture for diagnostics) — the caller decides whether to
/// use it, but must not report the job as a clean `Stored` success when
/// `degraded_reason` is `Some`.
pub(super) fn ingest_ir_bytes(
    builder: &mut BlobBuilder,
    ir_bytes: &[u8],
    prior: &[crate::frontier::ir::IrEntryKey],
) -> IrIngestOutcome {
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
            generation: Vec::new(),
            degraded_reason: Some(format!("no Hello frame: {err}")),
        };
    }

    let mut payloads: Vec<ir_vcs::wire::OwnedEntryPayload> = Vec::new();
    let mut staged: Vec<StagedSymbol> = Vec::new();
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
                    staged.push(StagedSymbol {
                        intro,
                        parent: entry.parent,
                        payload: entry.payload.clone(),
                    });
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
                // Occurrences bytes are opaque (producer-private format, no
                // public decoder in ir/ir-vcs) — accumulated in
                // stream.rs as raw bytes for provenance; not
                // decoded here. SourceDigest provenance is
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
                    generation: Vec::new(),
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
                generation: Vec::new(),
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
    let ref_set =
        build_reference_set_from_bodies(&bodies, &intro_to_path, owning_pkg_key.as_deref());
    tracing::debug!(
        by_file = ref_set.by_file.len(),
        total_refs = ref_set
            .by_file
            .iter()
            .map(|f| f.references.len())
            .sum::<usize>(),
        "reference set derived from Bodies frames"
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

    let generation = if degraded_reason.is_none() {
        match attach_generation_root(builder, &staged, owning_package.as_ref(), prior) {
            Ok(keys) => keys,
            Err(err) => {
                tracing::warn!(error = %err, "set_generation_root failed");
                degraded_reason.get_or_insert(err);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

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
        generation,
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
    /// Intro ids and storage hashes written into this generation's root.
    /// Empty when the stream carried no symbols or was degraded. A later
    /// ingest passes this slice as `prior` so unchanged payloads are not
    /// queued.
    pub(super) generation: Vec<crate::frontier::ir::IrEntryKey>,
    pub(super) degraded_reason: Option<String>,
}

#[path = "ir_oracle.rs"]
mod ir_oracle;

pub(in crate::server::coordination) use ir_oracle::{
    attach_empty_ir_sections, build_reference_set_from_table, set_empty_ir_section,
};
use ir_oracle::{build_occurrences_from_bodies, build_reference_set_from_bodies, seal_references};

#[cfg(test)]
#[path = "ir_stream_tests.rs"]
mod tests;
