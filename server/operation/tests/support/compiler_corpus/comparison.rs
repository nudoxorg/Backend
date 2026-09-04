//! Typed mismatch categories and permutation comparison.
//!
//! Every comparison keeps source-bound expectations and observed values in
//! closed enums; deferred observers remain explicit red outcomes.

use super::*;
use super::observation::{
    digest_authority_unavailable, digest_compact, digest_owned, digest_render,
    digest_reopened, digest_recipe, digest_source, digest_terminal, digest_u64,
    item_kind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Plane {
    Documentation,
    Attributes,
    Provenance,
    Occurrences,
    Extension,
    DurableSemanticData,
    DurableTypeFacts,
    DurableDocumentation,
    DurableExtensions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuthorityPlane {
    Source,
    SourceFile,
    Members,
    SemanticType,
    Visibility,
    Documentation,
    Attributes,
    LanguageExtension,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pass {
    Original,
    Reverse,
    FixedShuffle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PermutationField {
    Source,
    Recipe,
    Owned,
    Compact,
    Reopened,
    NeutralRender,
    DialectRender,
    Terminal,
    Availability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticImageField {
    Status,
    Identity,
    ImageFacts,
    Census,
    Entities,
    Types,
    Externals,
    Links,
    Occurrences,
    Extensions,
    SourceSpans,
    CanonicalType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RealAuditField {
    Source,
    Profile,
    Recipe,
    Toolchain,
    SourceSpans,
    Declarations,
    Types,
    Relations,
    Documentation,
    Extensions,
    Discovery,
    CanonicalType,
    SemanticImage,
    Reopened,
    Render,
}

impl PermutationField {
    const OUTPUT: [Self; 7] = [
        Self::Source,
        Self::Recipe,
        Self::Owned,
        Self::Compact,
        Self::Reopened,
        Self::NeutralRender,
        Self::DialectRender,
    ];
}

/// Exact source-vs-output mismatch.  Every arm retains typed expected and
/// observed facts; display text is intentionally not part of this state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CorpusMismatch {
    Source {
        key: CaseKey,
        expected: SourceIdentity,
        observed: SourceIdentity,
    },
    Recipe {
        key: CaseKey,
        expected: compiler_vocabulary::CompileRecipeFact,
        observed: compiler_vocabulary::CompileRecipeFact,
    },
    DeclarationCount {
        key: CaseKey,
        expected: CountExpectation,
        observed: u32,
    },
    PrimaryMultiplicity {
        key: CaseKey,
        expected: u16,
        observed: u16,
    },
    Primary {
        key: CaseKey,
        expected_kind: EntityKind,
        observed_kind: Option<ItemKind>,
        expected_name: Digest,
        observed_name: Digest,
    },
    Type {
        key: CaseKey,
        expected: ExpectedType,
        observed: ObservedTypeShape,
    },
    Parent {
        key: CaseKey,
        expected: multilingual_corpus::ParentExpectation,
        observed: Option<EntityId>,
    },
    AuthorityParent {
        key: CaseKey,
        expected: multilingual_corpus::ParentExpectation,
        observed: Option<EntityAuthorityFacts>,
    },
    AuthorityPlane {
        key: CaseKey,
        plane: AuthorityPlane,
        expected: FactAvailability,
        observed: Option<EntityAuthorityFacts>,
    },
    Members {
        key: CaseKey,
        expected: bool,
        observed_count: u32,
    },
    Member {
        key: CaseKey,
        expected: Option<Digest>,
        observed: Option<Digest>,
    },
    GenericParameters {
        key: CaseKey,
        expected: CountExpectation,
        observed: CountObservation,
    },
    Plane {
        key: CaseKey,
        plane: Plane,
        expected: PlaneAvailability,
        observed: PlaneObservation,
    },
    DurablePlane {
        key: CaseKey,
        plane: Plane,
        expected: PlaneObservation,
        observed: PlaneObservation,
    },
    Relation {
        key: CaseKey,
        expected: RelationExpectation,
        observed_occurrences: CountObservation,
        observed_links: CountObservation,
    },
    Render {
        key: CaseKey,
        dialect: bool,
        expected: RenderAvailability,
        observed: RenderVerdict,
    },
    Compact {
        key: CaseKey,
        expected_fragment: Digest,
        observed_fragment: Digest,
        expected_entities: CountExpectation,
        observed_entities: u32,
        expected_kind: EntityKind,
        observed_kind: Option<EntityKind>,
        expected_name: Digest,
        observed_name: Digest,
        expected_type: ExpectedType,
        observed_type: Option<TypeNode>,
    },
    CompactMultiplicity {
        key: CaseKey,
        expected: u16,
        observed: u16,
    },
    Reopened {
        key: CaseKey,
        expected_source: SourceIdentity,
        observed_source: SourceIdentity,
        expected_recipe: compiler_vocabulary::CompileRecipeFact,
        observed_recipe: compiler_vocabulary::CompileRecipeFact,
        expected_fragment: Digest,
        observed_fragment: Digest,
        expected_ranges: Digest,
        observed_ranges: Digest,
        expected_census: compiler_ir::SemanticCensus,
        observed_census: compiler_ir::SemanticCensus,
    },
    SemanticImage {
        key: CaseKey,
        field: SemanticImageField,
        expected: observation::SemanticObservation,
        observed: observation::SemanticObservation,
    },
    Real {
        case: inventory::RealPackageCase,
        field: RealAuditField,
        expected: Digest,
        observed: Digest,
    },
    RealUnavailable {
        case: inventory::RealPackageCase,
        field: RealAuditField,
        cause: AuthorityUnavailableCause,
    },
    RealTerminal {
        case: inventory::RealPackageCase,
        source: SourceIdentity,
        terminal: CompileTerminalKind,
    },
    CompileTerminal {
        key: CaseKey,
        source: SourceIdentity,
        terminal: CompileTerminalKind,
    },
    Availability {
        key: CaseKey,
        expected: CaseAvailability,
        observed: AuthorityUnavailableCause,
    },
    Permutation {
        key: CaseKey,
        pass: Pass,
        field: PermutationField,
        expected: Digest,
        observed: Digest,
    },
}


pub(super) fn count_matches(expected: CountExpectation, observed: u32) -> bool {
    match expected {
        CountExpectation::Exact(count) => observed == u32::from(count),
        // Lower bounds and deferred contracts are deliberately not source
        // parity. They remain representable so an adapter can report the
        // exact typed gap, but neither may certify a row.
        CountExpectation::Deferred => false,
    }
}

pub(super) fn count_observation_matches(expected: CountExpectation, observed: CountObservation) -> bool {
    match observed {
        CountObservation::Exact(count) => count_matches(expected, count),
        CountObservation::Unavailable | CountObservation::Unsupported => false,
    }
}

pub(super) fn plane_matches(expected: PlaneAvailability, observed: PlaneObservation) -> bool {
    match expected {
        PlaneAvailability::Required => observed == PlaneObservation::Captured,
        // An observer-deferred source contract is an explicit red result. It
        // must never turn an unread plane into a successful comparison.
        PlaneAvailability::ObserverUnavailable => false,
        PlaneAvailability::Unavailable => observed == PlaneObservation::Unavailable,
        PlaneAvailability::Unsupported => observed == PlaneObservation::Unsupported,
    }
}

fn expected_builtin(primitive: PrimitiveType) -> BuiltinType {
    match primitive {
        PrimitiveType::Bool => BuiltinType::Bool,
        PrimitiveType::I32 => BuiltinType::I32,
        PrimitiveType::String => BuiltinType::String,
    }
}

pub(super) fn type_matches(expected: ExpectedType, observed: ObservedTypeShape) -> bool {
    match expected {
        ExpectedType::Primitive(primitive) => {
            observed == ObservedTypeShape::Primitive(expected_builtin(primitive))
        }
        ExpectedType::Builtin(builtin) => observed == ObservedTypeShape::Primitive(builtin),
        ExpectedType::Callable => observed == ObservedTypeShape::Callable,
        ExpectedType::Nominal => observed == ObservedTypeShape::Nominal,
        ExpectedType::Structural => observed == ObservedTypeShape::Structural,
        ExpectedType::Generic => observed == ObservedTypeShape::Generic,
        ExpectedType::Reference => observed == ObservedTypeShape::Reference,
    }
}

pub(super) fn compact_type_matches(expected: ExpectedType, observed: Option<TypeNode>) -> bool {
    match expected {
        ExpectedType::Primitive(primitive) => {
            observed == Some(TypeNode::Primitive(primitive))
        }
        ExpectedType::Builtin(BuiltinType::Bool) => {
            observed == Some(TypeNode::Primitive(PrimitiveType::Bool))
        }
        ExpectedType::Builtin(BuiltinType::I32) => {
            observed == Some(TypeNode::Primitive(PrimitiveType::I32))
        }
        ExpectedType::Builtin(BuiltinType::String) => {
            observed == Some(TypeNode::Primitive(PrimitiveType::String))
        }
        // The compact opcode table has no faithful representation for these
        // language-specific builtin roles; the rich type-fact observer must
        // carry the exact role before this can become a parity assertion.
        ExpectedType::Builtin(_) => false,
        ExpectedType::Reference => matches!(observed, Some(TypeNode::Reference(_))),
        ExpectedType::Callable | ExpectedType::Nominal | ExpectedType::Structural
        | ExpectedType::Generic => false,
    }
}

pub(super) fn expected_mismatches(
    key: CaseKey,
    expected: ExpectedFacts,
    expected_symbol: &[u8],
    expected_member: Option<&[u8]>,
    expected_source: SourceIdentity,
    expected_recipe: compiler_vocabulary::CompileRecipeFact,
    expected_fragment: Digest,
    expected_ranges: Digest,
    output: &CaseOutputObservation,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    if output.source != expected_source {
        mismatches.push(CorpusMismatch::Source {
            key,
            expected: expected_source,
            observed: output.source,
        });
    }
    if output.recipe != expected_recipe {
        mismatches.push(CorpusMismatch::Recipe {
            key,
            expected: expected_recipe,
            observed: output.recipe,
        });
    }

    if !count_matches(expected.declarations, output.owned.declarations) {
        mismatches.push(CorpusMismatch::DeclarationCount {
            key,
            expected: expected.declarations,
            observed: output.owned.declarations,
        });
    }

    if output.owned.primary_matches != 1 {
        mismatches.push(CorpusMismatch::PrimaryMultiplicity {
            key,
            expected: 1,
            observed: output.owned.primary_matches,
        });
    }

    let expected_name = digest_bytes(expected_symbol);
    let primary = output.owned.primary;
    match primary {
        Some(primary) => {
            let expected_kind = item_kind(expected.primary_kind);
            if primary.kind != expected_kind || primary.name != expected_name {
                mismatches.push(CorpusMismatch::Primary {
                    key,
                    expected_kind: expected.primary_kind,
                    observed_kind: Some(primary.kind),
                    expected_name,
                    observed_name: primary.name,
                });
            }
            if !type_matches(expected.primary_type, primary.type_shape) {
                mismatches.push(CorpusMismatch::Type {
                    key,
                    expected: expected.primary_type,
                    observed: primary.type_shape,
                });
            }
            match expected.parent {
                multilingual_corpus::ParentExpectation::Root
                    if primary.parent.is_some() => mismatches.push(CorpusMismatch::Parent {
                        key,
                        expected: expected.parent,
                        observed: primary.parent,
                    }),
                multilingual_corpus::ParentExpectation::Nested
                    if primary.parent.is_none() => mismatches.push(CorpusMismatch::Parent {
                        key,
                        expected: expected.parent,
                        observed: primary.parent,
                    }),
                multilingual_corpus::ParentExpectation::Unavailable
                | multilingual_corpus::ParentExpectation::Root
                | multilingual_corpus::ParentExpectation::Nested => {}
            }
            let authority_parent_matches = primary.authority.is_some_and(|facts| {
                match expected.parent {
                    multilingual_corpus::ParentExpectation::Root => {
                        facts.parentage == ParentageAuthority::Root
                    }
                    multilingual_corpus::ParentExpectation::Nested => {
                        matches!(facts.parentage, ParentageAuthority::Bound(_))
                    }
                    multilingual_corpus::ParentExpectation::Unavailable => {
                        facts.parentage == ParentageAuthority::Unavailable
                    }
                }
            });
            if !authority_parent_matches {
                mismatches.push(CorpusMismatch::AuthorityParent {
                    key,
                    expected: expected.parent,
                    observed: primary.authority,
                });
            }
            check_authority_plane(
                key,
                AuthorityPlane::Source,
                expected.provenance,
                primary.authority,
                mismatches,
            );
            check_authority_plane(
                key,
                AuthorityPlane::SourceFile,
                expected.source_file,
                primary.authority,
                mismatches,
            );
            check_authority_plane(
                key,
                AuthorityPlane::SemanticType,
                PlaneAvailability::Required,
                primary.authority,
                mismatches,
            );
            check_authority_plane(
                key,
                AuthorityPlane::Documentation,
                expected.documentation,
                primary.authority,
                mismatches,
            );
            check_authority_plane(
                key,
                AuthorityPlane::Attributes,
                expected.attributes,
                primary.authority,
                mismatches,
            );
            check_authority_plane(
                key,
                AuthorityPlane::LanguageExtension,
                expected.extension,
                primary.authority,
                mismatches,
            );
            if expected.nested_member {
                check_authority_plane(
                    key,
                    AuthorityPlane::Members,
                    PlaneAvailability::Required,
                    primary.authority,
                    mismatches,
                );
            }
            if expected.nested_member != (primary.members > 0) {
                mismatches.push(CorpusMismatch::Members {
                    key,
                    expected: expected.nested_member,
                    observed_count: primary.members,
                });
            }
            if let Some(expected_member) = expected_member {
                let expected_member = Some(digest_bytes(expected_member));
                if primary.first_member != expected_member {
                    mismatches.push(CorpusMismatch::Member {
                        key,
                        expected: expected_member,
                        observed: primary.first_member,
                    });
                }
            }
        }
        None => {
            mismatches.push(CorpusMismatch::Primary {
                key,
                expected_kind: expected.primary_kind,
                observed_kind: None,
                expected_name,
                observed_name: [0_u8; 32],
            });
            mismatches.push(CorpusMismatch::Type {
                key,
                expected: expected.primary_type,
                observed: ObservedTypeShape::Absent,
            });
        }
    }

    if !count_observation_matches(expected.generic_parameters, output.owned.generic_parameters) {
        mismatches.push(CorpusMismatch::GenericParameters {
            key,
            expected: expected.generic_parameters,
            observed: output.owned.generic_parameters,
        });
    }

    check_plane(
        key,
        Plane::Documentation,
        expected.documentation,
        output.owned.documentation,
        mismatches,
    );
    check_plane(
        key,
        Plane::Attributes,
        expected.attributes,
        output.owned.attributes,
        mismatches,
    );
    check_plane(
        key,
        Plane::Provenance,
        expected.provenance,
        output.owned.source_plane,
        mismatches,
    );
    check_plane(
        key,
        Plane::Extension,
        expected.extension,
        output.owned.extension,
        mismatches,
    );

    let observed_occurrences = match output.owned.occurrences {
        CountObservation::Exact(count) if count > 0 => PlaneObservation::Captured,
        CountObservation::Exact(_) => PlaneObservation::Unavailable,
        CountObservation::Unavailable => PlaneObservation::ObserverUnavailable,
        CountObservation::Unsupported => PlaneObservation::Unsupported,
    };
    check_plane(
        key,
        Plane::Occurrences,
        expected.occurrences,
        observed_occurrences,
        mismatches,
    );

    match expected.relation {
        RelationExpectation::None | RelationExpectation::Unsupported => {}
        RelationExpectation::OccurrenceRequired => {
            if !matches!(
                output.owned.occurrences,
                CountObservation::Exact(count) if count > 0
            ) {
                mismatches.push(CorpusMismatch::Relation {
                    key,
                    expected: expected.relation,
                    observed_occurrences: output.owned.occurrences,
                    observed_links: output.owned.links,
                });
            }
        }
        RelationExpectation::OverloadRequired => {
            let has_relation = matches!(
                output.owned.occurrences,
                CountObservation::Exact(count) if count > 0
            ) || matches!(output.owned.links, CountObservation::Exact(count) if count > 0);
            if !has_relation || !count_matches(expected.declarations, output.owned.declarations) {
                mismatches.push(CorpusMismatch::Relation {
                    key,
                    expected: expected.relation,
                    observed_occurrences: output.owned.occurrences,
                    observed_links: output.owned.links,
                });
            }
        }
    }

    let compact = output.compact;
    if compact.primary_matches != 1 {
        mismatches.push(CorpusMismatch::CompactMultiplicity {
            key,
            expected: 1,
            observed: compact.primary_matches,
        });
    }
    if !count_matches(expected.declarations, compact.census.entities)
        || compact.primary_kind != Some(expected.primary_kind)
        || compact.primary_name != expected_name
        || !compact_type_matches(expected.primary_type, compact.primary_type)
    {
        mismatches.push(CorpusMismatch::Compact {
            key,
            expected_fragment,
            observed_fragment: compact.fragment,
            expected_entities: expected.declarations,
            observed_entities: compact.census.entities,
            expected_kind: expected.primary_kind,
            observed_kind: compact.primary_kind,
            expected_name,
            observed_name: compact.primary_name,
            expected_type: expected.primary_type,
            observed_type: compact.primary_type,
        });
    }

    check_plane(
        key,
        Plane::DurableSemanticData,
        PlaneAvailability::ObserverUnavailable,
        compact.semantic_data,
        mismatches,
    );
    check_plane(
        key,
        Plane::DurableTypeFacts,
        PlaneAvailability::ObserverUnavailable,
        compact.type_facts,
        mismatches,
    );
    check_plane(
        key,
        Plane::DurableDocumentation,
        PlaneAvailability::ObserverUnavailable,
        compact.documentation,
        mismatches,
    );
    check_plane(
        key,
        Plane::DurableExtensions,
        PlaneAvailability::ObserverUnavailable,
        compact.extensions,
        mismatches,
    );

    let expected_reopened = ReopenedObservation {
        fragment: expected_fragment,
        source: expected_source,
        recipe: expected_recipe,
        ranges: expected_ranges,
        census: compact.census,
        semantic_data: compact.semantic_data,
        occurrences: compact.occurrences,
        type_facts: compact.type_facts,
        documentation: compact.documentation,
        extensions: compact.extensions,
        semantic: output.owned.semantic,
    };
    if output.reopened != expected_reopened {
        mismatches.push(CorpusMismatch::Reopened {
            key,
            expected_source: expected_reopened.source,
            observed_source: output.reopened.source,
            expected_recipe: expected_reopened.recipe,
            observed_recipe: output.reopened.recipe,
            expected_fragment: expected_reopened.fragment,
            observed_fragment: output.reopened.fragment,
            expected_ranges: expected_reopened.ranges,
            observed_ranges: output.reopened.ranges,
            expected_census: expected_reopened.census,
            observed_census: output.reopened.census,
        });
    }

    check_reopened_plane(
        key,
        Plane::DurableSemanticData,
        expected_reopened.semantic_data,
        output.reopened.semantic_data,
        mismatches,
    );

    check_semantic_image(
        key,
        expected_reopened.semantic,
        output.reopened.semantic,
        mismatches,
    );
    check_reopened_plane(
        key,
        Plane::DurableTypeFacts,
        expected_reopened.type_facts,
        output.reopened.type_facts,
        mismatches,
    );
    check_reopened_plane(
        key,
        Plane::DurableDocumentation,
        expected_reopened.documentation,
        output.reopened.documentation,
        mismatches,
    );
    check_reopened_plane(
        key,
        Plane::DurableExtensions,
        expected_reopened.extensions,
        output.reopened.extensions,
        mismatches,
    );

    check_render(
        key,
        false,
        expected.neutral_render,
        output.neutral_render,
        mismatches,
    );
    check_render(
        key,
        true,
        expected.dialect_render,
        output.dialect_render,
        mismatches,
    );
}

fn check_semantic_image(
    key: CaseKey,
    expected: observation::SemanticObservation,
    observed: observation::SemanticObservation,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let mut check = |field: SemanticImageField, equal: bool| {
        if !equal {
            mismatches.push(CorpusMismatch::SemanticImage {
                key,
                field,
                expected,
                observed,
            });
        }
    };
    check(SemanticImageField::Status, expected.status == observed.status);
    check(
        SemanticImageField::Identity,
        expected.identity == observed.identity,
    );
    check(
        SemanticImageField::ImageFacts,
        expected.image == observed.image,
    );
    check(SemanticImageField::Census, expected.census == observed.census);
    check(SemanticImageField::Entities, expected.entities == observed.entities);
    check(SemanticImageField::Types, expected.types == observed.types);
    check(
        SemanticImageField::Externals,
        expected.externals == observed.externals,
    );
    check(SemanticImageField::Links, expected.links == observed.links);
    check(
        SemanticImageField::Occurrences,
        expected.occurrences == observed.occurrences,
    );
    check(
        SemanticImageField::Extensions,
        expected.extensions == observed.extensions,
    );
    check(
        SemanticImageField::SourceSpans,
        expected.source_spans == observed.source_spans,
    );
    check(
        SemanticImageField::CanonicalType,
        expected.canonical_type == observed.canonical_type,
    );
}

fn check_plane(
    key: CaseKey,
    plane: Plane,
    expected: PlaneAvailability,
    observed: PlaneObservation,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    if !plane_matches(expected, observed) {
        mismatches.push(CorpusMismatch::Plane {
            key,
            plane,
            expected,
            observed,
        });
    }
}

fn authority_value(facts: EntityAuthorityFacts, plane: AuthorityPlane) -> FactAvailability {
    match plane {
        AuthorityPlane::Source => facts.source,
        AuthorityPlane::SourceFile => facts.source_file,
        AuthorityPlane::Members => facts.members,
        AuthorityPlane::SemanticType => facts.semantic_type,
        AuthorityPlane::Visibility => facts.visibility,
        AuthorityPlane::Documentation => facts.documentation,
        AuthorityPlane::Attributes => facts.attributes,
        AuthorityPlane::LanguageExtension => facts.language_extension,
    }
}

fn authority_expected(expected: PlaneAvailability) -> Option<FactAvailability> {
    match expected {
        PlaneAvailability::Required => Some(FactAvailability::Captured),
        PlaneAvailability::Unavailable => Some(FactAvailability::Unavailable),
        PlaneAvailability::ObserverUnavailable | PlaneAvailability::Unsupported => None,
    }
}

fn check_authority_plane(
    key: CaseKey,
    plane: AuthorityPlane,
    expected: PlaneAvailability,
    observed: Option<EntityAuthorityFacts>,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let Some(expected) = authority_expected(expected) else {
        return;
    };
    if observed.is_none_or(|facts| authority_value(facts, plane) != expected) {
        mismatches.push(CorpusMismatch::AuthorityPlane {
            key,
            plane,
            expected,
            observed,
        });
    }
}

fn check_reopened_plane(
    key: CaseKey,
    plane: Plane,
    expected: PlaneObservation,
    observed: PlaneObservation,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let matches = expected == observed
        && expected != PlaneObservation::ObserverUnavailable
        && observed != PlaneObservation::ObserverUnavailable;
    if !matches {
        mismatches.push(CorpusMismatch::DurablePlane {
            key,
            plane,
            expected,
            observed,
        });
    }
}

fn check_render(
    key: CaseKey,
    dialect: bool,
    expected: RenderAvailability,
    observed: RenderVerdict,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let matches = match expected {
        RenderAvailability::NeutralRequired => matches!(observed, RenderVerdict::Rendered(_)),
        // The dialect renderer is not part of this committed seam. Preserve
        // the typed unsupported observation, but keep the row explicitly red
        // so an unavailable renderer cannot certify source parity.
        RenderAvailability::DialectUnsupported => false,
        RenderAvailability::Unavailable => observed == RenderVerdict::Unavailable,
    };
    if !matches {
        let expected = match expected {
            RenderAvailability::NeutralRequired => RenderAvailability::NeutralRequired,
            RenderAvailability::DialectUnsupported => RenderAvailability::DialectUnsupported,
            RenderAvailability::Unavailable => RenderAvailability::Unavailable,
        };
        mismatches.push(CorpusMismatch::Render {
            key,
            dialect,
            expected,
            observed,
        });
    }
}

fn field_digest(case: CaseObservation, field: PermutationField) -> Digest {
    match case {
        CaseObservation::Output(output) => match field {
            PermutationField::Source => digest_source(output.source),
            PermutationField::Recipe => digest_recipe(output.recipe),
            PermutationField::Owned => digest_owned(output.owned),
            PermutationField::Compact => digest_compact(output.compact),
            PermutationField::Reopened => digest_reopened(output.reopened),
            PermutationField::NeutralRender => digest_render(output.neutral_render),
            PermutationField::DialectRender => digest_render(output.dialect_render),
            PermutationField::Terminal | PermutationField::Availability => digest_u64(0),
        },
        CaseObservation::LocallyUnavailable { cause, .. } => match field {
            PermutationField::Availability => digest_authority_unavailable(cause),
            _ => digest_u64(0),
        },
        CaseObservation::Terminal {
            source, terminal, ..
        } => match field {
            PermutationField::Source => digest_source(source),
            PermutationField::Terminal => digest_terminal(terminal),
            _ => digest_u64(0),
        },
    }
}

fn field_equal(
    expected: CaseObservation,
    observed: CaseObservation,
    field: PermutationField,
) -> bool {
    match (expected, observed, field) {
        (CaseObservation::Output(a), CaseObservation::Output(b), PermutationField::Source) => {
            a.source == b.source
        }
        (CaseObservation::Output(a), CaseObservation::Output(b), PermutationField::Recipe) => {
            a.recipe == b.recipe
        }
        (CaseObservation::Output(a), CaseObservation::Output(b), PermutationField::Owned) => {
            a.owned == b.owned
        }
        (CaseObservation::Output(a), CaseObservation::Output(b), PermutationField::Compact) => {
            a.compact == b.compact
        }
        (CaseObservation::Output(a), CaseObservation::Output(b), PermutationField::Reopened) => {
            a.reopened == b.reopened
        }
        (
            CaseObservation::Output(a),
            CaseObservation::Output(b),
            PermutationField::NeutralRender,
        ) => a.neutral_render == b.neutral_render,
        (
            CaseObservation::Output(a),
            CaseObservation::Output(b),
            PermutationField::DialectRender,
        ) => a.dialect_render == b.dialect_render,
        (
            CaseObservation::Terminal { source: a, .. },
            CaseObservation::Terminal { source: b, .. },
            PermutationField::Source,
        ) => a == b,
        (
            CaseObservation::Terminal { terminal: a, .. },
            CaseObservation::Terminal { terminal: b, .. },
            PermutationField::Terminal,
        ) => a == b,
        (
            CaseObservation::LocallyUnavailable { cause: a, .. },
            CaseObservation::LocallyUnavailable { cause: b, .. },
            PermutationField::Availability,
        ) => a == b,
        _ => false,
    }
}

pub(super) fn compare_passes(
    packages: &[CorpusPackage],
    expected: &PassObservation,
    observed: &PassObservation,
    pass: Pass,
    summary: &mut MatrixSummary,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    for (index, package) in packages.iter().copied().enumerate() {
        let expected_case = expected.cases[index];
        let observed_case = observed.cases[index];
        let fields: &[PermutationField] = match (expected_case, observed_case) {
            (CaseObservation::Output(_), CaseObservation::Output(_)) => {
                &PermutationField::OUTPUT
            }
            (CaseObservation::Terminal { .. }, CaseObservation::Terminal { .. }) => {
                &[PermutationField::Source, PermutationField::Terminal]
            }
            (CaseObservation::LocallyUnavailable { .. }, CaseObservation::LocallyUnavailable { .. }) => {
                &[PermutationField::Availability]
            }
            _ => &[PermutationField::Availability],
        };
        for field in fields {
            if !field_equal(expected_case, observed_case, *field) {
                let expected_digest = field_digest(expected_case, *field);
                let observed_digest = field_digest(observed_case, *field);
                mismatches.push(CorpusMismatch::Permutation {
                    key: case_key(package),
                    pass,
                    field: *field,
                    expected: expected_digest,
                    observed: observed_digest,
                });
                let lane = summary.lane_mut(package);
                lane.mismatches = lane.mismatches.saturating_add(1);
            }
        }
    }
}
