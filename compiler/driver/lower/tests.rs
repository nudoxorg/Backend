//! Red falsifiers for the multi-declaration semantic emission lane: every
//! lowered constructor, role, and name must change the committed fragment
//! bytes, every fact admission rejection must retain the exact offending fact
//! and cause, and an empty fact set must retain its exact current schema form.
use compiler_ir::{
    AtomListId, BuildError, ConcreteType, CorePayloadHash, EntityAuthorityFacts, EntityId,
    EntityKind, EntityVersion, FactAvailability, FragmentView, NominalRef, Occurrence,
    ParentageAuthority, PrepareError, PreparedFragment, ReopenedTypeParameterList, RustFacts,
    RustOwnership, SemanticTypeChild, SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag,
    SourceIdentity, TypeExpr, TypeHeader, TypePairPayload, TypeParameterListId, TypeQuadPayload,
    TypeTriplePayload, VariadicForm, Visibility,
};
use compiler_ir::{ProductChildRole, ProductConstructorFault, SemanticProductConstructor};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

use super::{
    AdmissionFault, EmissionExtension, FactFault, FactSet, MAX_ANONYMOUS_TYPE_ROWS,
    MAX_EMISSION_DOC_FRAGMENTS, MAX_EMISSION_FACTS, MAX_EMISSION_OCCURRENCES, MAX_EXTENSION_ATOMS,
    MAX_FACT_CHILDREN, MAX_REF_LISTS, MAX_TYPE_PARAMETERS, RejectedFact, SemanticFact,
};
use crate::types::{ParentageState, SourceSpanFact};

const SOURCE_BYTES: &[u8] = b"emission-seam-source";
const OUTPUT_CAPACITY: usize = 1_024;

#[derive(Debug, Error)]
enum TestError {
    #[error("fixture source length does not fit the source identity")]
    Source(#[from] core::num::TryFromIntError),
    #[error("unexpected fact rejection at {fact} with {name_len} name bytes: {cause:?}")]
    Rejected {
        fact: usize,
        name_len: usize,
        cause: FactFault,
    },
    #[error("fixture push unexpectedly succeeded")]
    UnexpectedPush,
    #[error("lane admission fault: {0:?}")]
    Admission(AdmissionFault),
    #[error("fragment validation failed")]
    Validate(#[from] compiler_ir::FragmentError),
    #[error("fixture prepare failed")]
    Prepare(#[from] PrepareError),
    #[error("fixture write failed")]
    Write(#[from] compiler_ir::WriteError),
    #[error("owned image projection failed")]
    Build(#[from] BuildError),
    #[error("fragment output tail changed")]
    Tail,
    #[error("{label} did not change the committed bytes")]
    Unchanged { label: &'static str },
}

fn identity() -> Result<SourceIdentity, TestError> {
    Ok(SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE_BYTES),
        byte_len: u32::try_from(SOURCE_BYTES.len())?,
    })
}

fn recipe() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE_BYTES),
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"emission-seam-toolchain"),
    )
}

fn rejected(failure: RejectedFact<'_>) -> TestError {
    TestError::Rejected {
        fact: failure.fact,
        name_len: failure.name.len(),
        cause: failure.cause,
    }
}

fn lane_fault(cause: FactFault) -> TestError {
    TestError::Rejected {
        fact: 0,
        name_len: 0,
        cause,
    }
}

/// Exact storage and cursors which `FactSet::push` owns. A failed push must
/// preserve even unopened future slots, then produce the same state and bytes
/// as a lane that never saw the rejected fact.
#[derive(Debug, Eq, PartialEq)]
struct FactTransactionState<'source> {
    len: usize,
    total_children: usize,
    total_type_children: usize,
    occurrence_len: usize,
    doc_len: usize,
    extension_atom_len: usize,
    type_parameter_len: usize,
    type_parameter_bound_len: usize,
    atom_list_len: usize,
    type_list_len: usize,
    entity_list_len: usize,
    anonymous_rows: usize,
    anonymous_children_total: usize,
    anonymous_child_pending: u8,
    computed_rows: usize,
    computed_children_total: usize,
    computed_child_pending: u8,
    kinds: Vec<EntityKind>,
    names: Vec<&'source [u8]>,
    type_records: Vec<SemanticTypeRecord<'source>>,
    type_child_targets: Vec<u32>,
    type_child_names: Vec<Option<&'source [u8]>>,
    type_child_flags: Vec<u8>,
    type_child_counts: Vec<u8>,
    type_child_starts: Vec<u32>,
    constructors: Vec<SemanticProductConstructor>,
    child_roles: Vec<ProductChildRole>,
    child_targets: Vec<u32>,
    child_counts: Vec<u8>,
    child_starts: Vec<u32>,
    extensions: Vec<Option<EmissionExtension>>,
    key_digests: Vec<CorePayloadHash>,
    visibility: Vec<Visibility>,
    visibility_captured: Vec<bool>,
    documentation_captured: Vec<bool>,
    parentage: Vec<ParentageState>,
    source_spans: Vec<Option<SourceSpanFact>>,
    members_captured: Vec<bool>,
    type_parameter_ranges: Vec<Option<super::StagedTypeParameterRange>>,
    anonymous_records: Vec<SemanticTypeRecord<'source>>,
    anonymous_owners: Vec<u32>,
    anonymous_child_starts: Vec<u32>,
    anonymous_child_counts: Vec<u8>,
    anonymous_child_targets: Vec<u32>,
    anonymous_child_names: Vec<Option<&'source [u8]>>,
    anonymous_child_flags: Vec<u8>,
    computed_records: Vec<SemanticTypeRecord<'source>>,
    computed_owners: Vec<u32>,
    computed_child_starts: Vec<u32>,
    computed_child_counts: Vec<u8>,
    computed_child_targets: Vec<u32>,
    computed_child_names: Vec<Option<&'source [u8]>>,
    computed_child_flags: Vec<u8>,
}

fn transaction_state<'source>(facts: &FactSet<'source>) -> FactTransactionState<'source> {
    FactTransactionState {
        len: facts.len,
        total_children: facts.total_children,
        total_type_children: facts.total_type_children,
        occurrence_len: facts.occurrence_len,
        doc_len: facts.doc_len,
        extension_atom_len: facts.extension_atom_len,
        type_parameter_len: facts.type_parameter_len,
        type_parameter_bound_len: facts.type_parameter_bound_len,
        atom_list_len: facts.atom_list_len,
        type_list_len: facts.type_list_len,
        entity_list_len: facts.entity_list_len,
        anonymous_rows: facts.anonymous_rows,
        anonymous_children_total: facts.anonymous_children_total,
        anonymous_child_pending: facts.anonymous_child_pending,
        computed_rows: facts.computed_rows,
        computed_children_total: facts.computed_children_total,
        computed_child_pending: facts.computed_child_pending,
        kinds: facts.kinds.to_vec(),
        names: facts.names.to_vec(),
        type_records: facts.type_records.to_vec(),
        type_child_targets: facts.type_child_targets.to_vec(),
        type_child_names: facts.type_child_names.to_vec(),
        type_child_flags: facts.type_child_flags.to_vec(),
        type_child_counts: facts.type_child_counts.to_vec(),
        type_child_starts: facts.type_child_starts.to_vec(),
        constructors: facts.constructors.to_vec(),
        child_roles: facts.child_roles.to_vec(),
        child_targets: facts.child_targets.to_vec(),
        child_counts: facts.child_counts.to_vec(),
        child_starts: facts.child_starts.to_vec(),
        extensions: facts.extensions.to_vec(),
        key_digests: facts.key_digests.to_vec(),
        visibility: facts.visibility.to_vec(),
        visibility_captured: facts.visibility_captured.to_vec(),
        documentation_captured: facts.documentation_captured.to_vec(),
        // Provenance is transaction state only for admitted facts. Keep the
        // active prefix here: unopened planned capacity must not make a
        // no-mutation comparison depend on irrelevant future slots.
        parentage: facts.provenance.parentage()[..facts.len].to_vec(),
        source_spans: facts.provenance.source_spans()[..facts.len].to_vec(),
        members_captured: facts
            .provenance
            .member_sets()
            .iter()
            .take(facts.len)
            .map(|capture| *capture == super::provenance::MemberSetCapture::Captured)
            .collect(),
        type_parameter_ranges: facts.type_parameter_ranges.to_vec(),
        anonymous_records: facts.anonymous_records[..facts.anonymous_rows].to_vec(),
        anonymous_owners: facts.anonymous_owners[..facts.anonymous_rows].to_vec(),
        anonymous_child_starts: facts.anonymous_child_starts[..facts.anonymous_rows].to_vec(),
        anonymous_child_counts: facts.anonymous_child_counts[..facts.anonymous_rows].to_vec(),
        anonymous_child_targets: facts.anonymous_child_targets[..facts.anonymous_children_total]
            .to_vec(),
        anonymous_child_names: facts.anonymous_child_names[..facts.anonymous_children_total]
            .to_vec(),
        anonymous_child_flags: facts.anonymous_child_flags[..facts.anonymous_children_total]
            .to_vec(),
        computed_records: facts.computed_records[..facts.computed_rows].to_vec(),
        computed_owners: facts.computed_owners[..facts.computed_rows].to_vec(),
        computed_child_starts: facts.computed_child_starts[..facts.computed_rows].to_vec(),
        computed_child_counts: facts.computed_child_counts[..facts.computed_rows].to_vec(),
        computed_child_targets: facts.computed_child_targets[..facts.computed_children_total]
            .to_vec(),
        computed_child_names: facts.computed_child_names[..facts.computed_children_total].to_vec(),
        computed_child_flags: facts.computed_child_flags[..facts.computed_children_total].to_vec(),
    }
}

/// Writes one admitted lane and returns the exact committed fragment bytes,
/// proving the untouched output tail stayed unchanged.
fn write(facts: &FactSet<'_>) -> Result<Vec<u8>, TestError> {
    let mut output = vec![0xa5_u8; OUTPUT_CAPACITY];
    let length = super::admit(facts, identity()?, recipe(), recipe().profile, &mut output)
        .map_err(TestError::Admission)?
        .len();
    if !output[length..].iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Tail);
    }
    output.truncate(length);
    Ok(output)
}

/// The exact compact type columns materialized by the owned compatibility
/// projection. Pending child recovery must leave them identical to a control
/// lane, not merely make subsequent admission succeed.
#[derive(Debug, Eq, PartialEq)]
struct OwnedTypeProjection {
    headers: Vec<TypeHeader>,
    pairs: Vec<TypePairPayload>,
    triples: Vec<TypeTriplePayload>,
    quads: Vec<TypeQuadPayload>,
}

fn owned_type_projection(facts: &FactSet<'_>) -> Result<OwnedTypeProjection, TestError> {
    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    let columns = ir.storage_columns();
    Ok(OwnedTypeProjection {
        headers: columns.types.headers.to_vec(),
        pairs: columns.types.pairs.to_vec(),
        triples: columns.types.triples.to_vec(),
        quads: columns.types.quads.to_vec(),
    })
}

/// Typed identity/payload columns from the exact owned projection.  Tests
/// compare these facts directly rather than rendering IDs or formatting IR.
fn entity_versions(facts: &FactSet<'_>) -> Result<Vec<EntityVersion>, TestError> {
    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    Ok(ir.items().map(|item| item.version()).collect())
}

/// The owned topology projections affected by transaction-local provenance.
/// Keeping only these borrowed-view facts makes conflict falsifiers compare
/// the observable result without reaching into `Ir` storage internals.
#[derive(Debug, Eq, PartialEq)]
struct OwnedTopologyProjection {
    sources: Vec<Option<(u32, u32)>>,
    parents: Vec<Option<EntityId>>,
    members: Vec<Vec<EntityId>>,
}

fn owned_topology_projection(facts: &FactSet<'_>) -> Result<OwnedTopologyProjection, TestError> {
    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    Ok(OwnedTopologyProjection {
        sources: ir
            .items()
            .map(|item| item.source().map(|span| (span.start(), span.end())))
            .collect(),
        parents: ir.items().map(|item| item.parent()).collect(),
        members: ir.items().map(|item| item.members().to_vec()).collect(),
    })
}

/// Exact cold authority facts from the same owned-tree transaction. Tests use
/// the public column views directly rather than a driver compatibility copy.
fn owned_authority_projection(facts: &FactSet<'_>) -> Result<Vec<EntityAuthorityFacts>, TestError> {
    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    let columns = ir.entity_authority_columns();
    if columns.parentage.len() != ir.entity_count() {
        return Err(TestError::Tail);
    }
    Ok((0..columns.row_count())
        .map(|index| EntityAuthorityFacts {
            parentage: columns.parentage[index],
            source: columns.source[index],
            source_file: columns.source_file[index],
            members: columns.members[index],
            semantic_type: columns.semantic_type[index],
            documentation: columns.documentation[index],
            visibility: columns.visibility[index],
            attributes: columns.attributes[index],
            language_extension: columns.language_extension[index],
        })
        .collect())
}

fn pending_plan() -> FactSet<'static> {
    let mut plan = super::ResourcePlan::for_source(LanguageProfile::Rust(RustEdition::Rust2024), 0);
    // Recovery falsifiers need only one declared owner, one observer, and a
    // reusable row in each pending lane; they must not reserve protocol-max
    // sidecars simply to demonstrate cursor rollback.
    plan.facts = 4;
    plan.anonymous_rows = 2;
    plan.computed_rows = 2;
    FactSet::with_plan(plan)
}

fn capacity_pending_plan() -> FactSet<'static> {
    let mut plan = super::ResourcePlan::for_source(LanguageProfile::Rust(RustEdition::Rust2024), 0);
    plan.facts = 2;
    plan.anonymous_rows = 1;
    plan.computed_rows = 1;
    FactSet::with_plan(plan)
}

fn push_pending_seed(facts: &mut FactSet<'static>) -> Result<(), TestError> {
    facts
        .push(SemanticFact::new(
            EntityKind::Alias,
            b"seed",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    Ok(())
}

fn push_anonymous_observer(facts: &mut FactSet<'static>, row: u32) -> Result<(), TestError> {
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"anonymous_observer",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
            .type_child(row, Some(b"member"), 0),
        )
        .map_err(rejected)?;
    Ok(())
}

fn recovered_pending_rows_match_control(
    candidate: &FactSet<'_>,
    control: &FactSet<'_>,
) -> Result<(), TestError> {
    if transaction_state(candidate) != transaction_state(control)
        || write(candidate)? != write(control)?
        || owned_type_projection(candidate)? != owned_type_projection(control)?
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

/// The two-fact lane under mutation: a constant product fact plus a function
/// fact whose single ordered child targets the constant fact.
fn base_facts() -> [SemanticFact<'static>; 2] {
    [
        SemanticFact::new(
            EntityKind::Constant,
            b"alpha",
            SemanticProductConstructor::PRODUCT,
        ),
        SemanticFact::new(
            EntityKind::Function,
            b"beta",
            SemanticProductConstructor::function(0, 1),
        )
        .child(ProductChildRole::FunctionResult, 0),
    ]
}

fn admit_facts(facts: [SemanticFact<'static>; 2]) -> Result<FactSet<'static>, TestError> {
    let mut set = FactSet::new();
    for fact in facts {
        set.push(fact).map_err(rejected)?;
    }
    Ok(set)
}

#[test]
fn rich_projection_reuses_exact_compound_scratch_without_placeholder_ids() -> Result<(), TestError>
{
    let mut facts = FactSet::new();
    facts
        .push(SemanticFact::new(
            EntityKind::Alias,
            b"leaf",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"tuple",
                SemanticProductConstructor::TUPLE,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
            .type_child(0, Some(b"member"), 0),
        )
        .map_err(rejected)?;
    let mut c_variadic = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    c_variadic.payload0 = SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"c_tail",
                SemanticProductConstructor::function(0, 0),
            )
            .typed(c_variadic),
        )
        .map_err(rejected)?;
    // Carrier facts give callable elements their own names and semantic
    // roles. The shared function range retains both Go-style result labels.
    for name in [
        b"argument".as_slice(),
        b"left".as_slice(),
        b"right".as_slice(),
    ] {
        facts
            .push(SemanticFact::new(
                EntityKind::Parameter,
                name,
                SemanticProductConstructor::PRODUCT,
            ))
            .map_err(rejected)?;
    }
    let mut many = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    many.payload1 = 2;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"many",
                SemanticProductConstructor::function(1, 2),
            )
            .child(ProductChildRole::FunctionParameter, 3)
            .child(ProductChildRole::FunctionResult, 4)
            .child(ProductChildRole::FunctionResult, 5)
            .typed(many)
            .type_child(3, None, 0)
            .type_child(4, None, 0)
            .type_child(5, None, 0),
        )
        .map_err(rejected)?;
    for child in [3_u32, 4, 5] {
        facts.attach_parent(child, 6).map_err(lane_fault)?;
    }
    facts
        .push(
            SemanticFact::new(
                EntityKind::Record,
                b"object",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord))
            .type_child(0, Some(b"field"), 0),
        )
        .map_err(rejected)?;
    // A zero-parameter, zero-result callable exercises the former
    // `children[0]` seed without constructing any fake type coordinate.
    facts
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"empty",
                SemanticProductConstructor::function(0, 0),
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer)),
        )
        .map_err(rejected)?;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"template",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral))
            .type_text_child(b""),
        )
        .map_err(rejected)?;
    // A single result keeps its source label in the same role-bearing result
    // list as a Go multi-result signature. Neutral rendering may choose a
    // language-specific surface later; the semantic model must never erase
    // it merely because the list has cardinality one.
    facts
        .push(SemanticFact::new(
            EntityKind::Parameter,
            b"solo",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    let mut single = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    single.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"single",
                SemanticProductConstructor::function(0, 1),
            )
            .child(ProductChildRole::FunctionResult, 10)
            .typed(single)
            .type_child(10, None, 0),
        )
        .map_err(rejected)?;
    facts.attach_parent(10, 11).map_err(lane_fault)?;

    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    assert_eq!(ir.items().len(), 12);
    assert!(ir.items().all(|item| item.semantic_type().is_some()));
    let many = ir
        .items()
        .find(|item| item.name() == b"many")
        .expect("multi-result callable was projected");
    let Some(TypeExpr::Concrete(ConcreteType::Function {
        parameters,
        results,
        variadic,
        ..
    })) = many.semantic_type().and_then(|ty| ir.ty(ty))
    else {
        panic!("multi-result callable lost its function shape");
    };
    assert_eq!(variadic, VariadicForm::None);
    assert_eq!(
        ir.tuple_elements(parameters).expect("parameter list").len(),
        1
    );
    let results = ir.tuple_elements(results).expect("result list");
    assert_eq!(results.len(), 2);
    assert_eq!(
        ir.atom(results[0].label.expect("left label")),
        Some(&b"left"[..])
    );
    assert_eq!(
        ir.atom(results[1].label.expect("right label")),
        Some(&b"right"[..])
    );
    let single = ir
        .items()
        .find(|item| item.name() == b"single")
        .expect("single-result callable was projected");
    let Some(TypeExpr::Concrete(ConcreteType::Function { results, .. })) =
        single.semantic_type().and_then(|ty| ir.ty(ty))
    else {
        panic!("single-result callable lost its function shape");
    };
    let results = ir.tuple_elements(results).expect("single result list");
    assert_eq!(results.len(), 1);
    assert_eq!(
        ir.atom(results[0].label.expect("single result label")),
        Some(&b"solo"[..])
    );
    let c_tail = ir
        .items()
        .find(|item| item.name() == b"c_tail")
        .expect("C variadic callable was projected");
    let Some(TypeExpr::Concrete(ConcreteType::Function { variadic, .. })) =
        c_tail.semantic_type().and_then(|ty| ir.ty(ty))
    else {
        panic!("C variadic callable lost its tail form");
    };
    assert_eq!(variadic, VariadicForm::CUnbounded);
    Ok(())
}

#[test]
fn nested_compounds_plan_a_depth_first_scratch_bound() -> Result<(), TestError> {
    let mut facts = FactSet::new();
    facts
        .push(SemanticFact::new(
            EntityKind::Alias,
            b"leaf",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    facts
        .anonymous_type_child(0, Some(b"inner"), 0)
        .map_err(|cause| TestError::Rejected {
            fact: facts.len(),
            name_len: 0,
            cause,
        })?;
    let inner = facts
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(|cause| TestError::Rejected {
            fact: facts.len(),
            name_len: 0,
            cause,
        })?;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"outer",
                SemanticProductConstructor::TUPLE,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
            .type_child(inner, Some(b"outer"), 0),
        )
        .map_err(rejected)?;

    // Both tuple prefixes can be live on one depth-first path. The planner
    // therefore reserves two elements rather than the old row-wise maximum
    // of one; a later projection implementation may retain either prefix
    // without reallocating or manufacturing a sentinel ID.
    assert_eq!(facts.projected_type_demand().tuple_elements, 2);
    let ir = facts.build_ir(
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity()?,
        recipe(),
        crate::types::DeclarationScope::fixture(),
    )?;
    assert!(ir.items().all(|item| item.semantic_type().is_some()));
    Ok(())
}

#[test]
fn constructor_role_and_name_mutations_change_committed_fragment_bytes() -> Result<(), TestError> {
    let base = write(&admit_facts(base_facts())?)?;
    FragmentView::validate(&base)?;

    let mut constructor_mutation = base_facts();
    constructor_mutation[0] = SemanticFact::new(
        EntityKind::Constant,
        b"alpha",
        SemanticProductConstructor::TUPLE,
    );
    let mut role_mutation = base_facts();
    role_mutation[1] = SemanticFact::new(
        EntityKind::Function,
        b"beta",
        SemanticProductConstructor::function(1, 0),
    )
    .child(ProductChildRole::FunctionParameter, 0);
    let mut name_mutation = base_facts();
    name_mutation[0] = SemanticFact::new(
        EntityKind::Constant,
        b"gamma",
        SemanticProductConstructor::PRODUCT,
    );

    for (label, mutation) in [
        ("constructor mutation", constructor_mutation),
        ("role mutation", role_mutation),
        ("name mutation", name_mutation),
    ] {
        let bytes = write(&admit_facts(mutation)?)?;
        FragmentView::validate(&bytes)?;
        if bytes == base {
            return Err(TestError::Unchanged { label });
        }
    }

    // Canonical emission is deterministic across repeated admission.
    if write(&admit_facts(base_facts())?)? != base {
        return Err(TestError::Unchanged {
            label: "repeated admission",
        });
    }
    Ok(())
}

#[test]
fn fact_admission_rejection_retains_the_exact_offending_fact_and_cause() -> Result<(), TestError> {
    let mut set = FactSet::new();
    match set.push(SemanticFact::new(
        EntityKind::Constant,
        b"",
        SemanticProductConstructor::PRODUCT,
    )) {
        Err(RejectedFact {
            fact: 0,
            name: b"",
            cause: FactFault::EmptyName,
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }

    match set.push(SemanticFact::new(
        EntityKind::Constant,
        b"target",
        SemanticProductConstructor::PRODUCT,
    )) {
        Ok(0) => {}
        Ok(_) => return Err(TestError::UnexpectedPush),
        Err(failure) => return Err(rejected(failure)),
    }

    match set.push(
        SemanticFact::new(
            EntityKind::Constant,
            b"child",
            SemanticProductConstructor::PRODUCT,
        )
        .child(ProductChildRole::ProductMember, 5),
    ) {
        Err(RejectedFact {
            fact: 1,
            name: b"child",
            cause:
                FactFault::ChildTarget {
                    position: 0,
                    target: 5,
                    fact_count: 1,
                },
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }

    // A wrong-kind claim: a function constructor that names two parameter
    // children while offering one.
    match set.push(
        SemanticFact::new(
            EntityKind::Function,
            b"arity",
            SemanticProductConstructor::function(2, 0),
        )
        .child(ProductChildRole::FunctionParameter, 0),
    ) {
        Err(RejectedFact {
            fact: 1,
            name: b"arity",
            cause:
                FactFault::Constructor(ProductConstructorFault::Arity {
                    expected: 2,
                    actual: 1,
                    ..
                }),
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }

    match set.push(
        SemanticFact::new(
            EntityKind::Function,
            b"role",
            SemanticProductConstructor::function(0, 1),
        )
        .child(ProductChildRole::ProductMember, 0),
    ) {
        Err(RejectedFact {
            fact: 1,
            name: b"role",
            cause:
                FactFault::ChildRole {
                    position: 0,
                    expected: ProductChildRole::FunctionResult,
                    actual: ProductChildRole::ProductMember,
                },
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }

    // The rejected facts never entered the lane.
    if set.len() != 1 {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn bounded_fact_and_child_lanes_reject_overflow_and_admit_the_exact_bound() -> Result<(), TestError>
{
    let mut names = [[0_u8; 4]; MAX_EMISSION_FACTS];
    for (ordinal, name) in names.iter_mut().enumerate() {
        *name = [
            b'f',
            DIGITS[(ordinal / 100) % 10],
            DIGITS[(ordinal / 10) % 10],
            DIGITS[ordinal % 10],
        ];
    }

    // The exact lane bound admits; one fact beyond it is the exact typed
    // capacity rejection retaining the would-be ordinal.
    let mut full = FactSet::new();
    for (ordinal, name) in names.iter().enumerate() {
        match full.push(SemanticFact::new(
            EntityKind::Record,
            name.as_slice(),
            SemanticProductConstructor::PRODUCT,
        )) {
            Ok(pushed) if pushed == ordinal => {}
            Ok(_) => return Err(TestError::UnexpectedPush),
            Err(failure) => return Err(rejected(failure)),
        }
    }
    match full.push(SemanticFact::new(
        EntityKind::Record,
        b"row",
        SemanticProductConstructor::PRODUCT,
    )) {
        Err(RejectedFact {
            fact: MAX_EMISSION_FACTS,
            name: b"row",
            cause: FactFault::Capacity,
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }

    // The exact maximal lane admits with every fact after the first carrying
    // the full ordered child count targeting the first fact. One fact with
    // more children than the bounded child lane is the exact typed truncation
    // rejection while the lane still has room.
    let mut maximal = FactSet::new();
    let mut overflowing_child = SemanticFact::new(
        EntityKind::Record,
        b"row",
        SemanticProductConstructor::PRODUCT,
    );
    for _ in 0..=MAX_FACT_CHILDREN {
        overflowing_child = overflowing_child.child(ProductChildRole::ProductMember, 0);
    }
    match maximal.push(overflowing_child) {
        Err(RejectedFact {
            fact: 0,
            name: b"row",
            cause: FactFault::ChildCapacity,
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    for (ordinal, name) in names.iter().enumerate() {
        let mut fact = SemanticFact::new(
            EntityKind::Record,
            name.as_slice(),
            SemanticProductConstructor::PRODUCT,
        );
        if ordinal > 0 {
            for _ in 0..MAX_FACT_CHILDREN {
                fact = fact.child(ProductChildRole::ProductMember, 0);
            }
        }
        maximal.push(fact).map_err(rejected)?;
    }

    // Freeze every companion lane at its new exact bound before admission.
    for _ in 0..MAX_ANONYMOUS_TYPE_ROWS {
        maximal
            .intern_anonymous_type_row(0, super::opaque_record())
            .map_err(|cause| {
                rejected(RejectedFact {
                    fact: 0,
                    name: b"row",
                    cause,
                })
            })?;
    }
    let occurrence = Occurrence {
        target: compiler_ir::OccurrenceTarget::Foreign(compiler_ir::ForeignKey {
            origin: compiler_ir::ForeignOrigin::Universe {
                ecosystem: "example",
            },
            path: "example.com/demo",
            display: "demo",
            kind: None,
        }),
        kind: compiler_ir::ReferenceKind::FunctionCall,
        confidence: compiler_ir::OccurrenceConfidence::Syntactic,
        span: compiler_ir::RelSpan { start: 0, end: 0 },
    };
    for _ in 0..MAX_EMISSION_OCCURRENCES {
        maximal.push_occurrence(0, occurrence).map_err(|cause| {
            rejected(RejectedFact {
                fact: 0,
                name: b"row",
                cause,
            })
        })?;
    }
    for _ in 0..MAX_EMISSION_DOC_FRAGMENTS {
        maximal
            .push_doc(0, compiler_ir::DocFragmentInput::SoftBreak)
            .map_err(|cause| {
                rejected(RejectedFact {
                    fact: 0,
                    name: b"row",
                    cause,
                })
            })?;
    }
    let extension_atoms = (0..MAX_EXTENSION_ATOMS)
        .map(|index| (index as u32).to_le_bytes())
        .collect::<Vec<_>>();
    for atom in &extension_atoms {
        maximal.intern_atom(atom).map_err(|cause| {
            rejected(RejectedFact {
                fact: 0,
                name: b"row",
                cause,
            })
        })?;
    }
    for index in 0..MAX_REF_LISTS {
        let atom_list = [
            (index % MAX_EXTENSION_ATOMS) as u32,
            (index / MAX_EXTENSION_ATOMS) as u32,
        ];
        maximal.intern_atom_list(&atom_list).map_err(|cause| {
            rejected(RejectedFact {
                fact: 0,
                name: b"row",
                cause,
            })
        })?;
        maximal.intern_type_list(&[index as u32]).map_err(|cause| {
            rejected(RejectedFact {
                fact: 0,
                name: b"row",
                cause,
            })
        })?;
        maximal
            .intern_entity_list(&[index as u32])
            .map_err(|cause| {
                rejected(RejectedFact {
                    fact: 0,
                    name: b"row",
                    cause,
                })
            })?;
    }
    for _ in 0..MAX_TYPE_PARAMETERS {
        maximal
            .push_type_parameter(b"T", None, None)
            .map_err(|cause| {
                rejected(RejectedFact {
                    fact: 0,
                    name: b"row",
                    cause,
                })
            })?;
    }

    // The maximal lane writes one complete validated fragment within the
    // conservation reservation, with its exact entity count committed.
    // The raised lane's exact eight-child product payload is larger than the
    // former 64 KiB fixture; retain the same untouched-tail proof with ample
    // caller-owned output scratch.
    let mut output = vec![0xa5_u8; 4 * 1024 * 1024];
    let length = super::admit(
        &maximal,
        identity()?,
        recipe(),
        recipe().profile,
        &mut output,
    )
    .map_err(TestError::Admission)?
    .len();
    if !output[length..].iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Tail);
    }
    let view = FragmentView::validate(&output[..length])?;
    if view.entities().count() != MAX_EMISSION_FACTS {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn empty_fact_list_writes_the_exact_current_schema_fragment_without_semantic_data()
-> Result<(), TestError> {
    let empty = FactSet::new();
    let mut output = [0xa5_u8; OUTPUT_CAPACITY];
    let length = super::admit(&empty, identity()?, recipe(), recipe().profile, &mut output)
        .map_err(TestError::Admission)?
        .len();

    // Byte-exact current-schema baseline: the empty lane is the prepared fragment
    // with no SemanticData section at all.
    let prepared = PreparedFragment::prepare(identity()?, recipe(), &[], &[], &[])?;
    let mut reference = [0_u8; OUTPUT_CAPACITY];
    let legacy = prepared.write_into(&mut reference)?;
    if &output[..length] != legacy {
        return Err(TestError::Tail);
    }
    if !output[length..].iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Tail);
    }

    // A populated lane differs from the legacy fragment and validates with
    // its semantic section.
    let populated = write(&admit_facts(base_facts())?)?;
    if populated.as_slice() == legacy {
        return Err(TestError::Tail);
    }
    FragmentView::validate(&populated)?;
    Ok(())
}

#[test]
fn admitted_generic_extensions_reopen_exact_empty_and_nonempty_ranges() -> Result<(), TestError> {
    let mut facts = FactSet::new();
    let empty_atoms = facts.intern_atom_list(&[]).map_err(lane_fault)?;
    let empty = RustFacts {
        ownership: RustOwnership::Value,
        lifetimes: empty_atoms,
        where_clauses: TypeParameterListId::new(0),
        macros: empty_atoms,
    };
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"Empty",
                SemanticProductConstructor::PRODUCT,
            )
            .with_extension(EmissionExtension::Rust(empty)),
        )
        .map_err(rejected)?;
    facts
        .push_type_parameter(b"T", None, None)
        .map_err(|cause| {
            rejected(RejectedFact {
                fact: 1,
                name: b"Nonempty",
                cause,
            })
        })?;
    facts
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"Nonempty",
                SemanticProductConstructor::PRODUCT,
            )
            .with_extension(EmissionExtension::Rust(empty)),
        )
        .map_err(rejected)?;

    let bytes = write(&facts)?;
    let view = FragmentView::validate(&bytes)?;
    let pools = view
        .discover()
        .extension_pools()
        .map_err(|_| TestError::Tail)?
        .ok_or(TestError::Tail)?;
    let extensions = view
        .discover()
        .language_extensions()
        .map_err(|_| TestError::Tail)?
        .ok_or(TestError::Tail)?;
    let first_id = extensions
        .rust
        .get(EntityId::new(0))
        .map_err(|_| TestError::Tail)?
        .ok_or(TestError::Tail)?
        .where_clauses;
    let second_id = extensions
        .rust
        .get(EntityId::new(1))
        .map_err(|_| TestError::Tail)?
        .ok_or(TestError::Tail)?
        .where_clauses;
    let first = pools
        .type_parameter_list(first_id)
        .map_err(|_| TestError::Tail)?;
    let second = pools
        .type_parameter_list(second_id)
        .map_err(|_| TestError::Tail)?;
    if let ReopenedTypeParameterList::Exact(first) = first {
        if first.length != 0 {
            return Err(TestError::Tail);
        }
    } else {
        return Err(TestError::Tail);
    }
    if let ReopenedTypeParameterList::Exact(second) = second {
        if second.length != 1 || second.get(0).map_err(|_| TestError::Tail)?.name != b"T" {
            return Err(TestError::Tail);
        }
    } else {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn rejected_generic_fact_is_byte_for_byte_transactional_before_a_valid_push()
-> Result<(), TestError> {
    let rust_extension = |parameter_start| {
        EmissionExtension::Rust(RustFacts {
            ownership: RustOwnership::Value,
            lifetimes: AtomListId::new(0),
            where_clauses: TypeParameterListId::new(parameter_start),
            macros: AtomListId::new(0),
        })
    };
    let seed = || {
        SemanticFact::new(
            EntityKind::Constant,
            b"seed",
            SemanticProductConstructor::PRODUCT,
        )
    };
    let accepted = || {
        SemanticFact::new(
            EntityKind::Alias,
            b"accepted",
            SemanticProductConstructor::PRODUCT,
        )
        .child(ProductChildRole::ProductMember, 0)
        .with_extension(rust_extension(0))
    };

    let mut candidate = FactSet::new();
    candidate.push(seed()).map_err(rejected)?;
    let before = transaction_state(&candidate);
    // This would write the kind/name/type prefixes and advance the type-child
    // cursor before discovering its invalid generic-list start in the former
    // push ordering. A legal product child proves the rejection is not merely
    // a front-loaded constructor failure.
    let invalid = SemanticFact::new(
        EntityKind::Alias,
        b"rejected",
        SemanticProductConstructor::PRODUCT,
    )
    .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
    .type_child(0, Some(b"member"), 0)
    .child(ProductChildRole::ProductMember, 0)
    .with_extension(rust_extension(1));
    match candidate.push(invalid) {
        Err(RejectedFact {
            fact: 1,
            name: b"rejected",
            cause:
                FactFault::RefTarget {
                    lane: "type_parameter_ranges",
                    raw: 1,
                    fact_count: 0,
                },
        }) => {}
        Err(failure) => return Err(rejected(failure)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    if transaction_state(&candidate) != before {
        return Err(TestError::Tail);
    }

    let mut control = FactSet::new();
    control.push(seed()).map_err(rejected)?;
    candidate.push(accepted()).map_err(rejected)?;
    control.push(accepted()).map_err(rejected)?;
    if transaction_state(&candidate) != transaction_state(&control)
        || write(&candidate)? != write(&control)?
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn parentage_transitions_are_closed_typed_and_idempotent() -> Result<(), TestError> {
    let mut facts = FactSet::new();
    for name in [b"first".as_slice(), b"second", b"child", b"root"] {
        facts
            .push(SemanticFact::new(
                EntityKind::Constant,
                name,
                SemanticProductConstructor::PRODUCT,
            ))
            .map_err(rejected)?;
    }

    facts
        .attach_parent(2, 0)
        .map_err(|cause| TestError::Rejected {
            fact: 2,
            name_len: b"child".len(),
            cause,
        })?;
    // A repeated authority observation is the sole legal no-op transition.
    facts
        .attach_parent(2, 0)
        .map_err(|cause| TestError::Rejected {
            fact: 2,
            name_len: b"child".len(),
            cause,
        })?;
    match facts.attach_parent(2, 1) {
        Err(FactFault::ConflictingParentage {
            entity,
            existing: ParentageState::Bound { parent },
            requested:
                ParentageState::Bound {
                    parent: requested_parent,
                },
        }) if entity == EntityId::new(2)
            && parent == EntityId::new(0)
            && requested_parent == EntityId::new(1) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    match facts.mark_parentage_root(2) {
        Err(FactFault::ConflictingParentage {
            entity,
            existing: ParentageState::Bound { parent },
            requested: ParentageState::Root,
        }) if entity == EntityId::new(2) && parent == EntityId::new(0) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }

    facts
        .mark_parentage_root(3)
        .map_err(|cause| TestError::Rejected {
            fact: 3,
            name_len: b"root".len(),
            cause,
        })?;
    facts
        .mark_parentage_root(3)
        .map_err(|cause| TestError::Rejected {
            fact: 3,
            name_len: b"root".len(),
            cause,
        })?;
    match facts.attach_parent(3, 0) {
        Err(FactFault::ConflictingParentage {
            entity,
            existing: ParentageState::Root,
            requested: ParentageState::Bound { parent },
        }) if entity == EntityId::new(3) && parent == EntityId::new(0) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    let authority_identity = [0x5a; 16];
    facts
        .mark_unrepresented_parent(1, authority_identity)
        .map_err(|cause| TestError::Rejected {
            fact: 1,
            name_len: b"second".len(),
            cause,
        })?;
    match facts.mark_parentage_root(1) {
        Err(FactFault::ConflictingParentage {
            entity,
            existing: ParentageState::UnrepresentedAuthorityOwner { identity },
            requested: ParentageState::Root,
        }) if entity == EntityId::new(1) && identity == authority_identity => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    Ok(())
}

#[test]
fn provenance_is_exactly_transactional_and_members_require_complete_capture()
-> Result<(), TestError> {
    let mut candidate = FactSet::new();
    let mut control = FactSet::new();
    for name in [b"parent".as_slice(), b"child", b"empty"] {
        let fact = SemanticFact::new(
            EntityKind::Constant,
            name,
            SemanticProductConstructor::PRODUCT,
        );
        candidate.push(fact).map_err(rejected)?;
        control.push(fact).map_err(rejected)?;
    }
    let Some(first_span) = SourceSpanFact::new(1, 3) else {
        return Err(TestError::Tail);
    };
    let Some(conflicting_span) = SourceSpanFact::new(2, 4) else {
        return Err(TestError::Tail);
    };

    candidate
        .attach_source_span(0, first_span)
        .map_err(lane_fault)?;
    // The only legal repeated source observation is byte-for-byte identical.
    candidate
        .attach_source_span(0, first_span)
        .map_err(lane_fault)?;
    candidate.mark_parentage_root(0).map_err(lane_fault)?;
    candidate.mark_parentage_root(0).map_err(lane_fault)?;
    candidate.attach_parent(1, 0).map_err(lane_fault)?;
    candidate.attach_parent(1, 0).map_err(lane_fault)?;
    candidate.mark_parentage_root(2).map_err(lane_fault)?;

    control
        .attach_source_span(0, first_span)
        .map_err(lane_fault)?;
    control.mark_parentage_root(0).map_err(lane_fault)?;
    control.attach_parent(1, 0).map_err(lane_fault)?;
    control.mark_parentage_root(2).map_err(lane_fault)?;

    match candidate.attach_source_span(0, conflicting_span) {
        Err(FactFault::ConflictingSourceSpan {
            entity,
            existing,
            requested,
        }) if entity == EntityId::new(0)
            && existing == first_span
            && requested == conflicting_span => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    // Neither root nor a bound relation declares that all local members were
    // enumerated. The root's actual child makes that distinction observable.
    let authority = owned_authority_projection(&candidate)?;
    if authority.get(0).map(|row| row.members) != Some(FactAvailability::Unavailable)
        || authority.get(1).map(|row| row.members) != Some(FactAvailability::Unavailable)
        || authority.get(2).map(|row| row.members) != Some(FactAvailability::Unavailable)
    {
        return Err(TestError::Tail);
    }

    // This root has no local children; an explicit authority observation of
    // that empty set changes availability, and repeating it is harmless.
    candidate.mark_members_captured(2).map_err(lane_fault)?;
    candidate.mark_members_captured(2).map_err(lane_fault)?;
    control.mark_members_captured(2).map_err(lane_fault)?;
    if owned_authority_projection(&candidate)?
        .get(2)
        .map(|row| row.members)
        != Some(FactAvailability::Captured)
    {
        return Err(TestError::Tail);
    }

    // Each invalid coordinate retains its lane and raw operand exactly and
    // leaves all active provenance prefixes equal to the control lane.
    match candidate.attach_source_span(3, first_span) {
        Err(FactFault::RefTarget {
            lane: "entity_source_spans",
            raw: 3,
            fact_count: 3,
        }) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    match candidate.attach_parent(1, 3) {
        Err(FactFault::RefTarget {
            lane: "entity_parents",
            raw: 3,
            fact_count: 3,
        }) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    match candidate.mark_members_captured(3) {
        Err(FactFault::RefTarget {
            lane: "entity_members",
            raw: 3,
            fact_count: 3,
        }) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }

    let expected = OwnedTopologyProjection {
        sources: vec![Some((1, 3)), None, None],
        parents: vec![None, Some(EntityId::new(0)), None],
        members: vec![vec![], vec![], vec![]],
    };
    if transaction_state(&candidate) != transaction_state(&control)
        || write(&candidate)? != write(&control)?
        || owned_topology_projection(&candidate)? != expected
        || owned_topology_projection(&candidate)? != owned_topology_projection(&control)?
    {
        return Err(TestError::Tail);
    }

    let mut plan = super::ResourcePlan::for_source(LanguageProfile::Rust(RustEdition::Rust2024), 0);
    plan.facts = 1;
    let mut bounded = FactSet::with_primary_source(plan, 3);
    bounded
        .push(SemanticFact::new(
            EntityKind::Constant,
            b"bounded",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    let Some(escaped) = SourceSpanFact::new(1, 4) else {
        return Err(TestError::Tail);
    };
    match bounded.attach_source_span(0, escaped) {
        Err(FactFault::SourceSpan {
            entity: 0,
            start: 1,
            end: 4,
            source_len: 3,
        }) => {}
        Err(_) => return Err(TestError::Tail),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    Ok(())
}

#[test]
fn authority_facts_mark_empty_documentation_and_private_visibility_when_supplied()
-> Result<(), TestError> {
    let mut facts = FactSet::new();
    facts
        .push(
            SemanticFact::new(
                EntityKind::Constant,
                b"documented",
                SemanticProductConstructor::PRODUCT,
            )
            .with_visibility(Visibility::Private),
        )
        .map_err(rejected)?;
    facts
        .mark_documentation_captured(0)
        .map_err(|cause| TestError::Rejected {
            fact: 0,
            name_len: b"documented".len(),
            cause,
        })?;
    facts
        .push(SemanticFact::new(
            EntityKind::Constant,
            b"unavailable",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;

    let authority = owned_authority_projection(&facts)?;
    let Some(documented) = authority.first() else {
        return Err(TestError::Tail);
    };
    let Some(unavailable) = authority.get(1) else {
        return Err(TestError::Tail);
    };
    if documented.documentation != FactAvailability::Captured
        || documented.visibility != FactAvailability::Captured
        || unavailable.documentation != FactAvailability::Unavailable
        || unavailable.visibility != FactAvailability::Unavailable
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn anonymous_pending_rows_abort_each_failure_before_the_next_valid_row() -> Result<(), TestError> {
    // A failed append abandons the preceding valid child as well as the
    // invalid request, so the next row begins at a fresh reusable prefix.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .anonymous_type_child(0, Some(b"discarded"), 0)
        .map_err(lane_fault)?;
    match candidate.anonymous_type_child(u32::MAX, None, 0) {
        Err(FactFault::TypeChildTarget {
            position: 1,
            target: u32::MAX,
            fact_count: 1,
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .anonymous_type_child(0, Some(b"kept"), 0)
        .map_err(lane_fault)?;
    control
        .anonymous_type_child(0, Some(b"kept"), 0)
        .map_err(lane_fault)?;
    let candidate_row = candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    let control_row = control
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    if candidate_row != control_row {
        return Err(TestError::Tail);
    }
    push_anonymous_observer(&mut candidate, candidate_row)?;
    push_anonymous_observer(&mut control, control_row)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // A record-level structural fault similarly abandons its entire pending
    // run before the next tuple row is admitted.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    match candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::SelfType))
    {
        Err(FactFault::TypeRecord(SemanticTypeFault::ChildCount {
            tag: SemanticTypeTag::SelfType,
            actual: 1,
            ..
        })) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    control
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    let candidate_row = candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    let control_row = control
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    push_anonymous_observer(&mut candidate, candidate_row)?;
    push_anonymous_observer(&mut control, control_row)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // The row-wide child grammar is independently fallible. Its exact flag
    // fault survives the cleanup rather than being replaced by a reset error.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .anonymous_type_child(0, None, SemanticTypeChild::FLAG_READONLY)
        .map_err(lane_fault)?;
    match candidate.intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)) {
        Err(FactFault::TypeChild {
            position: 0,
            fault:
                SemanticTypeFault::ChildFlagsForbidden {
                    tag: SemanticTypeTag::Tuple,
                    position: 0,
                    actual: SemanticTypeChild::FLAG_READONLY,
                },
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    control
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    let candidate_row = candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    let control_row = control
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    push_anonymous_observer(&mut candidate, candidate_row)?;
    push_anonymous_observer(&mut control, control_row)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // Both ordinary and reserved-anchor owner failures use the same abort
    // transition. The latter then successfully binds the immediately-next
    // declared fact as its reserved owner.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    match candidate.intern_anonymous_type_row(1, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)) {
        Err(FactFault::RefTarget {
            lane: "type_rows",
            raw: 1,
            fact_count: 1,
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    control
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    let candidate_row = candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    let control_row = control
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    push_anonymous_observer(&mut candidate, candidate_row)?;
    push_anonymous_observer(&mut control, control_row)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    match candidate
        .intern_reserved_anchor_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
    {
        Err(FactFault::RefTarget {
            lane: "reserved_type_rows",
            raw: 0,
            fact_count: 1,
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    control
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    let candidate_row = candidate
        .intern_reserved_anchor_type_row(1, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    let control_row = control
        .intern_reserved_anchor_type_row(1, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    push_anonymous_observer(&mut candidate, candidate_row)?;
    push_anonymous_observer(&mut control, control_row)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // A full row lane cannot admit another replacement row, so this control
    // retains the prior committed prefix and proves the failed pending child
    // did not contaminate it.
    let mut candidate = capacity_pending_plan();
    let mut control = capacity_pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    control
        .intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
        .map_err(lane_fault)?;
    candidate
        .anonymous_type_child(0, None, 0)
        .map_err(lane_fault)?;
    match candidate.intern_anonymous_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)) {
        Err(FactFault::TypeRowCapacity) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    recovered_pending_rows_match_control(&candidate, &control)
}

#[test]
fn computed_pending_rows_abort_each_failure_before_the_next_valid_row() -> Result<(), TestError> {
    // The text sentinel is a first-class computed child. An invalid sibling
    // must discard it, then the next template row commits only its new text.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .computed_type_text_child(b"discarded")
        .map_err(lane_fault)?;
    match candidate.computed_type_child(super::COMPUTED_ROW_BASE, None, 0) {
        Err(FactFault::TypeChildTarget {
            position: 1,
            target: super::COMPUTED_ROW_BASE,
            fact_count: 1,
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(()) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    control
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    candidate
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    control
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // Record validation happens after a valid text append, and must roll it
    // back before the following template row is admitted.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .computed_type_text_child(b"discarded")
        .map_err(lane_fault)?;
    match candidate.intern_computed_type_row(0, SemanticTypeRecord::leaf(SemanticTypeTag::SelfType))
    {
        Err(FactFault::TypeRecord(SemanticTypeFault::ChildCount {
            tag: SemanticTypeTag::SelfType,
            actual: 1,
            ..
        })) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    control
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    candidate
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    control
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // Per-child flag validation also preserves its typed operands through
    // cleanup; the replacement row has one ordinary text segment.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .computed_type_child(u32::MAX, Some(b"discarded"), SemanticTypeChild::FLAG_REST)
        .map_err(lane_fault)?;
    match candidate.intern_computed_type_row(
        0,
        SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
    ) {
        Err(FactFault::TypeChild {
            position: 0,
            fault:
                SemanticTypeFault::ChildFlagsForbidden {
                    tag: SemanticTypeTag::TemplateLiteral,
                    position: 0,
                    actual: SemanticTypeChild::FLAG_REST,
                },
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    control
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    candidate
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    control
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // An invalid owner is checked before record admission but still abandons
    // the complete current text run and preserves the owner fault verbatim.
    let mut candidate = pending_plan();
    let mut control = pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .computed_type_text_child(b"discarded")
        .map_err(lane_fault)?;
    match candidate.intern_computed_type_row(
        1,
        SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
    ) {
        Err(FactFault::RefTarget {
            lane: "computed_owners",
            raw: 1,
            fact_count: 1,
        }) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    candidate
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    control
        .computed_type_text_child(b"kept")
        .map_err(lane_fault)?;
    candidate
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    control
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    recovered_pending_rows_match_control(&candidate, &control)?;

    // As with anonymous rows, capacity has no next in-lane row to admit; a
    // separately built control proves the preexisting computed prefix and
    // all cursors are untouched after the failed pending template child.
    let mut candidate = capacity_pending_plan();
    let mut control = capacity_pending_plan();
    push_pending_seed(&mut candidate)?;
    push_pending_seed(&mut control)?;
    candidate
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    control
        .intern_computed_type_row(
            0,
            SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
        )
        .map_err(lane_fault)?;
    candidate
        .computed_type_text_child(b"discarded")
        .map_err(lane_fault)?;
    match candidate.intern_computed_type_row(
        0,
        SemanticTypeRecord::leaf(SemanticTypeTag::TemplateLiteral),
    ) {
        Err(FactFault::ComputedRowCapacity) => {}
        Err(cause) => return Err(lane_fault(cause)),
        Ok(_) => return Err(TestError::UnexpectedPush),
    }
    recovered_pending_rows_match_control(&candidate, &control)
}

#[test]
fn unique_type_edit_changes_payload_without_reminting_stable_identity() -> Result<(), TestError> {
    let mut tuple = FactSet::new();
    tuple
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"Edited",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
        )
        .map_err(rejected)?;
    let mut record = FactSet::new();
    record
        .push(
            SemanticFact::new(
                EntityKind::Alias,
                b"Edited",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord)),
        )
        .map_err(rejected)?;
    let tuple = entity_versions(&tuple)?;
    let record = entity_versions(&record)?;
    if tuple[0].family != record[0].family || tuple[0].core_payload == record[0].core_payload {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn attaching_parentage_does_not_fabricate_parent_product_members() -> Result<(), TestError> {
    let mut empty = FactSet::new();
    empty
        .push(SemanticFact::new(
            EntityKind::Record,
            b"Owner",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    empty.mark_parentage_root(0).map_err(lane_fault)?;

    let mut member = FactSet::new();
    member
        .push(SemanticFact::new(
            EntityKind::Record,
            b"Owner",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    member
        .push(SemanticFact::new(
            EntityKind::Field,
            b"field",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    member.mark_parentage_root(0).map_err(lane_fault)?;
    member.attach_parent(1, 0).map_err(lane_fault)?;

    let empty = entity_versions(&empty)?;
    let member = entity_versions(&member)?;
    if empty[0].identity() != member[0].identity()
        || empty[0].core_payload != member[0].core_payload
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn bound_child_family_tracks_parent_variant_edits() -> Result<(), TestError> {
    let mut tuple_parent = FactSet::new();
    tuple_parent
        .push(
            SemanticFact::new(
                EntityKind::Record,
                b"Owner",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
        )
        .map_err(rejected)?;
    tuple_parent
        .push(SemanticFact::new(
            EntityKind::Field,
            b"field",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    tuple_parent.mark_parentage_root(0).map_err(lane_fault)?;
    tuple_parent.attach_parent(1, 0).map_err(lane_fault)?;

    let mut record_parent = FactSet::new();
    record_parent
        .push(
            SemanticFact::new(
                EntityKind::Record,
                b"Owner",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord)),
        )
        .map_err(rejected)?;
    record_parent
        .push(SemanticFact::new(
            EntityKind::Field,
            b"field",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    record_parent.mark_parentage_root(0).map_err(lane_fault)?;
    record_parent.attach_parent(1, 0).map_err(lane_fault)?;

    let tuple_parent = entity_versions(&tuple_parent)?;
    let record_parent = entity_versions(&record_parent)?;
    if tuple_parent[0].family != record_parent[0].family
        || tuple_parent[0].core_payload == record_parent[0].core_payload
        || tuple_parent[1].family == record_parent[1].family
        || tuple_parent[1].variant != record_parent[1].variant
        || tuple_parent[1].core_payload != record_parent[1].core_payload
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn overload_signatures_are_distinct_reorder_stable_and_nested_safe() -> Result<(), TestError> {
    let mut first = FactSet::new();
    first
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
        )
        .map_err(rejected)?;
    first
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord)),
        )
        .map_err(rejected)?;
    let mut reversed = FactSet::new();
    reversed
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord)),
        )
        .map_err(rejected)?;
    reversed
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
        )
        .map_err(rejected)?;
    let first_versions = entity_versions(&first)?;
    let reversed_versions = entity_versions(&reversed)?;
    if first_versions[0].identity() == first_versions[1].identity()
        || first_versions[0].identity() != reversed_versions[1].identity()
        || first_versions[1].identity() != reversed_versions[0].identity()
    {
        return Err(TestError::Tail);
    }

    // Bound overload siblings exercise the compact closed-parentage frame.
    // The central family writer preflights its exact parent identity cells,
    // so both children stay distinct without a coordinate input.
    let mut nested = FactSet::new();
    nested
        .push(SemanticFact::new(
            EntityKind::Record,
            b"Container",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    nested
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
        )
        .map_err(rejected)?;
    nested
        .push(
            SemanticFact::new(
                EntityKind::Function,
                b"overload",
                SemanticProductConstructor::PRODUCT,
            )
            .typed(SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord)),
        )
        .map_err(rejected)?;
    nested.mark_parentage_root(0).map_err(lane_fault)?;
    nested.attach_parent(1, 0).map_err(lane_fault)?;
    nested.attach_parent(2, 0).map_err(lane_fault)?;
    let nested_versions = entity_versions(&nested)?;
    if nested_versions[1].identity() == nested_versions[2].identity() {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn recursive_overload_signatures_distinguish_nested_and_nominal_descendants()
-> Result<(), TestError> {
    fn push_overload(
        facts: &mut FactSet<'static>,
        inner_tag: SemanticTypeTag,
    ) -> Result<u32, TestError> {
        let owner = u32::try_from(facts.len()).map_err(|_| TestError::Tail)?;
        let inner = facts
            .intern_reserved_anchor_type_row(owner, SemanticTypeRecord::leaf(inner_tag))
            .map_err(lane_fault)?;
        facts
            .anonymous_type_child(inner, None, 0)
            .map_err(lane_fault)?;
        let outer = facts
            .intern_reserved_anchor_type_row(
                owner,
                SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
            )
            .map_err(lane_fault)?;
        facts
            .push(
                SemanticFact::new(
                    EntityKind::Function,
                    b"overload",
                    SemanticProductConstructor::PRODUCT,
                )
                .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple))
                .type_child(outer, Some(b"result"), 0),
            )
            .map_err(rejected)?;
        Ok(owner)
    }

    let mut facts = FactSet::new();
    let first_parent = push_overload(&mut facts, SemanticTypeTag::Tuple)?;
    let second_parent = push_overload(&mut facts, SemanticTypeTag::AnonymousRecord)?;
    if (first_parent, second_parent) != (0, 1) {
        return Err(TestError::Tail);
    }
    for parent in [0_u32, 1] {
        facts
            .push(SemanticFact::new(
                EntityKind::Alias,
                b"Nested",
                SemanticProductConstructor::PRODUCT,
            ))
            .map_err(rejected)?;
        let child = u32::try_from(facts.len() - 1).map_err(|_| TestError::Tail)?;
        facts.attach_parent(child, parent).map_err(lane_fault)?;
    }
    for target in [2_u32, 3] {
        let mut nominal = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        nominal.nominal = Some(NominalRef::Local(EntityId::new(target)));
        facts
            .push(
                SemanticFact::new(
                    EntityKind::Function,
                    b"use_nested",
                    SemanticProductConstructor::PRODUCT,
                )
                .typed(nominal),
            )
            .map_err(rejected)?;
    }
    for entity in [0_u32, 1, 4, 5] {
        facts.mark_parentage_root(entity).map_err(lane_fault)?;
    }

    let versions = entity_versions(&facts)?;
    if versions[0].variant == versions[1].variant
        || versions[2].identity() == versions[3].identity()
        || versions[4].variant == versions[5].variant
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn recursive_signature_components_are_reorder_stable_without_row_tokens() -> Result<(), TestError> {
    fn versions(reversed: bool) -> Result<Vec<EntityVersion>, TestError> {
        let names = if reversed {
            [b"right".as_slice(), b"left"]
        } else {
            [b"left".as_slice(), b"right"]
        };
        let mut facts = FactSet::new();
        for name in names {
            facts
                .push(
                    SemanticFact::new(EntityKind::Alias, name, SemanticProductConstructor::PRODUCT)
                        .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
                )
                .map_err(rejected)?;
        }
        // A hostile admitted-image mutant: producer admission disallows
        // forward local references, while the materializer still must hash a
        // pre-existing cyclic image without a recursive call or row token.
        facts.type_records[0] = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        facts.type_records[0].nominal = Some(NominalRef::Local(EntityId::new(1)));
        facts.type_records[1] = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        facts.type_records[1].nominal = Some(NominalRef::Local(EntityId::new(0)));
        facts.mark_parentage_root(0).map_err(lane_fault)?;
        facts.mark_parentage_root(1).map_err(lane_fault)?;
        entity_versions(&facts)
    }

    let forward = versions(false)?;
    let reversed = versions(true)?;
    if forward[0].identity() != reversed[1].identity()
        || forward[1].identity() != reversed[0].identity()
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn recursive_signature_edges_retain_labelled_scc_topology() -> Result<(), TestError> {
    fn versions(edges: [u32; 3]) -> Result<Vec<EntityVersion>, TestError> {
        let mut facts = FactSet::new();
        for name in [b"alpha".as_slice(), b"beta", b"gamma"] {
            facts
                .push(
                    SemanticFact::new(EntityKind::Alias, name, SemanticProductConstructor::PRODUCT)
                        .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
                )
                .map_err(rejected)?;
        }
        // Both inputs have the same three direct row headers and form one
        // three-node SCC. Only their labelled local-reference topology
        // differs, so a component-wide marker without a target header would
        // incorrectly make the alpha variant identical.
        for (ordinal, target) in edges.into_iter().enumerate() {
            facts.type_records[ordinal] = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            facts.type_records[ordinal].nominal = Some(NominalRef::Local(EntityId::new(target)));
            facts
                .mark_parentage_root(u32::try_from(ordinal).map_err(|_| TestError::Tail)?)
                .map_err(lane_fault)?;
        }
        entity_versions(&facts)
    }

    let clockwise = versions([1, 2, 0])?;
    let counter_clockwise = versions([2, 0, 1])?;
    if clockwise[0].variant == counter_clockwise[0].variant {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn unrelated_insertion_does_not_remint_unique_siblings() -> Result<(), TestError> {
    let mut base = FactSet::new();
    for name in [b"alpha".as_slice(), b"beta"] {
        base.push(SemanticFact::new(
            EntityKind::Constant,
            name,
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    }
    let mut inserted = FactSet::new();
    for name in [b"unrelated".as_slice(), b"alpha", b"beta"] {
        inserted
            .push(SemanticFact::new(
                EntityKind::Constant,
                name,
                SemanticProductConstructor::PRODUCT,
            ))
            .map_err(rejected)?;
    }
    let base = entity_versions(&base)?;
    let inserted = entity_versions(&inserted)?;
    if base[0].identity() != inserted[1].identity() || base[1].identity() != inserted[2].identity()
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

#[test]
fn identical_sibling_collision_and_root_unavailable_scope_remain_exact() -> Result<(), TestError> {
    let mut identical = FactSet::new();
    for _ in 0..2 {
        identical
            .push(
                SemanticFact::new(
                    EntityKind::Function,
                    b"same",
                    SemanticProductConstructor::PRODUCT,
                )
                .typed(SemanticTypeRecord::leaf(SemanticTypeTag::Tuple)),
            )
            .map_err(rejected)?;
    }
    match entity_versions(&identical) {
        Err(TestError::Build(BuildError::DuplicateDeclarationIdentity { .. })) => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(TestError::Tail),
    }

    let mut root = FactSet::new();
    root.push(SemanticFact::new(
        EntityKind::Module,
        b"Scoped",
        SemanticProductConstructor::PRODUCT,
    ))
    .map_err(rejected)?;
    root.mark_parentage_root(0).map_err(lane_fault)?;
    let mut unavailable = FactSet::new();
    unavailable
        .push(SemanticFact::new(
            EntityKind::Module,
            b"Scoped",
            SemanticProductConstructor::PRODUCT,
        ))
        .map_err(rejected)?;
    let root_version = entity_versions(&root)?;
    let unavailable_version = entity_versions(&unavailable)?;
    if root_version[0].family == unavailable_version[0].family
        || owned_authority_projection(&root)?[0].parentage != ParentageAuthority::Root
        || owned_authority_projection(&unavailable)?[0].parentage != ParentageAuthority::Unavailable
    {
        return Err(TestError::Tail);
    }
    Ok(())
}

const DIGITS: [u8; 10] = *b"0123456789";
