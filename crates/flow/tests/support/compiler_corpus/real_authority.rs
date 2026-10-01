//! Per-lane authority expectations and authority-vs-IR plane verification.
//!
//! Every real-package row builds an [`AuthorityExpectation`] from the same
//! native authority owner its compile consumed (the oracle image, the checker
//! report, the module facts, …) while that owner is alive.  The expectation
//! states positive existence facts — every authority declaration row must
//! join at least one IR entity, joined shapes must agree, documented and
//! extended entity counts must reach parity, every authority relation target
//! must be witnessed, and the authority's first top-level declaration must
//! render on both the owned and the reopened reader.  The direction is
//! deliberately one-way: the IR legitimately carries synthesized rows
//! (signature carriers, type parameters) the authority never named, so the
//! IR is never required to be a subset of the authority.
//!
//! A plane a lane cannot ground is [`PlaneExpectation::NotCompared`]: a typed
//! counted outcome, never a fabricated success and never a parity mismatch.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::*;
use crate::comparison::RealAuditField;
use crate::observation::{
    ObservedTypeShape, RenderVerdict, digest_bytes, digest_render, digest_type_shape, digest_typed,
    digest_u64, observe_entity_at_source, render_neutral, render_neutral_reader, type_shape_reader,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PlaneExpectation<T> {
    /// The lane's authority grounds this plane with the carried facts.
    Grounded(T),
    /// No authority producer on this lane can ground the plane. Counted, red
    /// for verification, never treated as parity.
    NotCompared,
}

impl<T> Default for PlaneExpectation<T> {
    fn default() -> Self {
        Self::NotCompared
    }
}

impl<T> PlaneExpectation<T> {
    fn as_grounded(&self) -> Option<&T> {
        match self {
            Self::Grounded(facts) => Some(facts),
            Self::NotCompared => None,
        }
    }
}

/// One authority declaration row that must exist as at least one IR entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityDeclaration {
    /// The IR entity kind the lane's lowerer derives from this row.
    pub(super) kind: ItemKind,
    /// Exact declared-name bytes owned by the expectation.
    pub(super) name: Box<[u8]>,
    /// Exact declared-name span when the lane's IR attaches source spans;
    /// `None` selects the name-and-kind-only join.
    pub(super) name_span: Option<(u32, u32)>,
}

/// One authority type fact: the joined entity's semantic type must observe
/// exactly this shape class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityTypeRow {
    /// Join key: the entity kind of the owning declaration.
    pub(super) kind: ItemKind,
    /// Join key: the declared-name bytes.
    pub(super) name: Box<[u8]>,
    /// The shape class the joined entity's semantic type must observe.
    pub(super) shape: ObservedTypeShape,
}

/// One authority relation: the target must be witnessed by an occurrence or
/// link, and — when the lane carries occurrence source spans — at its exact
/// use site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityRelation {
    /// Resolved target name bytes (local entity name or foreign display).
    pub(super) target: Box<[u8]>,
    /// Half-open source byte span of the use site.
    pub(super) site: (u32, u32),
    /// Whether the lane's IR carries occurrence source spans; when `false`
    /// the site only identifies the row and the join is by target name.
    pub(super) located: bool,
}

/// The authority's first top-level declaration of the selected file: the
/// seed entity for the canonical-type and render planes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AuthorityPrimary {
    pub(super) kind: ItemKind,
    pub(super) name: Box<[u8]>,
    pub(super) name_span: Option<(u32, u32)>,
}

/// Every authority-grounded observer plane for one real-package row.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AuthorityExpectation {
    pub(super) declarations: PlaneExpectation<Vec<AuthorityDeclaration>>,
    pub(super) types: PlaneExpectation<Vec<AuthorityTypeRow>>,
    pub(super) relations: PlaneExpectation<Vec<AuthorityRelation>>,
    pub(super) documentation: PlaneExpectation<u32>,
    pub(super) extensions: PlaneExpectation<u32>,
    /// Census root-entity parity: the authority's top-level declaration count.
    pub(super) discovery: PlaneExpectation<u32>,
    /// Seeds both the canonical-type and the render planes.
    pub(super) primary: PlaneExpectation<AuthorityPrimary>,
}

/// Slices one exact half-open span out of the source bytes.
fn slice_span(source: &str, span: (u32, u32)) -> Option<Vec<u8>> {
    let start = usize::try_from(span.0).ok()?;
    let end = usize::try_from(span.1).ok()?;
    Some(source.as_bytes().get(start..end)?.to_vec())
}

fn not_compared_plane(
    case: inventory::RealPackageCase,
    field: RealAuditField,
    unavailable: &mut Vec<CorpusMismatch>,
) {
    unavailable.push(CorpusMismatch::RealUnavailable {
        case,
        field,
        cause: AuthorityUnavailableCause::NotCompared,
    });
}

fn real_mismatch(
    case: inventory::RealPackageCase,
    field: RealAuditField,
    expected: Digest,
    observed: Digest,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    mismatches.push(CorpusMismatch::Real {
        case,
        field,
        expected,
        observed,
    });
}

/// Count of IR entities whose declared kind and exact name match the row.
fn name_join<R: SemanticReader + ?Sized>(reader: &R, kind: ItemKind, name: &[u8]) -> u32 {
    u32::try_from(
        reader
            .canonical_entities()
            .filter(|entity| entity.kind == kind && reader.atom(entity.name) == Some(name))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// Span join through the common reader observation: the entity's source span
/// must contain the declared-name span in the row's own file.
fn span_join<R: SemanticReader + ?Sized>(
    reader: &R,
    kind: ItemKind,
    name: &[u8],
    name_span: (u32, u32),
    file: &[u8],
) -> u32 {
    let (matches, ..) =
        observe_entity_at_source(reader, kind, name, name_span.0, name_span.1, file);
    u32::from(matches)
}

fn declaration_joins<R: SemanticReader + ?Sized>(
    reader: &R,
    row: &AuthorityDeclaration,
    file: &[u8],
) -> u32 {
    match row.name_span {
        Some(name_span) => span_join(reader, row.kind, &row.name, name_span, file),
        None => name_join(reader, row.kind, &row.name),
    }
}

/// Whether some entity joined by the type row observes exactly its shape.
fn type_row_joins<R: SemanticReader + ?Sized>(reader: &R, row: &AuthorityTypeRow) -> bool {
    reader
        .canonical_entities()
        .filter(|entity| entity.kind == row.kind && reader.atom(entity.name) == Some(&row.name[..]))
        .any(|entity| type_shape_reader(reader, entity.semantic_type) == row.shape)
}

/// Resolves whether one link target names exactly the expected bytes: local
/// entities by their declared name, foreign targets by their display atom.
fn target_names<R: SemanticReader + ?Sized>(
    reader: &R,
    target: backend_semantic::ir::LinkTarget,
    expected: &[u8],
) -> bool {
    match target {
        backend_semantic::ir::LinkTarget::Local(entity) => reader
            .entity(entity)
            .is_some_and(|entity| reader.atom(entity.name) == Some(expected)),
        backend_semantic::ir::LinkTarget::External(external) => match reader.external(external) {
            Some(backend_semantic::ir::ExternalTarget::Foreign(foreign)) => {
                reader.atom(foreign.display) == Some(expected)
            }
            _ => false,
        },
    }
}

/// Whether the authority relation is witnessed by an occurrence at its site
/// (when the lane carries occurrence spans) or by any occurrence or link
/// naming its target.
fn relation_holds<R: SemanticReader + ?Sized>(reader: &R, relation: &AuthorityRelation) -> bool {
    let target = &relation.target[..];
    if relation.located {
        return reader.link_occurrences().any(|(_, occurrence)| {
            let site = occurrence.source.is_some_and(|span| {
                span.start() == relation.site.0 && span.end() == relation.site.1
            });
            site && reader
                .link(occurrence.link)
                .is_some_and(|link| target_names(reader, link.target, target))
        });
    }
    reader.link_occurrences().any(|(_, occurrence)| {
        reader
            .link(occurrence.link)
            .is_some_and(|link| target_names(reader, link.target, target))
    }) || reader
        .canonical_links()
        .any(|(_, link)| target_names(reader, link.target, target))
}

fn documented_entities<R: SemanticReader + ?Sized>(reader: &R) -> u32 {
    u32::try_from(
        reader
            .canonical_entities()
            .filter(|entity| entity.authority.documentation == FactAvailability::Captured)
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn extended_entities<R: SemanticReader + ?Sized>(reader: &R) -> u32 {
    u32::try_from(
        reader
            .canonical_entities()
            .filter(|entity| entity.authority.language_extension == FactAvailability::Captured)
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// The census's authority-proven root count: entities whose parentage the
/// authority marked Root. (The structural `root_entities` cell additionally
/// counts parentless synthesized carriers, which the authority never
/// predicts.)
fn census_roots<R: SemanticReader + ?Sized>(reader: &R) -> Option<u32> {
    Some(
        u32::try_from(
            backend_semantic::ir::SemanticImageDiscovery::new(reader)
                .census()
                .ok()?
                .entity_authority
                .parentage
                .roots,
        )
        .unwrap_or(u32::MAX),
    )
}

/// Joins the primary seed to its entity, preferring the span join.
fn join_primary<R: SemanticReader + ?Sized>(
    reader: &R,
    primary: &AuthorityPrimary,
    file: &[u8],
) -> Option<EntityId> {
    if let Some(name_span) = primary.name_span {
        let (matches, observed, ..) = observe_entity_at_source(
            reader,
            primary.kind,
            &primary.name,
            name_span.0,
            name_span.1,
            file,
        );
        if matches == 0 {
            return None;
        }
        return observed.map(|entity| entity.id);
    }
    reader
        .canonical_entities()
        .find(|entity| {
            entity.kind == primary.kind && reader.atom(entity.name) == Some(&primary.name[..])
        })
        .map(|entity| entity.id)
}

/// Verifies every authority-grounded observer plane against both the owned
/// and the reopened reader.  Returns the number of not-compared audit fields.
#[allow(
    clippy::too_many_arguments,
    reason = "the audit passes its typed sinks and expectation together"
)]
pub(super) fn check_authority_planes<R: SemanticReader + ?Sized>(
    ir: &Ir,
    reopened: &R,
    file: &[u8],
    case: inventory::RealPackageCase,
    expectation: &AuthorityExpectation,
    unavailable: &mut Vec<CorpusMismatch>,
    mismatches: &mut Vec<CorpusMismatch>,
) -> usize {
    let mut not_compared = 0_usize;
    // Declarations: every authority row joins at least one IR entity on both
    // readers, with the kind agreed by the join itself.
    let ts_scratch = std::env::var("NUDOX_SCRATCH_TS").is_ok();
    match expectation.declarations.as_grounded() {
        Some(rows) => {
            for row in rows {
                if declaration_joins(ir, row, file) == 0
                    || declaration_joins(reopened, row, file) == 0
                {
                    if ts_scratch {
                        eprintln!(
                            "SCRATCH-DECL coord={:?} kind={:?} name={:?} span={:?}",
                            case.coordinate.raw(),
                            row.kind,
                            String::from_utf8_lossy(&row.name),
                            row.name_span,
                        );
                    }
                    real_mismatch(
                        case,
                        RealAuditField::Declarations,
                        digest_bytes(&row.name),
                        digest_u64(u64::from(declaration_joins(reopened, row, file))),
                        mismatches,
                    );
                }
            }
        }
        None => {
            not_compared_plane(case, RealAuditField::Declarations, unavailable);
            not_compared += 1;
        }
    }
    // Types: the joined entity's observed shape class must equal the row's.
    match expectation.types.as_grounded() {
        Some(rows) => {
            for row in rows {
                if !type_row_joins(ir, row) || !type_row_joins(reopened, row) {
                    real_mismatch(
                        case,
                        RealAuditField::Types,
                        digest_type_shape(row.shape),
                        digest_bytes(&row.name),
                        mismatches,
                    );
                }
            }
        }
        None => {
            not_compared_plane(case, RealAuditField::Types, unavailable);
            not_compared += 1;
        }
    }
    // Relations: every authority target is witnessed on both readers.
    let ts_scratch = std::env::var("NUDOX_SCRATCH_TS").is_ok();
    match expectation.relations.as_grounded() {
        Some(rows) => {
            for row in rows {
                if !relation_holds(ir, row) || !relation_holds(reopened, row) {
                    if ts_scratch {
                        let located_holds = relation_holds(reopened, &AuthorityRelation {
                            target: row.target.clone(),
                            site: row.site,
                            located: true,
                        });
                        let name_only_holds = relation_holds(reopened, &AuthorityRelation {
                            target: row.target.clone(),
                            site: row.site,
                            located: false,
                        });
                        let mut spans: Vec<(u32, u32)> = reopened
                            .link_occurrences()
                            .filter_map(|(_, occurrence)| {
                                let names = reopened.link(occurrence.link).is_some_and(|link| {
                                    target_names(reopened, link.target, &row.target[..])
                                });
                                names.then(|| {
                                    occurrence
                                        .source
                                        .map(|span| (span.start(), span.end()))
                                        .unwrap_or((u32::MAX, u32::MAX))
                                })
                            })
                            .take(4)
                            .collect();
                        spans.sort_unstable();
                        eprintln!(
                            "SCRATCH-REL coord={:?} target={:?} site={:?} located={} located_holds={located_holds} name_only_holds={name_only_holds} witness_spans={spans:?}",
                            case.coordinate.raw(),
                            String::from_utf8_lossy(&row.target),
                            row.site,
                            row.located,
                        );
                    }
                    real_mismatch(
                        case,
                        RealAuditField::Relations,
                        digest_typed(&(row.target.to_vec(), row.site)),
                        digest_u64(0),
                        mismatches,
                    );
                }
            }
        }
        None => {
            not_compared_plane(case, RealAuditField::Relations, unavailable);
            not_compared += 1;
        }
    }
    // Documentation and extensions: captured-entity count parity.
    let count_planes = [
        (
            expectation.documentation.as_grounded(),
            RealAuditField::Documentation,
            documented_entities(ir),
            documented_entities(reopened),
        ),
        (
            expectation.extensions.as_grounded(),
            RealAuditField::Extensions,
            extended_entities(ir),
            extended_entities(reopened),
        ),
    ];
    for (grounded, field, owned_count, reopened_count) in count_planes {
        match grounded {
            Some(expected) => {
                if owned_count != *expected || reopened_count != *expected {
                    real_mismatch(
                        case,
                        field,
                        digest_u64(u64::from(*expected)),
                        digest_u64(u64::from(reopened_count)),
                        mismatches,
                    );
                }
            }
            None => {
                not_compared_plane(case, field, unavailable);
                not_compared += 1;
            }
        }
    }
    // Discovery: census root-entity parity on both readers.
    match expectation.discovery.as_grounded() {
        Some(expected) => {
            if census_roots(ir) != Some(*expected) || census_roots(reopened) != Some(*expected) {
                real_mismatch(
                    case,
                    RealAuditField::Discovery,
                    digest_u64(u64::from(*expected)),
                    digest_u64(u64::from(census_roots(ir).unwrap_or(0))),
                    mismatches,
                );
            }
        }
        None => {
            not_compared_plane(case, RealAuditField::Discovery, unavailable);
            not_compared += 1;
        }
    }
    // Canonical type and render: the authority's first top-level declaration
    // must join on both readers, render its canonical type identically on
    // both, and carry a rendered neutral signature.
    match expectation.primary.as_grounded() {
        Some(primary) => {
            let owned_primary = join_primary(ir, primary, file);
            let reopened_primary = join_primary(reopened, primary, file);
            let owned_canonical = owned_primary.map(|id| render_neutral_reader(ir, Some(id)));
            let reopened_canonical =
                reopened_primary.map(|id| render_neutral_reader(reopened, Some(id)));
            match (owned_canonical, reopened_canonical) {
                (Some(owned), Some(reopened)) => {
                    if owned != reopened || !matches!(owned, RenderVerdict::Rendered(_)) {
                        real_mismatch(
                            case,
                            RealAuditField::CanonicalType,
                            digest_render(owned),
                            digest_render(reopened),
                            mismatches,
                        );
                    }
                }
                (owned, reopened) => {
                    real_mismatch(
                        case,
                        RealAuditField::CanonicalType,
                        digest_render(owned.unwrap_or(RenderVerdict::Unavailable)),
                        digest_render(reopened.unwrap_or(RenderVerdict::Unavailable)),
                        mismatches,
                    );
                }
            }
            // The neutral signature plane renders the owned display signature
            // through the lane renderer when the primary joins uniquely by
            // span, and through the reader renderer otherwise; both the owned
            // and the reopened verdict must have rendered.
            let owned_observations = primary.name_span.map(|name_span| {
                let (matches, observed, ..) = observe_entity_at_source(
                    ir,
                    primary.kind,
                    &primary.name,
                    name_span.0,
                    name_span.1,
                    file,
                );
                (matches == 1).then_some(observed).flatten()
            });
            let owned_neutral = match owned_observations.flatten() {
                Some(observed) => render_neutral(ir, Some(observed)),
                None => owned_primary
                    .map(|id| render_neutral_reader(ir, Some(id)))
                    .unwrap_or(RenderVerdict::Unavailable),
            };
            if !matches!(owned_neutral, RenderVerdict::Rendered(_)) {
                real_mismatch(
                    case,
                    RealAuditField::Render,
                    digest_u64(1),
                    digest_render(owned_neutral),
                    mismatches,
                );
            }
            let reopened_neutral = reopened_primary
                .map(|id| render_neutral_reader(reopened, Some(id)))
                .unwrap_or(RenderVerdict::Unavailable);
            if !matches!(reopened_neutral, RenderVerdict::Rendered(_)) {
                real_mismatch(
                    case,
                    RealAuditField::Render,
                    digest_u64(1),
                    digest_render(reopened_neutral),
                    mismatches,
                );
            }
        }
        None => {
            not_compared_plane(case, RealAuditField::CanonicalType, unavailable);
            not_compared_plane(case, RealAuditField::Render, unavailable);
            not_compared += 2;
        }
    }
    not_compared
}

// ── Per-lane producers ────────────────────────────────────────────────────────

/// True when any written `#[cfg(…)]` (or `#[cfg_attr(…, cfg(…))]`) on one
/// inline module evaluates false under the crate's resolved options.
///
/// Mirror of the Rust lane's own law (`crates/engine/src/driver/lower/
/// rust.rs::module_cfg_disabled`): rust-analyzer projects a `#[cfg]`-disabled
/// module through the written syntax walk even though it omits every other
/// cfg-disabled item from HIR, and its module lookup resolves both same-name
/// branches to the one enabled definition. An unprovable predicate never
/// disables.
fn rust_module_cfg_disabled(
    item: &backend_frontend_rust::legacy::ra_ap_syntax::ast::Module,
    cfg: &backend_frontend_rust::legacy::ra_ap_hir::CfgOptions,
) -> bool {
    use backend_frontend_rust::legacy::ra_ap_syntax::{AstNode, ast::HasAttrs};
    item.attrs().any(|attr| {
        attr.meta()
            .is_some_and(|meta| rust_meta_cfg_disabled(&meta, cfg))
    })
}

/// Mirror of the lane's `meta_cfg_disabled`: a `cfg(pred)` whose predicate is
/// provably false, or a `cfg_attr(cond, …)` whose condition is provably true
/// and which applies a disabling meta, disables its item.
fn rust_meta_cfg_disabled(
    meta: &backend_frontend_rust::legacy::ra_ap_syntax::ast::Meta,
    cfg: &backend_frontend_rust::legacy::ra_ap_hir::CfgOptions,
) -> bool {
    use backend_frontend_rust::legacy::ra_ap_syntax::ast;
    match meta {
        ast::Meta::CfgMeta(meta) => meta.cfg_predicate().is_some_and(|predicate| {
            cfg.check(&backend_frontend_rust::legacy::ra_ap_hir::CfgExpr::parse_from_ast(predicate))
                == Some(false)
        }),
        ast::Meta::CfgAttrMeta(meta) => {
            let condition = meta.cfg_predicate().is_some_and(|predicate| {
                cfg.check(
                    &backend_frontend_rust::legacy::ra_ap_hir::CfgExpr::parse_from_ast(predicate),
                ) == Some(true)
            });
            condition
                && meta
                    .metas()
                    .any(|inner| rust_meta_cfg_disabled(&inner, cfg))
        }
        _ => false,
    }
}

/// Clones the source crate's resolved conditional-compilation options, owned
/// so the mirror's written walk can hold them. Mirror of the lane's
/// `crate_cfg_options`: the options already include the caller's Cargo
/// feature policy and target, so they evaluate exactly as rust-analyzer's
/// own HIR did.
fn rust_crate_cfg_options(
    authority: &backend_frontend_rust::legacy::RustAuthority<'_>,
) -> backend_frontend_rust::legacy::ra_ap_hir::CfgOptions {
    authority
        .semantics
        .hir_file_to_module_def(authority.source_file)
        .map(|root| {
            root.krate(authority.database)
                .cfg(authority.database)
                .clone()
        })
        .unwrap_or_default()
}

/// Runs the non-escaping rust-analyzer authority transaction and collects the
/// lane's owned authority rows.  Written rows join by their exact name span
/// inside the declaration extent the lane attaches; module-walk rows without
/// a written name token (positional tuple fields) stay name-only.  The
/// spans still ground the documentation and discovery counts.
///
/// The written walk also mirrors the lane's cfg law: a `#[cfg]`-disabled
/// inline module is dropped subtree and all. rust-analyzer projects a
/// disabled module through the written syntax walk while omitting it from
/// HIR (and from the module-scope walk), so without the mirror's filter the
/// expectation would predict rows the lane honestly drops — fleet crates
/// dominated by cfg-gated modules (`windows_x86_64_gnullvm`) would be
/// dominated by false convictions.
pub(super) fn rust_authority(
    project: &backend_frontend_rust::legacy::RustProject,
    cancelled: &AtomicBool,
    deadline: Instant,
    source: &[u8],
) -> AuthorityExpectation {
    let mut expectation = AuthorityExpectation::default();
    let control = backend_frontend_rust::legacy::RustAnalysisControl {
        cancelled,
        maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit(
            u32::try_from(source.len()).unwrap_or(u32::MAX),
        ),
        deadline,
    };
    let rows = project.analyze(control, |authority| {
        use backend_frontend_rust::legacy::SemanticKind;
        use backend_frontend_rust::legacy::ra_ap_syntax::{AstNode, ast};
        let mut rows = Vec::new();
        let mut covered_fields = Vec::<backend_frontend_rust::legacy::ra_ap_hir::Field>::new();
        let mut covered_impls = Vec::<backend_frontend_rust::legacy::ra_ap_hir::Impl>::new();
        let mut covered_defs = Vec::<backend_frontend_rust::legacy::ra_ap_hir::ModuleDef>::new();
        // The cfg law is evaluated against the crate's real resolved options
        // (feature policy and target included), exactly as the lane's HIR
        // did, and every dropped module's whole syntactic subtree goes with
        // it: the nested items are syntactically present but own no HIR
        // declaration.
        let crate_cfg = rust_crate_cfg_options(&authority);
        let mut disabled_spans: Vec<(u32, u32)> = Vec::new();
        for declaration in authority.declarations() {
            let Some(kind) = rust_kind(declaration.kind) else {
                continue;
            };
            let Ok(extent) = authority.span(&declaration.syntax) else {
                continue;
            };
            // A `#[cfg]`-disabled module is dropped, subtree and all,
            // exactly as the lane's written walk drops it and its
            // module-scope walk already omits it.
            if let backend_frontend_rust::legacy::RustDefinition::Module(_) =
                &declaration.definition
                && let Some(item) = ast::Module::cast(declaration.syntax.clone())
                && rust_module_cfg_disabled(&item, &crate_cfg)
            {
                disabled_spans.push((extent.start, extent.end));
                continue;
            }
            if disabled_spans
                .iter()
                .any(|(start, end)| *start <= extent.start && extent.end <= *end)
            {
                continue;
            }
            // An anonymous `const _: () = …;` owns no written name; the lane
            // never pushes it, so the authority never predicts it.
            if declaration.kind == SemanticKind::Constant
                && let Some(item) = ast::Const::cast(declaration.syntax.clone())
                && ast::Const::syntax(&item)
                    .children()
                    .find_map(ast::Name::cast)
                    .is_none()
            {
                continue;
            }
            let Ok(name_span) = rust_declaration_name(&authority, &declaration) else {
                continue;
            };
            let Ok(name) = authority.source_at(name_span) else {
                continue;
            };
            // Covered-definition bookkeeping mirrors the lane's dedup key
            // between its written walk and its module-scope walk.
            match &declaration.definition {
                backend_frontend_rust::legacy::RustDefinition::Field(field) => {
                    covered_fields.push(*field);
                }
                backend_frontend_rust::legacy::RustDefinition::Implementation(implementation) => {
                    covered_impls.push(*implementation);
                }
                other => {
                    if let Some(key) = rust_module_def(other) {
                        covered_defs.push(key);
                    }
                }
            }
            rows.push((
                kind,
                name.to_vec(),
                (extent.start, extent.end),
                Some((name_span.start, name_span.end)),
            ));
        }
        // The module-scope walk adds HIR-provable declarations the written
        // tree does not cast — tuple fields named by their canonical
        // positional spelling and macro-expansion items — deduped against
        // the written walk exactly as the lane dedups it.
        for module_declaration in authority.module_declarations() {
            let backend_frontend_rust::legacy::ModuleDeclaration {
                definition,
                item,
                name,
                ..
            } = module_declaration;
            let backend_frontend_rust::legacy::SourceOrigin::Local(span) = item else {
                continue;
            };
            let covered = match &definition {
                backend_frontend_rust::legacy::RustDefinition::Field(field) => {
                    covered_fields.contains(field)
                }
                backend_frontend_rust::legacy::RustDefinition::Implementation(implementation) => {
                    covered_impls.contains(implementation)
                }
                other => rust_module_def(other).is_some_and(|key| covered_defs.contains(&key)),
            };
            if covered {
                continue;
            }
            let Some(kind) = rust_kind(definition.kind()) else {
                continue;
            };
            // A tuple field's positional name has no written token; its join
            // stays name-only. Named module-walk rows join by their exact
            // name span inside the item extent.
            let name_span = name.map(|name_span| (name_span.start, name_span.end));
            let name: Vec<u8> = match name {
                Some(name_span) => match authority.source_at(name_span) {
                    Ok(bytes) => bytes.to_vec(),
                    Err(_) => continue,
                },
                None => {
                    let backend_frontend_rust::legacy::RustDefinition::Field(field) = &definition
                    else {
                        continue;
                    };
                    let Some(positional) = RUST_TUPLE_FIELD_NAMES.get(usize::from(field.index()))
                    else {
                        continue;
                    };
                    if field.name(authority.database).as_str().as_bytes() != *positional {
                        continue;
                    }
                    (*positional).to_vec()
                }
            };
            rows.push((kind, name, (span.start, span.end), name_span));
        }
        Ok(rows)
    });
    let Ok(rows) = rows else {
        return expectation;
    };
    // Documentation is captured for every pushed declaration row, and a row
    // is a parentage root exactly when no other row's extent strictly
    // contains it — both mirrors of the lane's own projection.
    let roots = rows
        .iter()
        .filter(|(_, _, span, _)| {
            !rows.iter().any(|(_, _, other, _)| {
                other.0 <= span.0 && span.1 <= other.1 && (other.0 < span.0 || span.1 < other.1)
            })
        })
        .count();
    expectation.declarations = PlaneExpectation::Grounded(
        rows.iter()
            .map(|(kind, name, _, name_span)| AuthorityDeclaration {
                kind: *kind,
                name: name.clone().into_boxed_slice(),
                name_span: *name_span,
            })
            .collect(),
    );
    expectation.documentation =
        PlaneExpectation::Grounded(u32::try_from(rows.len()).unwrap_or(u32::MAX));
    expectation.discovery = PlaneExpectation::Grounded(u32::try_from(roots).unwrap_or(u32::MAX));
    if let Some((kind, name, _, _)) = rows.first() {
        expectation.primary = PlaneExpectation::Grounded(AuthorityPrimary {
            kind: *kind,
            name: name.clone().into_boxed_slice(),
            name_span: None,
        });
    }
    expectation
}

/// Mirrors the lane's declaration naming: an implementation is named by its
/// self type's last written path segment (or its whole written self type when
/// the type owns no name segment), and every other declaration by the
/// authority's first-identifier rule. Naming an implementation after its
/// first method or generic parameter would collapse distinct impl blocks.
fn rust_declaration_name<'a>(
    authority: &'a backend_frontend_rust::legacy::RustAuthority,
    declaration: &backend_frontend_rust::legacy::RustDeclaration,
) -> Result<
    backend_frontend_rust::legacy::ByteSpan,
    backend_frontend_rust::legacy::RustAuthorityError,
> {
    use backend_frontend_rust::legacy::SemanticKind;
    use backend_frontend_rust::legacy::ra_ap_syntax::{AstNode, ast};
    if declaration.kind == SemanticKind::Implementation
        && let Some(implementation) = ast::Impl::cast(declaration.syntax.clone())
        && let Some(self_ty) = implementation.self_ty()
    {
        if let ast::Type::PathType(path_type) = &self_ty
            && let Some(segment) = path_type.path().and_then(|path| path.segments().last())
            && let Some(name_ref) = segment.name_ref()
        {
            return authority.span(name_ref.syntax());
        }
        return authority.span(self_ty.syntax());
    }
    authority.declaration_name(declaration)
}

fn rust_kind(kind: backend_frontend_rust::legacy::SemanticKind) -> Option<ItemKind> {
    use backend_frontend_rust::legacy::SemanticKind;
    Some(match kind {
        SemanticKind::Module => ItemKind::Module,
        SemanticKind::Function => ItemKind::Function,
        SemanticKind::Field => ItemKind::Field,
        SemanticKind::Record => ItemKind::Record,
        SemanticKind::Enum => ItemKind::Enum,
        SemanticKind::Trait => ItemKind::Trait,
        SemanticKind::Implementation => ItemKind::Implementation,
        SemanticKind::TypeAlias => ItemKind::Alias,
        SemanticKind::Constant => ItemKind::Constant,
        SemanticKind::Static => ItemKind::Static,
        SemanticKind::Variant => ItemKind::Variant,
        SemanticKind::Macro => ItemKind::Macro,
        SemanticKind::LocalBinding | SemanticKind::GenericParameter | SemanticKind::Builtin => {
            return None;
        }
    })
}

/// Canonical positional names of tuple fields, exactly the spellings Rust
/// itself uses for `.0`-style access and exactly the fold the lane applies
/// to walked fields without a name node.
const RUST_TUPLE_FIELD_NAMES: [&[u8]; 16] = [
    b"0", b"1", b"2", b"3", b"4", b"5", b"6", b"7", b"8", b"9", b"10", b"11", b"12", b"13", b"14",
    b"15",
];

/// The lane's `ModuleDef` dedup key for one written-walk definition: fields,
/// implementations, and macros dedup through their own identity planes, and
/// every other definition dedups through its module-def key.
fn rust_module_def(
    definition: &backend_frontend_rust::legacy::RustDefinition,
) -> Option<backend_frontend_rust::legacy::ra_ap_hir::ModuleDef> {
    use backend_frontend_rust::legacy::{RustDefinition, ra_ap_hir};
    match definition {
        RustDefinition::Field(_) | RustDefinition::Implementation(_) | RustDefinition::Macro(_) => {
            None
        }
        RustDefinition::Variant(variant) => Some(ra_ap_hir::ModuleDef::from(*variant)),
        RustDefinition::Module(module) => Some(ra_ap_hir::ModuleDef::from(*module)),
        RustDefinition::Trait(trait_) => Some(ra_ap_hir::ModuleDef::from(*trait_)),
        RustDefinition::Function(function) => Some(ra_ap_hir::ModuleDef::from(*function)),
        RustDefinition::Record(adt) | RustDefinition::Enum(adt) => {
            Some(ra_ap_hir::ModuleDef::from(*adt))
        }
        RustDefinition::TypeAlias(alias) => Some(ra_ap_hir::ModuleDef::from(*alias)),
        RustDefinition::Constant(constant) => Some(ra_ap_hir::ModuleDef::from(*constant)),
        RustDefinition::Static(static_) => Some(ra_ap_hir::ModuleDef::from(*static_)),
    }
}

/// Reads the Go oracle image the compile consumed and grounds the
/// declarations, types, relations, documentation, extensions, discovery, and
/// primary planes.  Bound rows carry the image's name-token extent, so their
/// declaration join is a span join; rows from sibling files stay name-only
/// because the lane leaves them source-less.
pub(super) fn go_authority(image_bytes: &[u8]) -> AuthorityExpectation {
    use backend_frontend_go::legacy::{DeclarationKind, GoImage, TypeRowKind};
    let mut expectation = AuthorityExpectation::default();
    let Ok(image) = GoImage::open(image_bytes) else {
        return expectation;
    };
    let declarations: Vec<AuthorityDeclaration> = image
        .declarations()
        .filter_map(|row| {
            let row = row.ok()?;
            let name_span = row
                .bound
                .then_some(row.name_span)
                .flatten()
                .map(|(start, end)| (start, end));
            Some(AuthorityDeclaration {
                kind: go_kind(&image, &row),
                name: row.name.to_vec().into_boxed_slice(),
                name_span,
            })
        })
        .collect();
    // Type rows ground only where the projected shape class is independent
    // of platform-dependent primitive spellings: basic-rooted aliases and
    // values stay ungrounded rather than predicted.
    let mut types = Vec::new();
    for index in 0..image.declaration_count() {
        let Ok(declaration) = image.declaration(index) else {
            continue;
        };
        let root = declaration
            .type_root
            .and_then(|root| usize::try_from(root).ok())
            .and_then(|root| image.type_row(root).ok());
        let shape = match declaration.kind {
            DeclarationKind::Type => Some(ObservedTypeShape::Nominal),
            DeclarationKind::Function => Some(match root.map(|row| row.kind) {
                None => ObservedTypeShape::Unknown,
                Some(TypeRowKind::Func) => ObservedTypeShape::Callable,
                Some(_) => ObservedTypeShape::Structural,
            }),
            DeclarationKind::Alias | DeclarationKind::Constant | DeclarationKind::Static => {
                match root {
                    None => Some(ObservedTypeShape::Unknown),
                    Some(root_row) => match root_row.kind {
                        TypeRowKind::Basic => None,
                        TypeRowKind::Named | TypeRowKind::Alias => Some(go_named_root_shape(
                            &image,
                            &root_row,
                            declaration.kind,
                            index,
                        )),
                        TypeRowKind::TypeParam => Some(ObservedTypeShape::Generic),
                        TypeRowKind::Func => Some(ObservedTypeShape::Callable),
                        TypeRowKind::Invalid => Some(ObservedTypeShape::Unknown),
                        _ => Some(ObservedTypeShape::Structural),
                    },
                }
            }
        };
        if let Some(shape) = shape {
            types.push(AuthorityTypeRow {
                kind: go_kind(&image, &declaration),
                name: declaration.name.to_vec().into_boxed_slice(),
                shape,
            });
        }
    }
    let relations = (0..image.reference_count())
        .filter_map(|index| {
            let row = image.reference(index).ok()?;
            // The lane attaches every bound owner's primary-source span and
            // lifts each reference's owner-relative occurrence over it, so a
            // reference whose owner row is bound in the primary source
            // becomes an occurrence at the exact absolute site — the image's
            // own `row.span`. Only a sibling-file (unbound) owner keeps its
            // occurrences position-free; exactly those rows join by target
            // name alone.
            let owner_bound = if row.owner_is_declaration {
                image
                    .declaration(usize::try_from(row.owner_row).ok()?)
                    .is_ok_and(|declaration| declaration.bound)
            } else {
                image
                    .method(usize::try_from(row.owner_row).ok()?)
                    .is_ok_and(|method| method.bound)
            };
            Some(AuthorityRelation {
                target: row.target.to_vec().into_boxed_slice(),
                site: row.span,
                located: owner_bound,
            })
        })
        .collect();
    // Documentation is captured for every declaration, method, and member
    // row the lane actually pushes. Member rows owned by anonymous inline
    // struct rows (nameless compound rows inside another type's layout)
    // have no pushed fact — only a declaration-rooted member run becomes
    // field facts — so they stay outside the captured population.
    let documentation = image
        .declaration_count()
        .saturating_add(image.method_count())
        .saturating_add(go_pushed_member_count(&image));
    expectation.declarations = PlaneExpectation::Grounded(declarations);
    expectation.types = PlaneExpectation::Grounded(types);
    expectation.relations = PlaneExpectation::Grounded(relations);
    expectation.documentation =
        PlaneExpectation::Grounded(u32::try_from(documentation).unwrap_or(u32::MAX));
    // The extension population also includes interface-method-set
    // projections the image does not name as declarations, so an exact count
    // is not predictable from the image alone.
    expectation.extensions = PlaneExpectation::NotCompared;
    expectation.discovery =
        PlaneExpectation::Grounded(u32::try_from(image.declaration_count()).unwrap_or(u32::MAX));
    if let Some((kind, name)) = (0..image.declaration_count())
        .find_map(|index| image.declaration(index).ok())
        .map(|declaration| {
            (
                go_kind(&image, &declaration),
                declaration.name.to_vec().into_boxed_slice(),
            )
        })
    {
        expectation.primary = PlaneExpectation::Grounded(AuthorityPrimary {
            kind,
            name,
            name_span: None,
        });
    }
    expectation
}

/// The lane's canonical kind for one declaration row: an interface-rooted
/// defined type projects as the trait, every other defined type as the
/// record — the same underlying-row kind selection the lane's lowerer
/// applies in pass one.
fn go_kind(
    image: &backend_frontend_go::legacy::GoImage<'_>,
    declaration: &backend_frontend_go::legacy::Declaration<'_>,
) -> ItemKind {
    use backend_frontend_go::legacy::{DeclarationKind as Kind, TypeRowKind};
    match declaration.kind {
        Kind::Type => {
            let interface = declaration
                .type_root
                .and_then(|root| usize::try_from(root).ok())
                .and_then(|root| image.type_row(root).ok())
                .is_some_and(|row| row.kind == TypeRowKind::Interface);
            if interface {
                ItemKind::Trait
            } else {
                ItemKind::Record
            }
        }
        Kind::Alias => ItemKind::Alias,
        Kind::Function => ItemKind::Function,
        Kind::Constant => ItemKind::Constant,
        Kind::Static => ItemKind::Static,
    }
}

/// Counts the member rows the lane pushes: exactly the members whose owner
/// type row is a declaration's own root row (`type_family` walks only the
/// root's member run). Members of nested anonymous compound rows — inline
/// struct rows emitted beneath another type — never become facts, so they
/// carry no captured documentation.
fn go_pushed_member_count(image: &backend_frontend_go::legacy::GoImage<'_>) -> usize {
    use std::collections::HashSet;
    let roots: HashSet<usize> = (0..image.declaration_count())
        .filter_map(|index| {
            image
                .declaration(index)
                .ok()
                .and_then(|declaration| declaration.type_root)
                .and_then(|root| usize::try_from(root).ok())
        })
        .collect();
    (0..image.member_count())
        .filter(|index| {
            image.member(*index).is_ok_and(|member| {
                usize::try_from(member.owner).is_ok_and(|owner| roots.contains(&owner))
            })
        })
        .count()
}

/// The shape class the lane projects for one alias/value's named type root
/// (`named_root` in the lane's own projection): a pure same-package name
/// resolves to its nominal fact only when that name is already a pushed
/// declaration — pass-one types and aliases, or an earlier alias for a
/// later alias — and a generic application keeps the base nominal's
/// resolution but applies arguments. Every foreign, universe, or
/// not-yet-pushed name folds to the typed `Unknown` that retains its
/// spelling, because an out-of-root type carries no honest spelling cell.
fn go_named_root_shape(
    image: &backend_frontend_go::legacy::GoImage<'_>,
    root: &backend_frontend_go::legacy::TypeRow<'_>,
    kind: backend_frontend_go::legacy::DeclarationKind,
    index: usize,
) -> ObservedTypeShape {
    use backend_frontend_go::legacy::DeclarationKind;
    let resolves_locally = !root.package.is_empty()
        && (0..image.declaration_count()).any(|candidate| {
            let Ok(known) = image.declaration(candidate) else {
                return false;
            };
            known.name == root.name
                && known.package == root.package
                && match (known.kind, kind) {
                    // Pass one records every type before any alias or value.
                    (DeclarationKind::Type, _) => true,
                    // Aliases are recorded in image order during pass one, so
                    // a later alias cannot resolve backward to an earlier
                    // alias that follows it.
                    (DeclarationKind::Alias, DeclarationKind::Alias) => candidate < index,
                    // Pass two values see every pass-one name.
                    (DeclarationKind::Alias, DeclarationKind::Constant)
                    | (DeclarationKind::Alias, DeclarationKind::Static) => true,
                    _ => false,
                }
        });
    if !resolves_locally {
        return ObservedTypeShape::Unknown;
    }
    if root.children.1 == 0 {
        ObservedTypeShape::Nominal
    } else {
        ObservedTypeShape::Generic
    }
}

/// Reads the javac image and grounds the declarations plane with the same
/// explicit-origin filter and kind map the Java lowerer applies.  Binding
/// units carry each declaration's own UTF-16 extent, projected onto the
/// source's byte domain exactly as the lane projects it, so explicit rows
/// join by span; extentless rows (packages, modules, non-binding units)
/// stay name-only.
pub(super) fn java_authority(image_bytes: &[u8], source: &[u8]) -> AuthorityExpectation {
    use backend_frontend_java::legacy::{JavaAuthorityImage, Origin};
    let mut expectation = AuthorityExpectation::default();
    // The compile consumes a source-bound envelope; the typed javac planes
    // live inside it.
    let Ok(envelope) = JavaAuthorityImage::open(image_bytes) else {
        return expectation;
    };
    let image = envelope.image;
    let mut declarations = Vec::new();
    let mut roots = 0_u32;
    let mut documented = 0_u32;
    let mut primary = None;
    for (index, row) in image.declarations().enumerate() {
        let Ok(row) = row else { continue };
        // Roots and documentation mirror the lane's own projection, which
        // does not filter by origin: every pushed row participates.
        if row.owner.is_none() {
            roots = roots.saturating_add(1);
        }
        // Documentation is captured for every pushed declaration (roots,
        // types, members, and executables alike) even when javadoc is
        // absent, so the authority count is the whole attributed package.
        documented = documented.saturating_add(1);
        if row.origin == Origin::Explicit {
            // The lane attaches the declaration's own UTF-16 extent,
            // projected onto the byte domain, as the entity's span.
            let name_span = image.declaration_extent(index).ok().and_then(|extent| {
                let (start16, end16) = (extent.start?, extent.end?);
                Some((
                    java_utf16_byte_offset(source, start16)?,
                    java_utf16_byte_offset(source, end16)?,
                ))
            });
            if primary.is_none() {
                primary = Some(AuthorityPrimary {
                    kind: java_kind(row.kind),
                    name: row.name.bytes.to_vec().into_boxed_slice(),
                    name_span,
                });
            }
            declarations.push(AuthorityDeclaration {
                kind: java_kind(row.kind),
                name: row.name.bytes.to_vec().into_boxed_slice(),
                name_span,
            });
        }
    }
    expectation.declarations = PlaneExpectation::Grounded(declarations);
    expectation.documentation = PlaneExpectation::Grounded(documented);
    expectation.discovery = PlaneExpectation::Grounded(roots);
    if let Some(primary) = primary {
        expectation.primary = PlaneExpectation::Grounded(primary);
    }
    expectation
}

fn java_kind(kind: backend_frontend_java::legacy::DeclarationKind) -> ItemKind {
    use backend_frontend_java::legacy::DeclarationKind as Kind;
    match kind {
        Kind::Module => ItemKind::Module,
        Kind::Package => ItemKind::Namespace,
        Kind::Class | Kind::Record => ItemKind::Record,
        Kind::Interface | Kind::Annotation => ItemKind::Trait,
        Kind::Enum => ItemKind::Enum,
        Kind::Field => ItemKind::Field,
        Kind::EnumConstant => ItemKind::Variant,
        Kind::Constructor | Kind::Method => ItemKind::Function,
    }
}

/// Projects one javac UTF-16 coordinate onto the source's byte domain —
/// the same projection the lane applies to the image's UTF-16 extents.
/// An offset that lands inside a surrogate pair stays unmatched.
fn java_utf16_byte_offset(source: &[u8], units: u32) -> Option<u32> {
    let text = core::str::from_utf8(source).ok()?;
    let mut seen = 0_u32;
    for (offset, character) in text.char_indices() {
        if seen == units {
            return u32::try_from(offset).ok();
        }
        seen = seen.checked_add(u32::try_from(character.len_utf16()).ok()?)?;
    }
    if seen == units {
        return u32::try_from(text.len()).ok();
    }
    None
}

/// Reads the Roslyn image and grounds every plane the image spans can reach.
/// The C# lane attaches the declaration-extent span to every retained row,
/// so the declaration join is a span join; every retained row also carries
/// captured documentation and a C# extension row.
pub(super) fn csharp_authority(image_bytes: &[u8]) -> AuthorityExpectation {
    use backend_frontend_csharp::legacy::{CSharpImage, PartialRole};
    let mut expectation = AuthorityExpectation::default();
    let Ok(image) = CSharpImage::open(image_bytes) else {
        return expectation;
    };
    let mut declarations = Vec::new();
    let mut retained = 0_u32;
    let mut roots = 0_u32;
    let mut primary = None;
    for row in image.declarations() {
        let Ok(row) = row else { continue };
        // A partial pair commits one type fact owned by its Definition part;
        // Implementation parts are never pushed and never predicted.
        if row.partial == PartialRole::Implementation {
            continue;
        }
        let Some(kind) = csharp_kind(row.kind, row.flags.is_const) else {
            continue;
        };
        retained = retained.saturating_add(1);
        if row.owner.is_none() {
            roots = roots.saturating_add(1);
        }
        let declaration = AuthorityDeclaration {
            kind,
            name: row.name.bytes.to_vec().into_boxed_slice(),
            name_span: Some((row.name_start, row.name_end)),
        };
        if primary.is_none() {
            primary = Some(AuthorityPrimary {
                kind: declaration.kind,
                name: declaration.name.clone(),
                name_span: declaration.name_span,
            });
        }
        declarations.push(declaration);
    }
    expectation.declarations = PlaneExpectation::Grounded(declarations);
    expectation.documentation = PlaneExpectation::Grounded(retained);
    // The extension population also includes signature-parameter carriers
    // the lane mints per executable projection, so an exact count is not
    // predictable from the declaration rows alone.
    expectation.extensions = PlaneExpectation::NotCompared;
    expectation.discovery = PlaneExpectation::Grounded(roots);
    if let Some(primary) = primary {
        expectation.primary = PlaneExpectation::Grounded(primary);
    }
    expectation
}

fn csharp_kind(
    kind: backend_frontend_csharp::legacy::DeclarationKind,
    is_const: bool,
) -> Option<ItemKind> {
    use backend_frontend_csharp::legacy::DeclarationKind as Kind;
    Some(match kind {
        Kind::Class | Kind::Struct | Kind::Record | Kind::RecordStruct => ItemKind::Record,
        Kind::Interface => ItemKind::Trait,
        Kind::Enum => ItemKind::Enum,
        Kind::Namespace => ItemKind::Namespace,
        Kind::Delegate
        | Kind::Constructor
        | Kind::StaticConstructor
        | Kind::Method
        | Kind::Operator
        | Kind::Conversion => ItemKind::Function,
        Kind::Field if is_const => ItemKind::Constant,
        Kind::Field => ItemKind::Field,
        Kind::EnumMember => ItemKind::Variant,
        Kind::Property | Kind::Indexer | Kind::Event => ItemKind::Field,
        _ => return None,
    })
}

/// How the lane's fact walk projects one OXC symbol: a declaration entity, a
/// signature parameter entity, a type-graph-embodied row, or nothing at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TsLaneProjection {
    /// The lane pushes one declaration fact for this symbol.
    Entity,
    /// The lane pushes one parameter-kind fact for this symbol: a formal
    /// parameter of a callable whose signature the lane pushes (a named
    /// function, a method signature or definition, or a call or construct
    /// signature).
    Parameter,
    /// The lane covers the declaration row, but the row's uses are embodied
    /// inside a type graph (a mapped record's key text, a variadic tail) and
    /// never commit occurrences: a mapped-type key or a rest parameter.
    Embodied,
    /// The lane never mints a fact for this symbol: a parameter of an arrow
    /// function, anonymous function, or function type; a destructured
    /// binding element; or a destructured catch pattern.
    Unpublished,
}

/// Classifies how the lane projects one symbol from its OXC declaration
/// node. This is the mirror of the lane's documented fact walk: parameters
/// become [`TsLaneProjection::Parameter`] exactly when the enclosing callable
/// is one whose signature the lane pushes (`Function` with a name — covering
/// declarations, `declare function`, and named function expressions — plus
/// method signatures, method definitions, and call/construct signatures;
/// constructors qualify through the `MethodDefinition` above their anonymous
/// `Function` value). Arrow, anonymous, and function-type parameters and
/// destructured binding elements stay [`TsLaneProjection::Unpublished`].
/// A binding-identifier catch parameter is published as a storage binding
/// (`Entity` / `Static`); a destructured catch pattern stays unpublished,
/// same as a destructured declarator element.
fn ts_lane_projection(
    semantic: &backend_frontend_typescript::legacy::Semantic,
    symbol: backend_frontend_typescript::legacy::SymbolId,
) -> TsLaneProjection {
    use backend_frontend_typescript::legacy::AstKind;
    let nodes = semantic.nodes();
    let declared = nodes.get_node(semantic.scoping().symbol_declaration(symbol));
    // A mapped-type key (`{ [P in K]: V }`) is declared by the mapped type
    // itself in oxc (the symbol's declaring node carries the key's span).
    // The lane's declaration facts still cover these rows — the key name is
    // the mapped record's own text — but the lane embodies uses of the key
    // inside its type graph and never commits occurrences naming it, so the
    // key is flagged for the relations plane's target filter only.
    if matches!(declared.kind(), AstKind::TSMappedType(_)) {
        return TsLaneProjection::Embodied;
    }
    match declared.kind() {
        AstKind::FormalParameter(_) | AstKind::FormalParameterRest(_) => {
            let parameters = nodes.parent_id(declared.id());
            let callable = nodes.get_node(nodes.parent_id(parameters));
            let pushed = match callable.kind() {
                AstKind::Function(function) => {
                    function.id.is_some()
                        || matches!(
                            nodes.get_node(nodes.parent_id(callable.id())).kind(),
                            AstKind::MethodDefinition(_)
                        )
                }
                AstKind::TSMethodSignature(_)
                | AstKind::MethodDefinition(_)
                | AstKind::TSCallSignatureDeclaration(_)
                | AstKind::TSConstructSignatureDeclaration(_) => true,
                _ => false,
            };
            if !pushed {
                return TsLaneProjection::Unpublished;
            }
            match declared.kind() {
                // A destructured parameter publishes one fact named by the
                // whole binding pattern, never per element.
                AstKind::FormalParameter(parameter)
                    if !parameter.pattern.is_binding_identifier() =>
                {
                    TsLaneProjection::Unpublished
                }
                // A variadic tail (`...args`) is published as one fact named
                // by the dotted pattern span, and its uses are embodied in
                // the owning signature's type graph.
                AstKind::FormalParameterRest(_) => TsLaneProjection::Embodied,
                _ => TsLaneProjection::Parameter,
            }
        }
        AstKind::VariableDeclarator(declarator) => {
            // A destructured declarator (`const { a, b } = …`) publishes one
            // fact named by the whole binding pattern, never per element, so
            // the element symbols are unpublished.
            if declarator.id.is_binding_identifier() {
                TsLaneProjection::Entity
            } else {
                TsLaneProjection::Unpublished
            }
        }
        AstKind::CatchParameter(parameter) => {
            if parameter.pattern.is_binding_identifier() {
                TsLaneProjection::Entity
            } else {
                TsLaneProjection::Unpublished
            }
        }
        _ => TsLaneProjection::Entity,
    }
}

/// Returns the written annotation span when one symbol declares through a
/// block declarator (`const`/`let`/`var`), `Some(None)` for an unannotated
/// one, and `None` when the symbol declares any other way. The lane reuses
/// the first same-name declarator per lexical owner when the derived type
/// records are identical — which the mirror approximates by the exact
/// annotation spelling — so later twins never reach the published set.
fn declarator_annotation(
    semantic: &backend_frontend_typescript::legacy::Semantic,
    symbol: backend_frontend_typescript::legacy::SymbolId,
) -> Option<Option<(u32, u32)>> {
    use backend_frontend_typescript::legacy::{AstKind, GetSpan};
    let declared = semantic
        .nodes()
        .get_node(semantic.scoping().symbol_declaration(symbol));
    match declared.kind() {
        AstKind::VariableDeclarator(declarator) => Some(match declarator.type_annotation.as_ref()
        {
            Some(annotation) => {
                let span = annotation.type_annotation.span();
                Some((span.start, span.end))
            }
            None => None,
        }),
        _ => None,
    }
}

/// Binds the TSZ checker report and the OXC module authority for the exact
/// compiled source and grounds the declarations and relations planes.  The
/// authority is the TSZ checker itself.
///
/// MIRRORED LANE LAW.  The lane's fact walk pushes declaration facts for the
/// OXC symbol table's module scope (root scopes plus the direct parameter and
/// type-parameter scopes of top-level callables) and parameter facts —
/// [`ItemKind::Parameter`] — for exactly the callable classes
/// [`ts_lane_projection`] enumerates; it never mints facts for arrow,
/// anonymous, or function-type parameters, or destructured binding elements.
/// Binding-identifier catch parameters are published as storage bindings
/// (`Entity` / `Static`); destructured catch patterns stay unpublished.  The
/// declarations plane predicts exactly that set, with
/// the parameter rows carrying the lane's parameter kind.  For relations, the
/// lane commits an occurrence only when a pushed fact owns the use site *and*
/// the referenced symbol is itself a published fact, so the mirror predicts a
/// relation only for lane-published targets; an unowned site is never
/// committed by the lane, so its row stays lawful only when some owned
/// reference elsewhere witnesses the same target.
pub(super) fn typescript_authority(
    report: &backend_frontend_typescript::legacy::Report,
    profile: TypeScriptSource,
    source: &[u8],
) -> AuthorityExpectation {
    use backend_frontend_typescript::legacy::CheckerIndex;
    let mut expectation = AuthorityExpectation::default();
    let Ok(text) = core::str::from_utf8(source) else {
        return expectation;
    };
    let Ok(index) = CheckerIndex::bind(report, text) else {
        return expectation;
    };
    // The checker owns the lane's type facts: a declaration row the checker
    // typed (keyed by its exact name span) is a groundable canonical-type
    // seed. Every checker-known declaration span is also a candidate
    // published target, because the lane's fact walk registers every
    // declaration the checker sees — nested members and callables included —
    // except the parameter rows of callables it never pushes, which the OXC
    // classification below filters back out.
    let mut checker_typed: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let mut checker_declarations: std::collections::HashSet<(u32, u32)> =
        std::collections::HashSet::new();
    for row in index.declarations() {
        checker_declarations.insert((row.name.start, row.name.end));
        if row.r#type.is_some() {
            checker_typed.insert((row.name.start, row.name.end));
        }
    }
    let outcome = backend_frontend_typescript::legacy::with_analysis_declaration(
        profile,
        text,
        report.declaration_file,
        |module| {
            let scoping = module.semantic.scoping();
            let root = scoping.root_scope_id();
            let nodes = module.semantic.nodes();
            use backend_frontend_typescript::legacy::{AstKind, GetSpan, SymbolFlags};
            // One classification sweep over every symbol, in source order:
            // the lane projection, the declared span, and whether the symbol
            // sits at the module scope the declarations plane predicts.
            let mut classified: Vec<(
                backend_frontend_typescript::legacy::SymbolId,
                (u32, u32),
                (u32, u32),
                (u32, u32),
                TsLaneProjection,
                bool,
                bool,
            )> = Vec::new();
            for symbol in scoping.symbol_ids() {
                let span = scoping.symbol_span(symbol);
                let declared = nodes.get_node(scoping.symbol_declaration(symbol));
                if std::env::var("NUDOX_SCRATCH_TS").is_ok() {
                    let name_bytes = scoping.symbol_span(symbol);
                    let name = text
                        .get(name_bytes.start as usize..name_bytes.end as usize)
                        .unwrap_or_default();
                    if matches!(name, "Key" | "ThisTag" | "KeyType" | "P" | "K" | "K2" | "k" | "regex" | "ctx")
                        && span.start < 90000
                    {
                        let parent = nodes.get_node(nodes.parent_id(declared.id())).kind();
                        let prefix = |k: &_| {
                            let d = format!("{k:?}");
                            d.chars().take_while(|ch| *ch != '(').collect::<String>()
                        };
                        eprintln!(
                            "SCRATCH-NODE name={name} span=({},{}) declared={} parent={} gp={}",
                            span.start,
                            span.end,
                            prefix(&declared.kind()),
                            prefix(&parent),
                            prefix(&nodes.get_node(nodes.parent_id(nodes.parent_id(declared.id()))).kind()),
                        );
                    }
                }
                let projection = ts_lane_projection(&module.semantic, symbol);
                let declared_span = (declared.kind().span().start, declared.kind().span().end);
                // The lane names a variadic tail by its dotted pattern span
                // (`rest.rest.span()`, annotation excluded), not the bare
                // identifier span oxc hands the symbol table.
                let row_span = match declared.kind() {
                    AstKind::FormalParameterRest(rest) => {
                        (rest.rest.span.start, rest.rest.span.end)
                    }
                    _ => (span.start, span.end),
                };
                let scope = scoping.symbol_scope_id(symbol);
                // Only module-scope symbols are predicted: deeper scopes are
                // the lane's signature carriers and locals, which the
                // authority never predicts.
                let admitted = scope == root || scoping.scope_parent_id(scope) == Some(root);
                let type_parameter =
                    scoping.symbol_flags(symbol).contains(SymbolFlags::TypeParameter);
                classified.push((
                    symbol,
                    (span.start, span.end),
                    declared_span,
                    row_span,
                    projection,
                    admitted,
                    type_parameter,
                ));
            }
            // The lane's merged twins: a bare same-name fact inside one
            // lexical owner is reused, never minted twice — lone type
            // parameters (bare `TypeVar` rows) and unannotated block
            // declarators alike — so every later same-name twin under the
            // same innermost declared owner is merged away into the first
            // and stays unpublished. Every registered fact is a potential
            // lexical owner, not only the module-scope predictions.
            let owner_candidates: Vec<(u32, u32)> = classified
                .iter()
                .filter(|(.., projection, _, _)| {
                    *projection != TsLaneProjection::Unpublished
                })
                .map(|(_, _, declared_span, ..)| *declared_span)
                .collect();
            let innermost_owner = |span: &(u32, u32), own: &(u32, u32)| -> (u32, u32) {
                owner_candidates
                    .iter()
                    .filter(|candidate| {
                        **candidate != *own && candidate.0 <= span.0 && span.1 <= candidate.1
                    })
                    .max_by_key(|(start, _)| *start)
                    .copied()
                    .unwrap_or((0, 0))
            };
            let mut merged_twins: std::collections::HashSet<(u32, u32)> =
                std::collections::HashSet::new();
            let mut seen_owners: std::collections::HashSet<(u32, u32, Box<[u8]>, Box<[u8]>)> =
                std::collections::HashSet::new();
            for (symbol, span, declared_span, _, projection, _, type_parameter) in &classified {
                // The twin class: a lone type parameter is a bare `TypeVar`
                // row named by its identifier; a declarator twin must carry
                // an identical annotation class. Distinct kinds never merge.
                let class: Vec<u8> = if *type_parameter {
                    b"type-var".to_vec()
                } else if matches!(projection, TsLaneProjection::Entity) {
                    match declarator_annotation(&module.semantic, *symbol) {
                        Some(annotation) => {
                            let mut class = b"declarator".to_vec();
                            if let Some((start, end)) = annotation {
                                if let Some(spelling) = slice_span(text, (start, end)) {
                                    class.extend_from_slice(&spelling);
                                }
                            }
                            class
                        }
                        None => continue,
                    }
                } else {
                    continue;
                };
                let Some(name) = slice_span(text, *span) else {
                    continue;
                };
                let owner = innermost_owner(span, declared_span);
                let key = (
                    owner.0,
                    owner.1,
                    name.clone().into_boxed_slice(),
                    class.into_boxed_slice(),
                );
                if !seen_owners.insert(key) {
                    merged_twins.insert(*span);
                }
            }
            // Declaring spans of the module-scope facts, the owners every
            // reference occurrence is measured against: the lane registers a
            // provenance span for each pushed fact and commits each
            // reference as an owner-relative occurrence over it.
            let owner_spans: Vec<(u32, u32)> = classified
                .iter()
                .filter(|(_, span, .., projection, admitted, _)| {
                    *admitted
                        && *projection != TsLaneProjection::Unpublished
                        && !merged_twins.contains(span)
                })
                .map(|(_, _, declared_span, _, ..)| *declared_span)
                .collect();
            // The lane-published name spans over every scope, and the
            // per-span classification the checker rows filter through.
            let mut lane_published: std::collections::HashSet<(u32, u32)> =
                std::collections::HashSet::new();
            let mut projection_by_span: std::collections::HashMap<(u32, u32), TsLaneProjection> =
                std::collections::HashMap::new();
            let mut declarations = Vec::new();
            let mut primary = None;
            for (symbol, span, _, row_span, projection, admitted, _) in &classified {
                let projection = if merged_twins.contains(span) {
                    TsLaneProjection::Unpublished
                } else {
                    *projection
                };
                projection_by_span.insert(*span, projection);
                if projection != TsLaneProjection::Unpublished {
                    lane_published.insert(*span);
                }
                if !admitted || projection == TsLaneProjection::Unpublished {
                    continue;
                }
                let Some(name) = slice_span(text, *row_span) else {
                    continue;
                };
                // The lane registers every signature parameter as a
                // parameter-kind fact; everything else keeps the
                // flag-derived kind.
                let kind = match projection {
                    TsLaneProjection::Parameter | TsLaneProjection::Embodied => ItemKind::Parameter,
                    _ => {
                        let Some(kind) = ts_kind(scoping.symbol_flags(*symbol)) else {
                            continue;
                        };
                        kind
                    }
                };
                // The primary seed stays a name-only join (it must be a
                // declaration the checker actually typed), while the
                // declaration rows themselves join by their exact name span.
                if primary.is_none() && checker_typed.contains(span) {
                    primary = Some(AuthorityPrimary {
                        kind,
                        name: name.clone().into_boxed_slice(),
                        name_span: None,
                    });
                }
                declarations.push(AuthorityDeclaration {
                    kind,
                    name: name.into_boxed_slice(),
                    name_span: Some(*row_span),
                });
            }
            // The OXC-bound reference spans: the lane's own occurrence pass
            // walks exactly these. A checker-reported site OXC never bound
            // (property accesses and other member reads) reaches the lane's
            // checker-only pass, whose per-site commitment depends on lane
            // embodiment facts the mirror cannot soundly re-derive — so the
            // mirror predicts only OXC-bound sites.
            let mut oxc_resolved_spans: std::collections::HashSet<(u32, u32)> =
                std::collections::HashSet::new();
            for symbol in scoping.symbol_ids() {
                for reference_id in scoping.get_resolved_reference_ids(symbol) {
                    let reference = scoping.get_reference(*reference_id);
                    let span = nodes.get_node(reference.node_id()).kind().span();
                    oxc_resolved_spans.insert((span.start, span.end));
                }
            }
            (declarations, primary, owner_spans, lane_published, projection_by_span, oxc_resolved_spans)
        },
    );
    let Ok((
        declarations,
        primary,
        owner_spans,
        lane_published,
        projection_by_span,
        oxc_resolved_spans,
    )) = outcome
    else {
        return expectation;
    };
    // The lane commits each reference occurrence owned by the pushed fact
    // whose declaring span contains the use, and lifts the owner-relative
    // span over that fact's provenance span into an exact absolute site —
    // the checker-reported use span. A relation is therefore located whenever
    // the use site is owned by a pushed (module-scope) fact. The rare
    // unowned site — a use outside every pushed declaring span, the
    // single-file analogue of a sibling-file row — is never committed by the
    // lane, and only those rows join by target name alone: the mirror keeps
    // an unowned row only when some owned reference elsewhere witnesses the
    // same target, so the mirror never predicts a relation the lane cannot
    // witness.
    //
    // The published-target set is the lane's own fact set: the OXC-classified
    // published symbols plus the checker rows the OXC symbol table does not
    // classify as unpublished (the lane pushes every non-parameter checker
    // row — nested members, callables, and overload entries included). The
    // one exception is the embodied rows — mapped-type keys and variadic
    // tails: the lane covers those rows in its declaration facts but
    // embodies their uses inside a type graph, so no occurrence ever names
    // them.
    let mut published = lane_published;
    for span in checker_declarations {
        if projection_by_span.get(&span) != Some(&TsLaneProjection::Unpublished) {
            published.insert(span);
        }
    }
    for (span, projection) in &projection_by_span {
        if *projection == TsLaneProjection::Embodied {
            published.remove(span);
        }
    }
    let owned_targets: std::collections::HashSet<(u32, u32)> = index
        .references()
        .filter_map(|reference| {
            let target = reference.target?;
            let owned = owner_spans
                .iter()
                .any(|(start, end)| *start <= reference.span.start && reference.span.end <= *end);
            owned.then_some((target.start, target.end))
        })
        .collect();
    let relations = index
        .references()
        .filter_map(|reference| {
            let target = reference.target?;
            if !published.contains(&(target.start, target.end)) {
                return None;
            }
            if !oxc_resolved_spans.contains(&(reference.span.start, reference.span.end)) {
                return None;
            }
            let owned = owner_spans
                .iter()
                .any(|(start, end)| *start <= reference.span.start && reference.span.end <= *end);
            if !owned && !owned_targets.contains(&(target.start, target.end)) {
                return None;
            }
            Some(AuthorityRelation {
                target: slice_span(text, (target.start, target.end))?.into_boxed_slice(),
                site: (reference.span.start, reference.span.end),
                located: owned,
            })
        })
        .collect();
    expectation.declarations = PlaneExpectation::Grounded(declarations);
    expectation.relations = PlaneExpectation::Grounded(relations);
    if let Some(primary) = primary {
        expectation.primary = PlaneExpectation::Grounded(primary);
    }
    expectation
}

fn ts_kind(kind: backend_frontend_typescript::legacy::SymbolFlags) -> Option<ItemKind> {
    use backend_frontend_typescript::legacy::{OxcDeclarationKind, SymbolFlags};
    let kind = if kind.contains(SymbolFlags::ConstVariable) {
        OxcDeclarationKind::Constant
    } else if kind.contains(SymbolFlags::Function) {
        OxcDeclarationKind::Function
    } else if kind.contains(SymbolFlags::Class) {
        OxcDeclarationKind::Class
    } else if kind.contains(SymbolFlags::TypeAlias) {
        OxcDeclarationKind::TypeAlias
    } else if kind.contains(SymbolFlags::Interface) {
        OxcDeclarationKind::Interface
    } else if kind.intersects(SymbolFlags::Enum) {
        OxcDeclarationKind::Enum
    } else if kind.contains(SymbolFlags::EnumMember) {
        OxcDeclarationKind::EnumMember
    } else if kind.intersects(SymbolFlags::Namespace) {
        OxcDeclarationKind::Namespace
    } else if kind.contains(SymbolFlags::TypeParameter) {
        OxcDeclarationKind::TypeParameter
    } else if kind.intersects(SymbolFlags::Import | SymbolFlags::TypeImport) {
        OxcDeclarationKind::Import
    } else if kind.intersects(SymbolFlags::Variable) {
        OxcDeclarationKind::Variable
    } else {
        return None;
    };
    Some(match kind {
        OxcDeclarationKind::Constant => ItemKind::Constant,
        OxcDeclarationKind::Variable => ItemKind::Static,
        OxcDeclarationKind::Function => ItemKind::Function,
        OxcDeclarationKind::Class => ItemKind::Record,
        OxcDeclarationKind::TypeAlias => ItemKind::Alias,
        OxcDeclarationKind::Interface => ItemKind::Trait,
        OxcDeclarationKind::Enum => ItemKind::Enum,
        OxcDeclarationKind::EnumMember => ItemKind::Variant,
        OxcDeclarationKind::Namespace => ItemKind::Module,
        OxcDeclarationKind::TypeParameter => ItemKind::Parameter,
        OxcDeclarationKind::Import => ItemKind::Reexport,
    })
}

/// Reads the Ruff module facts and grounds declarations (span join over the
/// lane's live rows), documentation, extensions, discovery, and relations.
/// The mirror below reproduces the lane's frozen shadowing collapse
/// (`compute_live_set`): one group per lexical (owner, kind, name), subgroup
/// fingerprints by span-erased signature, the later twin wins, and a dead
/// owner takes its subtree with it. Predicting the live set keeps the
/// authority projection sound instead of predicting rows the lane
/// legitimately drops.
pub(super) fn python_authority(
    facts: &backend_frontend_python::legacy::ModuleFacts,
) -> AuthorityExpectation {
    use backend_frontend_python::legacy::DeclarationKind;
    let mut expectation = AuthorityExpectation::default();
    let live = python_live_set(facts);
    // The module row is never a lane fact, so it is never predicted.
    let live_rows: Vec<usize> = (0..facts.declarations.len())
        .filter(|index| live[*index] && facts.declarations[*index].kind != DeclarationKind::Module)
        .collect();
    let declarations: Vec<AuthorityDeclaration> = live_rows
        .iter()
        .map(|index| {
            let row = &facts.declarations[*index];
            AuthorityDeclaration {
                kind: python_kind(row.kind),
                name: row.name.as_bytes().to_vec().into_boxed_slice(),
                name_span: Some((row.name_span.start, row.name_span.end)),
            }
        })
        .collect();
    // Roots: no live class or function strictly contains the row's span —
    // the lane's parentage pass searches live candidates only.
    let roots = live_rows
        .iter()
        .filter(|index| python_innermost_owner(facts, &live, **index).is_none())
        .count();
    // Documentation is captured for every pushed declaration row.
    let documented = u32::try_from(live_rows.len()).unwrap_or(u32::MAX);
    // Relations: an occurrence projects exactly when a pushed live row owns
    // it by name and span containment; predicting those keeps every
    // predicted site verifiable by its absolute span and target name.
    let relations = facts
        .occurrences
        .iter()
        .filter(|occurrence| {
            python_owner_row(facts, &live, occurrence.owner.as_bytes(), occurrence.span).is_some()
        })
        .map(|occurrence| AuthorityRelation {
            target: occurrence.target.as_bytes().to_vec().into_boxed_slice(),
            site: (occurrence.span.start, occurrence.span.end),
            located: true,
        })
        .collect();
    expectation.declarations = PlaneExpectation::Grounded(declarations);
    expectation.documentation = PlaneExpectation::Grounded(documented);
    expectation.discovery = PlaneExpectation::Grounded(u32::try_from(roots).unwrap_or(u32::MAX));
    expectation.relations = PlaneExpectation::Grounded(relations);
    if let Some(primary) = live_rows
        .iter()
        .find(|index| python_innermost_owner(facts, &live, **index).is_none())
        .map(|index| {
            let row = &facts.declarations[*index];
            AuthorityPrimary {
                kind: python_kind(row.kind),
                name: row.name.as_bytes().to_vec().into_boxed_slice(),
                name_span: Some((row.name_span.start, row.name_span.end)),
            }
        })
    {
        expectation.primary = PlaneExpectation::Grounded(primary);
    }
    expectation
}

fn python_innermost_owner(
    facts: &backend_frontend_python::legacy::ModuleFacts,
    live: &[bool],
    index: usize,
) -> Option<usize> {
    use backend_frontend_python::legacy::DeclarationKind;
    let span = facts.declarations[index].span;
    let mut owner: Option<usize> = None;
    let mut owner_area = u64::MAX;
    for (candidate, declaration) in facts.declarations.iter().enumerate() {
        if candidate == index || !live[candidate] {
            continue;
        }
        if !matches!(
            declaration.kind,
            DeclarationKind::Class | DeclarationKind::Function
        ) {
            continue;
        }
        if !python_span_contains(declaration.span, span) {
            continue;
        }
        if declaration.span == span {
            continue;
        }
        let area = u64::from(declaration.span.end.saturating_sub(declaration.span.start));
        if area < owner_area {
            owner_area = area;
            owner = Some(candidate);
        }
    }
    owner
}

const fn python_span_contains(
    outer: backend_frontend_python::legacy::Span,
    inner: backend_frontend_python::legacy::Span,
) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The innermost enclosing class or function of one declaration regardless of
/// liveness — the shadowing groups' scope key, mirroring the lane's
/// `innermost_owner`.
fn python_owner_any(
    facts: &backend_frontend_python::legacy::ModuleFacts,
    index: usize,
) -> Option<usize> {
    use backend_frontend_python::legacy::DeclarationKind;
    let span = facts.declarations[index].span;
    let mut owner: Option<usize> = None;
    let mut owner_area = u64::MAX;
    for (candidate, declaration) in facts.declarations.iter().enumerate() {
        if candidate == index {
            continue;
        }
        if !matches!(
            declaration.kind,
            DeclarationKind::Class | DeclarationKind::Function
        ) {
            continue;
        }
        if !python_span_contains(declaration.span, span) {
            continue;
        }
        if declaration.span == span {
            continue;
        }
        let area = u64::from(declaration.span.end.saturating_sub(declaration.span.start));
        if area < owner_area {
            owner_area = area;
            owner = Some(candidate);
        }
    }
    owner
}

/// Faithful mirror of the lane's frozen `compute_live_set`: one group per
/// lexical (owner, kind, name); within a group, subgroup by span-erased
/// signature fingerprint and keep only the later twin; then a dead owner
/// takes its whole subtree with it.
fn python_live_set(facts: &backend_frontend_python::legacy::ModuleFacts) -> Vec<bool> {
    use backend_frontend_python::legacy::DeclarationKind;
    use std::collections::HashMap;
    let count = facts.declarations.len();
    let owners: Vec<Option<usize>> = (0..count)
        .map(|index| python_owner_any(facts, index))
        .collect();
    let mut live = vec![true; count];
    let mut groups: HashMap<(Option<usize>, u8, &str), Vec<usize>> = HashMap::new();
    for (index, declaration) in facts.declarations.iter().enumerate() {
        if declaration.kind == DeclarationKind::Module {
            continue;
        }
        let discriminant = match declaration.kind {
            DeclarationKind::Module => 0_u8,
            DeclarationKind::Class => 1,
            DeclarationKind::Function => 2,
            DeclarationKind::Field => 3,
            DeclarationKind::Constant => 4,
            DeclarationKind::Alias => 5,
        };
        groups
            .entry((owners[index], discriminant, declaration.name.as_str()))
            .or_default()
            .push(index);
    }
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let mut fingerprints: HashMap<String, Vec<usize>> = HashMap::new();
        for index in indices {
            fingerprints
                .entry(python_shadowing_fingerprint(facts, *index))
                .or_default()
                .push(*index);
        }
        for twins in fingerprints.values() {
            if twins.len() < 2 {
                continue;
            }
            let mut ordered = twins.clone();
            ordered.sort_by_key(|index| (facts.declarations[*index].span.start, *index));
            for shadowed in &ordered[..ordered.len() - 1] {
                live[*shadowed] = false;
            }
        }
    }
    let mut by_area: Vec<usize> = (0..count).collect();
    by_area.sort_by_key(|index| {
        let span = facts.declarations[*index].span;
        u64::from(span.end.saturating_sub(span.start))
    });
    for index in by_area.into_iter().rev() {
        if live[index]
            && let Some(owner) = owners[index]
            && !live[owner]
        {
            live[index] = false;
        }
    }
    live
}

fn python_shadowing_fingerprint(
    facts: &backend_frontend_python::legacy::ModuleFacts,
    index: usize,
) -> String {
    use backend_frontend_python::legacy::{AnnotationPosition, ClassForm, DeclarationKind};
    let declaration = &facts.declarations[index];
    match declaration.kind {
        DeclarationKind::Function => {
            let mut parts = Vec::with_capacity(declaration.parameters.len());
            for parameter in &declaration.parameters {
                parts.push(format!(
                    "{:?}:{:?}",
                    parameter.kind,
                    python_normalized_annotation(&parameter.annotation)
                ));
            }
            let returns = python_annotation_for(facts, declaration, AnnotationPosition::Return)
                .map(|found| format!("{:?}", python_normalized_annotation(&found.annotation)))
                .unwrap_or_else(|| "none".to_owned());
            format!("fn:[{}] ret({})", parts.join(","), returns)
        }
        DeclarationKind::Field | DeclarationKind::Constant => {
            match python_annotation_for(facts, declaration, AnnotationPosition::Field) {
                Some(found) => {
                    format!("var:{:?}", python_normalized_annotation(&found.annotation))
                }
                None => "var:unannotated".to_owned(),
            }
        }
        DeclarationKind::Alias => {
            let is_type_alias =
                declaration.value_span.is_none() && declaration.value_source.is_some();
            if is_type_alias
                && let Some(found) =
                    python_annotation_for(facts, declaration, AnnotationPosition::AliasValue)
            {
                format!(
                    "alias:{:?}",
                    python_normalized_annotation(&found.annotation)
                )
            } else {
                "alias:import".to_owned()
            }
        }
        DeclarationKind::Class => match declaration.class_form {
            Some(ClassForm::TypedDict) | Some(ClassForm::Protocol) => {
                let mut bases = Vec::new();
                for base in &declaration.bases {
                    bases.push(format!("{:?}", python_normalized_annotation(base)));
                }
                let mut members = Vec::new();
                for member_index in python_structural_members(facts, declaration) {
                    let member = &facts.declarations[member_index];
                    let key = match member.kind {
                        DeclarationKind::Function => {
                            python_shadowing_fingerprint(facts, member_index)
                        }
                        DeclarationKind::Field | DeclarationKind::Constant => {
                            match python_annotation_for(facts, member, AnnotationPosition::Field) {
                                Some(found) => {
                                    format!("{:?}", python_normalized_annotation(&found.annotation))
                                }
                                None => "unannotated".to_owned(),
                            }
                        }
                        _ => format!("{:?}:{}", member.kind, member.name),
                    };
                    members.push(format!("{}:{key}", member.name));
                }
                format!(
                    "structural:{:?}|bases:[{}]|total:{:?}|members:[{}]",
                    declaration.class_form,
                    bases.join(","),
                    declaration.total,
                    members.join(",")
                )
            }
            _ => "plain-class".to_owned(),
        },
        DeclarationKind::Module => "module".to_owned(),
    }
}

/// Member keys carry the member name beside its signature: structural
/// members join the key by `name:signature` so distinctly-named rows never
/// share a fingerprint. (The lane embeds the same key shape.)
fn python_structural_members(
    facts: &backend_frontend_python::legacy::ModuleFacts,
    class: &backend_frontend_python::legacy::DeclarationFact,
) -> Vec<usize> {
    use backend_frontend_python::legacy::{ClassForm, DeclarationKind};
    let wanted_kind = match class.class_form {
        Some(ClassForm::TypedDict) => DeclarationKind::Field,
        _ => DeclarationKind::Function,
    };
    facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.kind == wanted_kind
                && python_span_contains(class.span, candidate.span)
                && facts.declarations.iter().all(|other| {
                    other.kind != DeclarationKind::Class
                        || other.span == class.span
                        || !python_span_contains(other.span, candidate.span)
                })
        })
        .map(|(index, _)| index)
        .collect()
}

fn python_annotation_for<'facts>(
    facts: &'facts backend_frontend_python::legacy::ModuleFacts,
    declaration: &backend_frontend_python::legacy::DeclarationFact,
    position: backend_frontend_python::legacy::AnnotationPosition,
) -> Option<&'facts backend_frontend_python::legacy::AnnotationFact> {
    facts.annotations.iter().find(|candidate| {
        candidate.position == position
            && candidate.owner == declaration.name
            && python_span_contains(declaration.span, candidate.span)
    })
}

/// One annotation with every source coordinate erased, mirroring the lane's
/// frozen normalization: `typing.` prefixes strip, literals widen to their
/// base primitives, and unsupported syntax keeps only its syntax kind.
fn python_normalized_annotation(
    annotation: &backend_frontend_python::legacy::Annotation,
) -> backend_frontend_python::legacy::Annotation {
    use backend_frontend_python::legacy::{Annotation, LiteralValue, Span};
    match annotation {
        Annotation::Name { name, .. } => {
            let canonical = name.strip_prefix("typing.").unwrap_or(name);
            Annotation::Name {
                name: canonical.to_owned(),
                span: None,
            }
        }
        Annotation::Generic { base, args } => Annotation::Generic {
            base: Box::new(python_normalized_annotation(base)),
            args: args.iter().map(python_normalized_annotation).collect(),
        },
        Annotation::List(items) => {
            Annotation::List(items.iter().map(python_normalized_annotation).collect())
        }
        Annotation::StringLiteral(value) => Annotation::StringLiteral(value.clone()),
        Annotation::Union(members) => {
            Annotation::Union(members.iter().map(python_normalized_annotation).collect())
        }
        Annotation::Literal(values) => {
            let mut widened: Vec<Annotation> = Vec::new();
            let mut unwidenable = false;
            for value in values {
                let mapped = match value {
                    LiteralValue::String(_) => Annotation::Name {
                        name: "str".to_owned(),
                        span: None,
                    },
                    LiteralValue::Integer(_) => Annotation::Name {
                        name: "int".to_owned(),
                        span: None,
                    },
                    LiteralValue::Float { .. } => Annotation::Name {
                        name: "float".to_owned(),
                        span: None,
                    },
                    LiteralValue::Complex { .. } => Annotation::Name {
                        name: "complex".to_owned(),
                        span: None,
                    },
                    LiteralValue::Boolean(_) => Annotation::Name {
                        name: "bool".to_owned(),
                        span: None,
                    },
                    LiteralValue::None => Annotation::None,
                    LiteralValue::Ellipsis | LiteralValue::Unsupported(_) => {
                        unwidenable = true;
                        break;
                    }
                };
                if !widened.contains(&mapped) {
                    widened.push(mapped);
                }
            }
            if unwidenable {
                Annotation::Literal(values.clone())
            } else {
                match widened.as_slice() {
                    [] => Annotation::Literal(values.clone()),
                    [single] => single.clone(),
                    _ => Annotation::Union(widened),
                }
            }
        }
        Annotation::None => Annotation::None,
        Annotation::Unknown(reason) => Annotation::Unknown(match reason {
            backend_frontend_python::legacy::TypeReason::Unannotated { position } => {
                backend_frontend_python::legacy::TypeReason::Unannotated {
                    position: *position,
                }
            }
            backend_frontend_python::legacy::TypeReason::UnsupportedSyntax { kind, .. } => {
                backend_frontend_python::legacy::TypeReason::UnsupportedSyntax {
                    kind: *kind,
                    span: Span { start: 0, end: 0 },
                }
            }
            backend_frontend_python::legacy::TypeReason::TruncatedAtDepthLimit => {
                backend_frontend_python::legacy::TypeReason::TruncatedAtDepthLimit
            }
        }),
    }
}

/// The lane's `owner_row` rule: the innermost live row whose name matches and
/// whose span contains the occurrence.
fn python_owner_row(
    facts: &backend_frontend_python::legacy::ModuleFacts,
    live: &[bool],
    owner: &[u8],
    span: backend_frontend_python::legacy::Span,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, declaration) in facts.declarations.iter().enumerate() {
        if !live[index] {
            continue;
        }
        // The module row is never pushed, so it can never own an occurrence.
        if declaration.kind == backend_frontend_python::legacy::DeclarationKind::Module {
            continue;
        }
        if declaration.name.as_bytes() != owner {
            continue;
        }
        if !python_span_contains(declaration.span, span) {
            continue;
        }
        if best.is_none_or(|best| declaration.span.start > facts.declarations[best].span.start) {
            best = Some(index);
        }
    }
    best
}

fn python_kind(kind: backend_frontend_python::legacy::DeclarationKind) -> ItemKind {
    use backend_frontend_python::legacy::DeclarationKind as Kind;
    match kind {
        Kind::Module => ItemKind::Module,
        Kind::Class => ItemKind::Record,
        Kind::Function => ItemKind::Function,
        Kind::Field => ItemKind::Field,
        Kind::Constant => ItemKind::Static,
        Kind::Alias => ItemKind::Alias,
    }
}

/// Runs the libclang legacy collector over the project's one entry
/// translation unit — the same analysis the lane's lowerer consumes — and
/// grounds the declarations plane by span join.  The prediction mirrors the
/// lane's own documented admission laws (`engine/src/driver/lower/clang.rs`),
/// not the raw scratch: USR-twin representatives collapse, named pass-one
/// anchors admit, block-scope locals of executables refuse, parameters of
/// unrepresented owners refuse, and every admitted row captures its
/// documentation.
pub(super) fn clang_authority(
    project: &backend_frontend_clang::ClangProject,
    source: &[u8],
    cancelled: &AtomicBool,
) -> AuthorityExpectation {
    use backend_frontend_clang::legacy::{
        ClangInput, ClangScratch, MAX_CLANG_DECLARATIONS, MAX_CLANG_DIAGNOSTICS,
        MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES, MAX_CLANG_REFERENCES, MAX_CLANG_TYPE_EDGES,
        MAX_CLANG_TYPES, collect_cancellable,
    };
    use std::ffi::{CStr, CString};
    let mut expectation = AuthorityExpectation::default();
    let Ok(file_name) = CString::new(project.entry().to_string_lossy().as_bytes()) else {
        return expectation;
    };
    let Ok(directory) = CString::new(project.root().to_string_lossy().as_bytes()) else {
        return expectation;
    };
    let arguments = project.arguments();
    let Ok(owned) = arguments
        .iter()
        .map(|argument| CString::new(argument.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return expectation;
    };
    let borrowed: Vec<&CStr> = owned.iter().map(CString::as_c_str).collect();
    let Ok(input) = ClangInput::from_database(&file_name, source, &borrowed, &directory) else {
        return expectation;
    };
    let mut declarations = vec![empty_declaration(); MAX_CLANG_DECLARATIONS];
    let mut types = vec![empty_type(); MAX_CLANG_TYPES];
    let mut type_edges = vec![empty_type_edge(); MAX_CLANG_TYPE_EDGES];
    let mut references = vec![empty_reference(); MAX_CLANG_REFERENCES];
    let mut diagnostics = vec![empty_diagnostic(); MAX_CLANG_DIAGNOSTICS];
    let mut includes = vec![empty_include(); MAX_CLANG_INCLUDES];
    let mut overrides = vec![empty_override(); MAX_CLANG_OVERRIDES];
    let facts = collect_cancellable(
        input,
        ClangScratch {
            declarations: &mut declarations,
            types: &mut types,
            type_edges: &mut type_edges,
            references: &mut references,
            diagnostics: &mut diagnostics,
            includes: &mut includes,
            overrides: &mut overrides,
        },
        cancelled,
    );
    let Ok(facts) = facts else {
        return expectation;
    };
    let (rows, pushed) = clang_admitted_rows(&facts, source);
    // Documentation is captured for every admitted declaration row (`push_docs`
    // marks exactly the pushed ordinals) plus every distinct include-spelling
    // module row `push_includes` mints under the lane's captured-empty
    // contract.
    let documented =
        u32::try_from(rows.len() + clang_include_module_rows(&facts, source)).unwrap_or(u32::MAX);
    let primary = rows.first().map(|row| AuthorityPrimary {
        kind: row.kind,
        name: row.name.clone(),
        name_span: row.name_span,
    });
    expectation.declarations = PlaneExpectation::Grounded(rows);
    expectation.documentation = PlaneExpectation::Grounded(documented);
    // Relations: the lane's occurrence plane mirrors the authority's
    // reference facts. A reference is witnessed exactly when its owner is a
    // pushed declaration whose extent contains the site, and its target
    // projects a name-joinable row — a local pushed declaration, or an
    // unresolved type/member site whose written spelling becomes the foreign
    // display. USR-keyed stable targets carry no name spelling, so they are
    // never name-witnessed and never predicted.
    {
        use backend_frontend_clang::legacy::{ReferenceKind, ReferenceTarget};
        let mut relations = Vec::new();
        for reference in facts.references {
            let Some(owner) = reference.owner else {
                continue;
            };
            let Some((_, (owner_start, owner_end))) = pushed.get(&owner) else {
                continue;
            };
            let (site_start, site_end) = (reference.span.start, reference.span.end);
            if !(owner_start <= &site_start && &site_end <= owner_end) {
                continue;
            }
            let written = &source[usize::try_from(site_start).unwrap_or(usize::MAX)
                ..usize::try_from(site_end).unwrap_or(usize::MAX)];
            let target = match reference.target {
                ReferenceTarget::Local(identity) => match pushed.get(&identity) {
                    Some((name, _)) => name.clone(),
                    None => continue,
                },
                ReferenceTarget::Unresolved => match reference.kind {
                    ReferenceKind::Type | ReferenceKind::Template | ReferenceKind::Member => {
                        // The lane's `foreign_universe_key` commits an
                        // unresolved site exactly when its written spelling
                        // is a non-empty valid UTF-8 path; a degenerate
                        // (zero-length) or non-UTF-8 site keeps no honest
                        // foreign key and stays absent.
                        match core::str::from_utf8(written) {
                            Ok(spelling) if !spelling.is_empty() => written.to_vec(),
                            _ => continue,
                        }
                    }
                    _ => continue,
                },
                ReferenceTarget::Foreign { .. } => continue,
            };
            relations.push(AuthorityRelation {
                target: target.into_boxed_slice(),
                site: (site_start, site_end),
                located: true,
            });
        }
        expectation.relations = PlaneExpectation::Grounded(relations);
    }
    if let Some(primary) = primary {
        expectation.primary = PlaneExpectation::Grounded(primary);
    }
    expectation
}

/// The lane's documented admission set over one collected scratch image.
///
/// This is a row-for-row mirror of the projection in
/// `engine/src/driver/lower/clang.rs`, derived from its own documented laws:
///
/// - Representatives: declaration cursors sharing one libclang USR collapse
///   to one fact that prefers the definition (`select_representatives`).
/// - Pass one admits exactly the named anchors later type rows can name —
///   records, enumerations, typedefs, templates, and namespaces — so an
///   anonymous record or enumeration (whose keyword spelling is not a name)
///   is never an entity; only its named member fields are.
/// - Pass two admits enumerators, fields, macros, executables, and honest
///   file/namespace/record-scope storage, but refuses block-scope locals of
///   executables (`owner_is_functional`) and every row whose owner identity
///   has no retained declaration row (`owner_is_unrepresented` — lambda
///   closures and their signatures).
/// - The deferred parameter stash emits only each owner's chosen run, and
///   the projection admits only parameters whose owner is represented.
///
/// Predicting the admission set keeps the expectation sound: it never
/// predicts a row the honest lowering refuses, and every row it predicts
/// must join an IR entity.  The returned index maps each admitted row's USR
/// identity to its declared name and extent — the same resolution the
/// lane's `ordinal_of` exposes to its occurrence and topology passes.
#[allow(
    clippy::type_complexity,
    reason = "the parallel identity index mirrors the lane's own resolution tables"
)]
fn clang_admitted_rows(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    source: &[u8],
) -> (
    Vec<AuthorityDeclaration>,
    std::collections::HashMap<
        backend_frontend_clang::legacy::SymbolIdentity,
        (Vec<u8>, (u32, u32)),
    >,
) {
    use backend_frontend_clang::legacy::{DeclarationKind, DefinitionState, TypeKind};
    use std::collections::HashMap;
    let declarations = facts.declarations;
    let mut pushed: HashMap<backend_frontend_clang::legacy::SymbolIdentity, (Vec<u8>, (u32, u32))> =
        HashMap::new();
    // Mirror of `select_representatives`: USR twins collapse, the definition
    // of a twin promotes itself and demotes the earlier declaration twin.
    let mut representative: Vec<Option<usize>> = vec![None; declarations.len()];
    for (index, declaration) in declarations.iter().enumerate() {
        let mut winner = Some(index);
        if let Some(identity) = declaration.identity {
            let earlier = (0..index).find(|earlier| {
                representative.get(*earlier).copied().flatten() == Some(*earlier)
                    && declarations.get(*earlier).and_then(|known| known.identity) == Some(identity)
            });
            if let Some(earlier) = earlier {
                winner = None;
                let becomes_definition = declaration.definition == DefinitionState::Definition
                    && declarations
                        .get(earlier)
                        .is_some_and(|known| known.definition == DefinitionState::Declaration);
                if becomes_definition {
                    if let Some(slot) = representative.get_mut(earlier) {
                        *slot = None;
                    }
                    winner = Some(index);
                }
            }
        }
        if let Some(slot) = representative.get_mut(index) {
            *slot = winner;
        }
    }
    // The lane's own `name_of`: the written name span slices non-empty
    // bytes out of the source. Returns the span and its exact bytes.
    let name_bytes = |declaration: &backend_frontend_clang::legacy::DeclarationFact| {
        let span = declaration.name?;
        let start = usize::try_from(span.start).ok()?;
        let end = usize::try_from(span.end).ok()?;
        let bytes = source.get(start..end)?;
        (!bytes.is_empty()).then(|| (span.start, span.end, bytes.to_vec()))
    };
    let owner_is_functional = |declaration: &backend_frontend_clang::legacy::DeclarationFact| {
        let Some(owner) = declaration.owner else {
            return false;
        };
        let Some(owner_decl) = declarations
            .iter()
            .find(|candidate| candidate.identity == Some(owner))
        else {
            return false;
        };
        match owner_decl.kind {
            DeclarationKind::Function
            | DeclarationKind::Method
            | DeclarationKind::Constructor
            | DeclarationKind::Destructor => true,
            DeclarationKind::Template => owner_decl
                .type_root
                .and_then(|root| facts.types.get(root.raw as usize))
                .is_some_and(|row| row.kind == TypeKind::Function),
            _ => false,
        }
    };
    let owner_is_unrepresented = |declaration: &backend_frontend_clang::legacy::DeclarationFact| {
        declaration.owner.is_some_and(|owner| {
            !declarations
                .iter()
                .any(|candidate| candidate.identity == Some(owner))
        })
    };
    let mut rows = Vec::new();
    for (index, declaration) in declarations.iter().enumerate() {
        if representative.get(index).copied().flatten() != Some(index) {
            continue;
        }
        let admitted = match declaration.kind {
            // Pass-one anchors: named records, enumerations, typedefs,
            // templates, and namespaces are entities; an anonymous record
            // owns no written name and never becomes one.
            DeclarationKind::Record
            | DeclarationKind::Enumeration
            | DeclarationKind::TypeAlias
            | DeclarationKind::Template
            | DeclarationKind::Namespace
            | DeclarationKind::Enumerator
            | DeclarationKind::Field
            | DeclarationKind::Macro
            | DeclarationKind::Function
            | DeclarationKind::Method
            | DeclarationKind::Constructor
            | DeclarationKind::Destructor => name_bytes(declaration).is_some(),
            // File, namespace, and record scope storage keeps its honest
            // parentage; block-scope locals of executables and the storage
            // of unrepresented owners are refused.
            DeclarationKind::Variable => {
                name_bytes(declaration).is_some()
                    && !owner_is_functional(declaration)
                    && !owner_is_unrepresented(declaration)
            }
            // Signature storage of a represented executable is admitted
            // (named carriers, plus the synthetic result carrier the
            // authority never names); a lambda closure's parameters have no
            // honest parent row and stay absent.
            DeclarationKind::Parameter => {
                name_bytes(declaration).is_some() && !owner_is_unrepresented(declaration)
            }
            DeclarationKind::TemplateParameter | DeclarationKind::Unknown => false,
        };
        if !admitted {
            continue;
        }
        let Some(kind) = clang_kind(declaration, facts.types) else {
            continue;
        };
        let Some((start, end, name)) = name_bytes(declaration) else {
            continue;
        };
        // The lane records an identity only when libclang proved one, and
        // identity twins collapse to one representative, so the first
        // admission per identity is the only one.
        if let Some(identity) = declaration.identity {
            pushed
                .entry(identity)
                .or_insert((name.clone(), (declaration.span.start, declaration.span.end)));
        }
        rows.push(AuthorityDeclaration {
            kind,
            name: name.into_boxed_slice(),
            name_span: Some((start, end)),
        });
    }
    (rows, pushed)
}

/// Counts the module rows `push_includes` mints: one per distinct include
/// spelling whose delimited path slices to non-empty valid UTF-8 inside the
/// directive's own span. These rows are not authority declaration rows (the
/// declarations plane never predicts them), but each one captures its
/// documentation, so the documentation count includes them.
fn clang_include_module_rows(
    facts: &backend_frontend_clang::legacy::ClangFacts<'_>,
    source: &[u8],
) -> usize {
    let mut spellings: Vec<Vec<u8>> = Vec::new();
    for include in facts.includes {
        let start = usize::try_from(include.span.start).unwrap_or(usize::MAX);
        let end = usize::try_from(include.span.end).unwrap_or(usize::MAX);
        let Some(bytes) = source.get(start..end) else {
            continue;
        };
        // The delimited path of `#include "…" / #include <…>` inside the
        // directive span, exactly as the lane's `include_spelling_span`.
        let Some(open_at) = bytes.iter().position(|byte| *byte == b'<' || *byte == b'"') else {
            continue;
        };
        let closer = match bytes[open_at] {
            b'<' => b'>',
            _ => b'"',
        };
        let Some(relative) = bytes
            .get(open_at + 1..)
            .and_then(|rest| rest.iter().position(|byte| *byte == closer))
        else {
            continue;
        };
        let Some(spelling) = bytes.get(open_at + 1..open_at + 1 + relative) else {
            continue;
        };
        if spelling.is_empty() || core::str::from_utf8(spelling).is_err() {
            continue;
        }
        if !spellings.iter().any(|known| known == spelling) {
            spellings.push(spelling.to_vec());
        }
    }
    spellings.len()
}

fn clang_kind(
    declaration: &backend_frontend_clang::legacy::DeclarationFact,
    types: &[backend_frontend_clang::legacy::TypeFact],
) -> Option<ItemKind> {
    use backend_frontend_clang::legacy::{DeclarationKind as Kind, TypeKind};
    Some(match declaration.kind {
        Kind::Namespace => ItemKind::Namespace,
        Kind::Macro => ItemKind::Macro,
        Kind::Record => ItemKind::Record,
        Kind::Enumeration => ItemKind::Enum,
        Kind::Enumerator => ItemKind::Variant,
        Kind::Function | Kind::Method | Kind::Constructor | Kind::Destructor => ItemKind::Function,
        Kind::Field => ItemKind::Field,
        Kind::Variable => ItemKind::Static,
        Kind::Parameter => ItemKind::Parameter,
        Kind::TypeAlias => ItemKind::Alias,
        Kind::Template => {
            let function = declaration
                .type_root
                .and_then(|root| usize::try_from(root.raw).ok())
                .and_then(|root| types.get(root))
                .is_some_and(|row| row.kind == TypeKind::Function);
            if function {
                ItemKind::Function
            } else {
                ItemKind::Record
            }
        }
        Kind::Unknown | Kind::TemplateParameter => return None,
    })
}

fn clang_span() -> backend_frontend_clang::legacy::SourceSpan {
    backend_frontend_clang::legacy::SourceSpan { start: 0, end: 0 }
}

fn clang_identity() -> backend_frontend_clang::legacy::SymbolIdentity {
    backend_frontend_clang::legacy::SymbolIdentity {
        bytes: [0; backend_frontend_clang::legacy::SYMBOL_IDENTITY_BYTES],
    }
}

fn empty_declaration() -> backend_frontend_clang::legacy::DeclarationFact {
    backend_frontend_clang::legacy::DeclarationFact {
        id: backend_frontend_clang::legacy::DeclarationId { raw: 0 },
        kind: backend_frontend_clang::legacy::DeclarationKind::Unknown,
        definition: backend_frontend_clang::legacy::DefinitionState::Declaration,
        virtuality: backend_frontend_clang::legacy::MethodVirtuality::NonVirtual,
        identity: None,
        span: clang_span(),
        name: None,
        owner: None,
        documentation: None,
        storage: backend_frontend_clang::legacy::StorageClass::None,
        type_root: None,
        enum_underlying: None,
    }
}

fn empty_type() -> backend_frontend_clang::legacy::TypeFact {
    backend_frontend_clang::legacy::TypeFact {
        id: backend_frontend_clang::legacy::TypeId { raw: 0 },
        kind: backend_frontend_clang::legacy::TypeKind::Unknown,
        qualifiers: backend_frontend_clang::legacy::TypeQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        declaration: None,
        array_len: None,
        builtin: None,
        size_bits: None,
        align_bits: None,
        is_variadic: false,
    }
}

fn empty_type_edge() -> backend_frontend_clang::legacy::TypeEdge {
    backend_frontend_clang::legacy::TypeEdge {
        source: backend_frontend_clang::legacy::TypeId { raw: 0 },
        relation: backend_frontend_clang::legacy::TypeRelation::Pointee,
        target: backend_frontend_clang::legacy::TypeId { raw: 0 },
    }
}

fn empty_reference() -> backend_frontend_clang::legacy::ReferenceFact {
    backend_frontend_clang::legacy::ReferenceFact {
        kind: backend_frontend_clang::legacy::ReferenceKind::Value,
        span: clang_span(),
        owner: None,
        target: backend_frontend_clang::legacy::ReferenceTarget::Unresolved,
    }
}

fn empty_diagnostic() -> backend_frontend_clang::legacy::DiagnosticFact {
    backend_frontend_clang::legacy::DiagnosticFact {
        severity: backend_frontend_clang::legacy::DiagnosticSeverity::Unknown,
        location: None,
        category: 0,
        message: None,
    }
}

fn empty_include() -> backend_frontend_clang::legacy::IncludeFact {
    backend_frontend_clang::legacy::IncludeFact {
        kind: backend_frontend_clang::legacy::SourceDependencyKind::Include,
        span: clang_span(),
        resolved: None,
    }
}

fn empty_override() -> backend_frontend_clang::legacy::OverrideFact {
    backend_frontend_clang::legacy::OverrideFact {
        source: clang_identity(),
        target: clang_identity(),
        target_file: None,
    }
}

#[cfg(test)]
mod authority_plane_tests {
    use super::*;
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DocInput, EntityVersion, IrBuilder,
        LinkKind, OccurrenceAuthorityFacts, SemanticImageView, SourceSpan, TreeEntityId,
        TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
        encode_full_semantic_image, full_semantic_image_len,
    };

    fn version(value: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([value; 16]),
            variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
            core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
        }
    }

    /// One synthetic two-entity image: a documented record `Widget` and an
    /// undocumented function `make`, where `make` calls `Widget` at site
    /// 40..46 and both rows carry spans in `lib.rs`.
    fn synthetic_ir() -> Ir {
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(backend_semantic::ir::LanguageProfile::CSharp(
                backend_semantic::ir::CSharpVersion::CSharp14,
            ))
            .expect("csharp profile admitted");
        let file_atom = builder.intern_atom(b"lib.rs").expect("file atom");
        let ty_string = builder
            .intern_type(TypeExpr::Concrete(ConcreteType::Builtin(
                backend_semantic::ir::BuiltinType::String,
            )))
            .expect("string type");
        let documented = backend_semantic::ir::EntityAuthorityFacts {
            parentage: backend_semantic::ir::ParentageAuthority::Root,
            visibility: FactAvailability::Captured,
            source: FactAvailability::Captured,
            source_file: FactAvailability::Captured,
            semantic_type: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            ..backend_semantic::ir::EntityAuthorityFacts::default()
        };
        let undocumented = backend_semantic::ir::EntityAuthorityFacts {
            parentage: backend_semantic::ir::ParentageAuthority::Root,
            visibility: FactAvailability::Captured,
            source: FactAvailability::Captured,
            source_file: FactAvailability::Captured,
            semantic_type: FactAvailability::Captured,
            ..backend_semantic::ir::EntityAuthorityFacts::default()
        };
        let docs = [DocInput::Text("widget docs")];
        let items = [
            TreeItemInput {
                name: b"Widget",
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: documented,
                parent: None,
                semantic_type: Some(ty_string),
                members: &[],
                docs: &docs,
                attributes: &[],
                source: SourceSpan::new(file_atom, 0, 50),
                extension: None,
            },
            TreeItemInput {
                name: b"make",
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: undocumented,
                parent: None,
                semantic_type: Some(ty_string),
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file_atom, 52, 96),
                extension: None,
            },
        ];
        let versions = [version(1), version(2)];
        let links = [TreeLinkInput {
            from: TreeEntityId::new(1),
            target: TreeLinkTarget::Local(TreeEntityId::new(0)),
            kind: backend_semantic::ir::LinkKind::Calls,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(file_atom, 40, 46),
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("tree admitted");
        builder.finish().expect("synthetic ir builds")
    }

    /// Encodes the synthetic image through the durable wire format so the
    /// second reader of every plane check is a cold decoded image.
    fn reopened_bytes(ir: &Ir) -> Vec<u8> {
        let length = full_semantic_image_len(ir).expect("image length");
        let mut bytes = vec![0_u8; length];
        let written = encode_full_semantic_image(ir, &mut bytes).expect("encode");
        assert_eq!(written, bytes.len());
        bytes
    }

    fn declaration(
        kind: ItemKind,
        name: &[u8],
        name_span: Option<(u32, u32)>,
    ) -> AuthorityDeclaration {
        AuthorityDeclaration {
            kind,
            name: name.to_vec().into_boxed_slice(),
            name_span,
        }
    }

    fn grounded_expectation() -> AuthorityExpectation {
        AuthorityExpectation {
            declarations: PlaneExpectation::Grounded(vec![
                declaration(ItemKind::Record, b"Widget", Some((7, 13))),
                declaration(ItemKind::Function, b"make", Some((60, 64))),
            ]),
            types: PlaneExpectation::Grounded(vec![AuthorityTypeRow {
                kind: ItemKind::Function,
                name: b"make".to_vec().into_boxed_slice(),
                shape: ObservedTypeShape::Primitive(backend_semantic::ir::BuiltinType::String),
            }]),
            relations: PlaneExpectation::Grounded(vec![AuthorityRelation {
                target: b"Widget".to_vec().into_boxed_slice(),
                site: (40, 46),
                located: true,
            }]),
            documentation: PlaneExpectation::Grounded(1),
            // The synthetic image captures no language-extension rows; the
            // parity count still binds the plane to the honest zero.
            extensions: PlaneExpectation::Grounded(0),
            discovery: PlaneExpectation::Grounded(2),
            primary: PlaneExpectation::Grounded(AuthorityPrimary {
                kind: ItemKind::Function,
                name: b"make".to_vec().into_boxed_slice(),
                name_span: Some((60, 64)),
            }),
        }
    }

    fn audit(
        ir: &Ir,
        image: &SemanticImageView<'_>,
        expectation: &AuthorityExpectation,
    ) -> (usize, Vec<CorpusMismatch>, Vec<CorpusMismatch>) {
        let case = inventory::real_package_cases().next().expect("frozen case");
        let mut unavailable = Vec::new();
        let mut mismatches = Vec::new();
        let not_compared = check_authority_planes(
            ir,
            image,
            b"lib.rs",
            case,
            expectation,
            &mut unavailable,
            &mut mismatches,
        );
        (not_compared, unavailable, mismatches)
    }

    fn field_of(entry: &CorpusMismatch) -> Option<RealAuditField> {
        match entry {
            CorpusMismatch::Real { field, .. } | CorpusMismatch::RealUnavailable { field, .. } => {
                Some(*field)
            }
            _ => None,
        }
    }

    /// The clang admission mirror predicts exactly the lane's live set on a
    /// synthetic C++ shape that exercises every documented refusal: body
    /// locals and function-scope statics of executables are dropped, a
    /// prototype/definition USR twin collapses to one row, an anonymous
    /// record is never an entity while its named fields are, owner-less
    /// function-pointer typedef parameters stay admitted, and each distinct
    /// include spelling captures one documentation row. The lambda closure
    /// covers the unrepresented-owner law: whatever the host libclang visits
    /// inside the closure, no predicted row may name closure-internal
    /// storage, because its semantic parent (the call operator) has no
    /// declaration row.
    #[test]
    fn clang_admission_mirror_predicts_the_lane_live_set() {
        use backend_frontend_clang::legacy::{
            ClangInput, ClangScratch, MAX_CLANG_DECLARATIONS, MAX_CLANG_DIAGNOSTICS,
            MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES, MAX_CLANG_REFERENCES, MAX_CLANG_TYPE_EDGES,
            MAX_CLANG_TYPES, SYMBOL_IDENTITY_BYTES, SymbolIdentity, collect_cancellable,
        };
        let source: &[u8] = br#"#include "mirror-include.h"
typedef int (*mirror_handler)(void *token, unsigned long len);
static int file_scope_storage = 1;
int prototype_only(int a, int b);
int mirror_twin(int x)
{
    int body_local = x;
    return body_local;
}
int mirror_twin(int x);
enum mirror_directive { mirror_not_limited, mirror_limited, mirror_fill = 4 };
struct { int anonymous_field; } anonymous_holder;
typedef struct { int named_member; } named_wrapper;
static int global_array[4] = {0, 1, 2, 3};
static const int closure_storage = ([](int lambda_param) -> int { int lambda_local = 2; return lambda_local; })(1);
int mirror_use(void)
{
    static int function_static = 7;
    return global_array[0] + function_static;
}
"#;
        let cancelled = AtomicBool::new(false);
        let input = ClangInput::from_profile(
            c"nudox-mirror-input",
            source,
            LanguageProfile::Cxx(CxxStandard::Cxx23),
        )
        .expect("synthetic input admitted");
        let mut declarations = vec![
            backend_frontend_clang::legacy::DeclarationFact {
                id: backend_frontend_clang::legacy::DeclarationId { raw: 0 },
                kind: backend_frontend_clang::legacy::DeclarationKind::Unknown,
                definition: backend_frontend_clang::legacy::DefinitionState::Declaration,
                virtuality: backend_frontend_clang::legacy::MethodVirtuality::NonVirtual,
                identity: None,
                span: backend_frontend_clang::legacy::SourceSpan { start: 0, end: 0 },
                name: None,
                owner: None,
                documentation: None,
                storage: backend_frontend_clang::legacy::StorageClass::None,
                type_root: None,
                enum_underlying: None,
            };
            MAX_CLANG_DECLARATIONS
        ];
        let mut types = vec![
            backend_frontend_clang::legacy::TypeFact {
                id: backend_frontend_clang::legacy::TypeId { raw: 0 },
                kind: backend_frontend_clang::legacy::TypeKind::Unknown,
                qualifiers: backend_frontend_clang::legacy::TypeQualifiers {
                    is_const: false,
                    is_volatile: false,
                    is_restrict: false,
                },
                declaration: None,
                array_len: None,
                builtin: None,
                size_bits: None,
                align_bits: None,
                is_variadic: false,
            };
            MAX_CLANG_TYPES
        ];
        let mut type_edges = vec![
            backend_frontend_clang::legacy::TypeEdge {
                source: backend_frontend_clang::legacy::TypeId { raw: 0 },
                relation: backend_frontend_clang::legacy::TypeRelation::Pointee,
                target: backend_frontend_clang::legacy::TypeId { raw: 0 },
            };
            MAX_CLANG_TYPE_EDGES
        ];
        let mut references = vec![
            backend_frontend_clang::legacy::ReferenceFact {
                kind: backend_frontend_clang::legacy::ReferenceKind::Value,
                span: backend_frontend_clang::legacy::SourceSpan { start: 0, end: 0 },
                owner: None,
                target: backend_frontend_clang::legacy::ReferenceTarget::Unresolved,
            };
            MAX_CLANG_REFERENCES
        ];
        let mut diagnostics = vec![
            backend_frontend_clang::legacy::DiagnosticFact {
                severity: backend_frontend_clang::legacy::DiagnosticSeverity::Unknown,
                location: None,
                category: 0,
                message: None,
            };
            MAX_CLANG_DIAGNOSTICS
        ];
        let mut includes = vec![
            backend_frontend_clang::legacy::IncludeFact {
                kind: backend_frontend_clang::legacy::SourceDependencyKind::Include,
                span: backend_frontend_clang::legacy::SourceSpan { start: 0, end: 0 },
                resolved: None,
            };
            MAX_CLANG_INCLUDES
        ];
        let mut overrides = vec![
            backend_frontend_clang::legacy::OverrideFact {
                source: SymbolIdentity {
                    bytes: [0; SYMBOL_IDENTITY_BYTES]
                },
                target: SymbolIdentity {
                    bytes: [0; SYMBOL_IDENTITY_BYTES]
                },
                target_file: None,
            };
            MAX_CLANG_OVERRIDES
        ];
        let facts = collect_cancellable(
            input,
            ClangScratch {
                declarations: &mut declarations,
                types: &mut types,
                type_edges: &mut type_edges,
                references: &mut references,
                diagnostics: &mut diagnostics,
                includes: &mut includes,
                overrides: &mut overrides,
            },
            &cancelled,
        )
        .expect("synthetic facts collect");
        let (rows, _pushed) = clang_admitted_rows(&facts, source);
        let predicted = |name: &[u8]| rows.iter().any(|row| &row.name[..] == name);
        // Admitted: typedefs, owner-less function-pointer parameters,
        // file-scope storage, prototypes, the twin's surviving definition,
        // the enum and its enumerators, anonymous-record fields, the
        // named wrapper typedef and its field, the closure holder itself,
        // and the last executable.
        for admitted in [
            &b"mirror_handler"[..],
            b"token",
            b"len",
            b"file_scope_storage",
            b"prototype_only",
            b"a",
            b"b",
            b"mirror_twin",
            b"mirror_directive",
            b"mirror_not_limited",
            b"mirror_limited",
            b"mirror_fill",
            b"anonymous_holder",
            b"anonymous_field",
            b"named_wrapper",
            b"named_member",
            b"global_array",
            b"closure_storage",
            b"mirror_use",
        ] {
            assert!(
                predicted(admitted),
                "an honest lowering admits {admitted:?}: {rows:?}"
            );
        }
        // The USR twin collapses: prototype and definition are one fact.
        assert_eq!(
            rows.iter()
                .filter(|row| &row.name[..] == b"mirror_twin")
                .count(),
            1,
            "a prototype/definition twin predicts exactly one row"
        );
        // Dropped: body locals and block-scope statics of executables share
        // the documented refusal.
        for dropped in [&b"body_local"[..], b"function_static"] {
            assert!(
                !predicted(dropped),
                "block-scope storage of an executable is never predicted: {dropped:?}"
            );
        }
        // Unrepresented owners: any scratch row whose owner identity has no
        // retained declaration row (the visited lambda closure's internals)
        // is never predicted.
        let unrepresented: Vec<&[u8]> = facts
            .declarations
            .iter()
            .filter(|declaration| {
                declaration.owner.is_some_and(|owner| {
                    !facts
                        .declarations
                        .iter()
                        .any(|candidate| candidate.identity == Some(owner))
                })
            })
            .filter_map(|declaration| {
                declaration
                    .name
                    .map(|span| &source[span.start as usize..span.end as usize])
            })
            .collect();
        for name in &unrepresented {
            assert!(
                !predicted(name),
                "unrepresented-owner storage is never predicted: {name:?}"
            );
        }
        // The closure holder's initializer visits closure internals on the
        // measured host; the refusal law must have something to refuse.
        assert!(
            !unrepresented.is_empty(),
            "the synthetic closure must exercise the unrepresented-owner law"
        );
        // Documentation: every admitted row plus one captured-empty module
        // row per distinct include spelling.
        let include_rows = clang_include_module_rows(&facts, source);
        assert_eq!(include_rows, 1, "one distinct include spelling");
    }

    /// The Rust mirror obeys the lane's cfg law: a `#[cfg]`-disabled inline
    /// module is dropped subtree and all. rust-analyzer projects the disabled
    /// module through the written syntax walk while omitting it from HIR, so
    /// an unfiltered mirror would predict the module, its nested items, and
    /// everything the module lookup twins onto the enabled same-name branch
    /// — rows the lane honestly drops (the dominant shape of cfg-gated fleet
    /// crates like `windows_x86_64_gnullvm`).
    #[test]
    fn rust_mirror_drops_a_cfg_disabled_module_subtree() {
        use crate::authority::ToolSlot;
        use backend_engine::driver::NativeTool;
        let slot = ToolSlot::resolve(NativeTool::Rustc);
        let Some(host) = slot.host() else {
            eprintln!("no host rust toolchain; skipping the cfg-disabled-module mirror repro");
            return;
        };
        // `cfg(any())` is provably false under every resolved option set, so
        // the disabled-module law fires without any feature-policy coupling.
        let source: &[u8] = b"\
pub struct Kept;

#[cfg(any())]
mod vanished {
    pub struct Ghost;

    pub const PHANTOM: u8 = 7;

    pub fn phantom() -> u8 {
        PHANTOM
    }
}

pub fn keep_going() -> Kept {
    Kept
}
";
        let fixture = crate::authority::rust_fixture_for_source(0, source, &host)
            .expect("synthetic Cargo fixture");
        let cancelled = AtomicBool::new(false);
        let expectation = rust_authority(
            &fixture.project,
            &cancelled,
            Instant::now() + crate::DEADLINE,
            source,
        );
        let PlaneExpectation::Grounded(rows) = &expectation.declarations else {
            panic!("the rust mirror must ground the declarations plane");
        };
        let predicted = |name: &[u8]| {
            rows.iter()
                .any(|row| row.name.as_ref() == name && row.name_span.is_some())
        };
        assert!(
            predicted(b"Kept") && predicted(b"keep_going"),
            "the enabled rows must still be predicted: {rows:?}"
        );
        for dropped in [&b"vanished"[..], b"Ghost", b"PHANTOM", b"phantom"] {
            assert!(
                !rows.iter().any(|row| row.name.as_ref() == dropped),
                "a cfg-disabled module's whole subtree must stay unpredicted: {dropped:?} in {rows:?}"
            );
        }
        // The module-scope walk is HIR-based and already omits the disabled
        // module; the written-walk filter above is what closes the leak.
        // Nothing may predict the disabled subtree from either walk.
    }

    /// Every grounded plane passes on a healthy owned/reopened pair: no
    /// mismatch, no unavailable terminal, and no not-compared field.
    #[test]
    fn all_grounded_planes_verify_a_healthy_image() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let expectation = grounded_expectation();
        let (not_compared, unavailable, mismatches) = audit(&ir, &image, &expectation);
        assert_eq!(
            not_compared, 0,
            "no plane may be not compared: {unavailable:?}"
        );
        assert!(
            unavailable.is_empty(),
            "no unavailable terminals: {unavailable:?}"
        );
        assert!(mismatches.is_empty(), "no mismatches: {mismatches:?}");
    }

    /// A declaration the IR dropped must surface as a fatal declarations
    /// mismatch naming the lost row.
    #[test]
    fn declaration_plane_catches_a_dropped_declaration() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let mut expectation = grounded_expectation();
        if let PlaneExpectation::Grounded(rows) = &mut expectation.declarations {
            rows.push(declaration(ItemKind::Function, b"ghost", None));
        }
        let (_, _, mismatches) = audit(&ir, &image, &expectation);
        assert!(
            mismatches
                .iter()
                .any(|entry| field_of(entry) == Some(RealAuditField::Declarations)),
            "a dropped authority row must be a declarations mismatch: {mismatches:?}"
        );
    }

    /// A row whose kind drifted (the IR holds `make` as a function, the
    /// authority claims a constant) must fail the join by kind.
    #[test]
    fn declaration_plane_catches_kind_drift() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let mut expectation = grounded_expectation();
        if let PlaneExpectation::Grounded(rows) = &mut expectation.declarations {
            rows[1] = declaration(ItemKind::Constant, b"make", Some((60, 64)));
        }
        let (_, _, mismatches) = audit(&ir, &image, &expectation);
        assert!(
            mismatches
                .iter()
                .any(|entry| field_of(entry) == Some(RealAuditField::Declarations)),
            "kind drift must be a declarations mismatch: {mismatches:?}"
        );
    }

    /// A joined entity whose observed shape class differs from the
    /// authority's prediction must surface as a types mismatch, while the
    /// agreeing prediction stays green.
    #[test]
    fn type_plane_catches_shape_drift() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let mut expectation = grounded_expectation();
        if let PlaneExpectation::Grounded(rows) = &mut expectation.types {
            rows[0].shape = ObservedTypeShape::Nominal;
        }
        let (_, _, mismatches) = audit(&ir, &image, &expectation);
        assert!(
            mismatches
                .iter()
                .any(|entry| field_of(entry) == Some(RealAuditField::Types)),
            "shape drift must be a types mismatch: {mismatches:?}"
        );
    }

    /// Documentation parity is a count over captured rows: a lost
    /// documentation fact lowers the captured count and must mismatch.
    #[test]
    fn documentation_plane_catches_a_lost_doc() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let mut expectation = grounded_expectation();
        expectation.documentation = PlaneExpectation::Grounded(2);
        let (_, _, mismatches) = audit(&ir, &image, &expectation);
        assert!(
            mismatches
                .iter()
                .any(|entry| field_of(entry) == Some(RealAuditField::Documentation)),
            "a lost documentation fact must be a documentation mismatch: {mismatches:?}"
        );
    }

    /// A relation whose target never appears as an occurrence or link must
    /// surface as a relations mismatch, and a corrupted site must fail the
    /// located join.
    #[test]
    fn relation_plane_catches_a_missing_reference() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        for relation in [
            AuthorityRelation {
                target: b"Ghost".to_vec().into_boxed_slice(),
                site: (40, 46),
                located: true,
            },
            AuthorityRelation {
                target: b"Widget".to_vec().into_boxed_slice(),
                site: (99, 105),
                located: true,
            },
        ] {
            let mut expectation = grounded_expectation();
            expectation.relations = PlaneExpectation::Grounded(vec![relation]);
            let (_, _, mismatches) = audit(&ir, &image, &expectation);
            assert!(
                mismatches
                    .iter()
                    .any(|entry| field_of(entry) == Some(RealAuditField::Relations)),
                "a missing or mislocated reference must be a relations mismatch: {mismatches:?}"
            );
        }
    }

    /// A census whose root count no longer matches the authority's
    /// top-level declaration count must surface as a discovery mismatch.
    #[test]
    fn discovery_plane_catches_census_corruption() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let mut expectation = grounded_expectation();
        expectation.discovery = PlaneExpectation::Grounded(1);
        let (_, _, mismatches) = audit(&ir, &image, &expectation);
        assert!(
            mismatches
                .iter()
                .any(|entry| field_of(entry) == Some(RealAuditField::Discovery)),
            "census corruption must be a discovery mismatch: {mismatches:?}"
        );
    }

    /// A plane the lane cannot ground stays a typed not-compared outcome:
    /// counted, pushed as a RealUnavailable terminal with the NotCompared
    /// cause, and never a parity mismatch — the audit verdict stays clean
    /// while the row is honestly not verified.
    #[test]
    fn not_compared_plane_counts_without_claiming_parity() {
        let ir = synthetic_ir();
        let bytes = reopened_bytes(&ir);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let expectation = AuthorityExpectation::default();
        let (not_compared, unavailable, mismatches) = audit(&ir, &image, &expectation);
        assert_eq!(
            not_compared, 8,
            "six single-field planes plus canonical-type and render: {not_compared}"
        );
        assert!(
            mismatches.is_empty(),
            "not-compared is never a mismatch: {mismatches:?}"
        );
        assert_eq!(
            unavailable.len(),
            8,
            "every not-compared field stays counted"
        );
        assert!(unavailable.iter().all(|entry| matches!(
            entry,
            CorpusMismatch::RealUnavailable {
                cause: AuthorityUnavailableCause::NotCompared,
                ..
            }
        )));
    }
}
