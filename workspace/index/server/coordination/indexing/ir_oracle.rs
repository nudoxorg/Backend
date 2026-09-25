//! Oracle references and usage occurrences, two readings of one walk.

use std::collections::{BTreeMap, HashMap};

use crate::server::registry::blob::creation::BlobBuilder;

/// Derive a [`ReferenceSet`] from the accumulated `Bodies` frames.
///
/// See `ingest_ir_bytes` for the policy rationale. This is a pure function
/// (no I/O) so it can be unit-tested independently.
///
/// # Policy
/// - `OracleCall` with `target: Some(stable_ref)` → `Reference` with `kind =
///   stable_ref.kind as u8`, span from `rel_span`.
/// - `OracleTypeMention` → `Reference` with kind discriminant for
///   `ReferenceKind::TypeReference` (2).
/// - `RefTarget::Local(intro_hex)` when `stable_ref.package` matches
///   `owning_pkg` (same-package reference). Otherwise `RefTarget::External {
///   path: intro_hex, dependency: "eco:name" }`.
/// - References are grouped by the owning entry's `source_path`; entries whose
///   `intro` is not in `intro_to_path` are grouped under the sentinel path
///   `"<unknown>"` (body arrived without a matching Symbol frame).
pub(super) fn build_reference_set_from_bodies(
    bodies: &[ir_vcs::protocol::BodyWire],
    intro_to_path: &HashMap<ir::change::IntroId, String>,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::Reference;
    use smol_str::SmolStr;

    // Per-file accumulator: file_path → Vec<Reference>.
    let mut by_file: BTreeMap<SmolStr, Vec<Reference>> = BTreeMap::new();
    visit_oracle_edges(bodies, |intro, stable, kind, _confidence, span| {
        let src_path = intro_to_path
            .get(&intro)
            .map(|path| SmolStr::from(path.as_str()))
            .unwrap_or_else(|| SmolStr::new("<unknown>"));
        by_file.entry(src_path).or_default().push(Reference {
            target: make_ref_target(stable, owning_pkg),
            span_start: span.start as u64,
            span_end: span.end as u64,
            kind: kind as u8,
        });
    });
    seal_references(by_file)
}

/// Graph-worthy oracle facts, once. Both the reference section and the
/// usage-query occurrences are this walk.
pub(super) fn visit_oracle_edges(
    bodies: &[ir_vcs::protocol::BodyWire],
    mut visit: impl FnMut(
        ir::change::IntroId,
        &ir::change::StableRef,
        ir::vocab::ReferenceKind,
        ir::vocab::Confidence,
        ir::vocab::RelSpan,
    ),
) {
    use ir::{
        body::BodyEmbed,
        vocab::{Confidence, ReferenceKind},
    };

    for body in bodies {
        let BodyEmbed::Present(facts) = &body.body else {
            continue;
        };
        for call in &facts.oracle.calls {
            let Some(target) = call.target.as_ref() else {
                continue;
            };
            if !call.confidence.is_graph_worthy() {
                continue;
            }
            visit(
                body.intro,
                target,
                call.kind,
                call.confidence,
                call.rel_span,
            );
        }
        for mention in &facts.oracle.type_mentions {
            visit(
                body.intro,
                &mention.ty,
                ReferenceKind::TypeReference,
                Confidence::Oracle,
                mention.rel_span,
            );
        }
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
///   `target` and its confidence is graph-worthy (`>= Confidence::Index`); its
///   own reported `kind`/`confidence`/`rel_span` are carried through unchanged
///   — an `Occurrence` has a confidence field to record exactly this, unlike
///   the blob `Reference` wire shape.
/// - `OracleTypeMention` has no confidence field of its own (every mention the
///   oracle records is already a resolved fact, never a partial one), so it is
///   recorded at `Confidence::Oracle` — the tier that name reflects — with
///   `ReferenceKind::TypeReference`.
/// - The owner is `bw.intro` (the entry the body belongs to) in both cases.
///
/// Tree-sitter-only call facts (`BodyEmbed::Present(_).treesitter`) carry no
/// stable cross-package target and are skipped, same as the reference-set
/// projection.
pub(super) fn build_occurrences_from_bodies(
    bodies: &[ir_vcs::protocol::BodyWire],
) -> Vec<(ir::change::IntroId, ir::vocab::Occurrence)> {
    use ir::vocab::Occurrence;

    let mut occurrences = Vec::new();
    visit_oracle_edges(bodies, |intro, target, kind, confidence, span| {
        occurrences.push((
            intro,
            Occurrence::new(target.clone(), kind, confidence, span),
        ));
    });
    occurrences
}

/// Map a [`ir::change::StableRef`] to a
/// [`crate::server::registry::blob::RefTarget`].
///
/// The `path` component is the target symbol's `IntroId` in hex — the durable,
/// wire-stable identity the IR plane uses. Source file paths for target symbols
/// are not available at this stage (the host would need to look them up from
/// the IR store, which is an async operation not available in the blocking
/// decode path). The `intro_hex` is sufficient for cross-reference graph edges;
/// the IR graph adapter resolves it to a file path on demand.
pub(super) fn make_ref_target(
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
/// path ([`super::super::compile_inprocess`]) needs an empty IR section
/// *alongside a real reference section*: there is no forward semantic→wire
/// encoder in the workspace to rebuild a faithful `Vec<OwnedEntryPayload>` from
/// the sealed table, and nothing decodes this section as payloads yet (only its
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
/// the workspace to rebuild the NdIrF1 `Symbols`/`Bodies` stream from it (so
/// the cage's [`ingest_ir_bytes`] byte-decode path cannot be reused — see
/// [`super::super::compile_inprocess`]). References are therefore recovered
/// from the one artifact the in-process path *does* have: the sealed table's
/// own cross-symbol `Ref` edges — every type nominal a declaration mentions in
/// its signature / fields / impl-of, already lowered by `seal` to `Ref::Intro`
/// (same-package) or `Ref::Foreign` (cross-package).
///
/// It reuses the exact [`RefTarget`](crate::server::registry::blob::RefTarget)
/// / [`Reference`](crate::server::registry::blob::Reference) /
/// [`ReferenceSet`](crate::server::registry::blob::ReferenceSet) blob
/// vocabulary and the [`make_ref_target`] mapping the cage path uses, so both
/// hosts land in one on-disk reference format that `save::blobs`'s
/// `ReferenceSet::decode` audit and the future reverse-`occ` index read
/// identically.
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
/// naming this entry's own parent or a child is dropped. What remains is
/// exactly the type-nominal usage graph.
pub(in crate::server::coordination) fn build_reference_set_from_table(
    table: &ir::apply::PristineIntroTable,
    source_root: &std::path::Path,
    owning_pkg: Option<&str>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{RefTarget, Reference};
    use ir::{index::Ref, vocab::ReferenceKind};
    use smol_str::SmolStr;
    use std::collections::HashSet;

    let mut by_file: BTreeMap<SmolStr, Vec<Reference>> = BTreeMap::new();

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
    }

    seal_references(by_file)
}

/// Path order is the section order. A `HashMap` would make the postcard bytes
/// depend on iteration, so an unchanged reference graph would hash as new.
pub(super) fn seal_references(
    by_file: BTreeMap<smol_str::SmolStr, Vec<crate::server::registry::blob::Reference>>,
) -> crate::server::registry::blob::ReferenceSet {
    use crate::server::registry::blob::{FileReferences, ReferenceSet};
    ReferenceSet {
        by_file: by_file
            .into_iter()
            .filter(|(_, refs)| !refs.is_empty())
            .map(|(path, references)| FileReferences { path, references })
            .collect(),
    }
}

/// Build an `External` [`RefTarget`](crate::server::registry::blob::RefTarget)
/// for a named-but-unlinked cross-package reference.
///
/// The in-process path seals against [`ir::foreign::Unlinked`], so every
/// foreign ref arrives with `target: None` but a fully-named
/// [`ir::foreign::ForeignKey`]. `dependency` is `ecosystem:name` when the
/// producer could name the owning package, else the bare ecosystem tag
/// (`Namespace`/`Universe` origins that decline to guess a package). `path` is
/// the target's canonical path in its own language's spelling — the durable
/// join key a corpus-side resolver matches on, mirroring the `intro_hex` `path`
/// the linked cage path emits.
pub(super) fn external_ref_target_from_key(
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

/// The in-package-relative source path a reference is grouped under: the
/// entry's declaration source with `source_root` stripped (so it matches the
/// [`FileEntry::path`] keys the builder stages), falling back to the raw path,
/// or the `"<unknown>"` sentinel for a synthesized / unlocated entry (the same
/// sentinel the cage path uses for a body without a matching symbol).
pub(super) fn reference_source_path(
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
