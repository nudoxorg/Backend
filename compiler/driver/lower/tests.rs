//! Red falsifiers for the multi-declaration semantic emission lane: every
//! lowered constructor, role, and name must change the committed fragment
//! bytes, every fact admission rejection must retain the exact offending fact
//! and cause, and an empty fact set must remain schema-1 compatible.
use compiler_ir::{
    EntityKind, FragmentView, PrepareError, PreparedFragment, PrimitiveType, SourceIdentity,
};
use compiler_ir::{ProductChildRole, ProductConstructorFault, SemanticProductConstructor};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

use super::{
    AdmissionFault, FactFault, FactSet, FactType, MAX_EMISSION_FACTS, MAX_FACT_CHILDREN,
    RejectedFact, SemanticFact,
};

const SOURCE_BYTES: &[u8] = b"emission-seam-source";
const OUTPUT_CAPACITY: usize = 512;

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

/// Writes one admitted lane and returns the exact committed fragment bytes,
/// proving the untouched output tail stayed unchanged.
fn write(facts: &FactSet<'_>) -> Result<Vec<u8>, TestError> {
    let mut output = vec![0xa5_u8; OUTPUT_CAPACITY];
    let length = super::admit(facts, identity()?, recipe(), &mut output)
        .map_err(TestError::Admission)?
        .len();
    if !output[length..].iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Tail);
    }
    output.truncate(length);
    Ok(output)
}

/// The two-fact lane under mutation: a constant product fact plus a function
/// fact whose single ordered child targets the constant fact.
fn base_facts() -> [SemanticFact<'static>; 2] {
    [
        SemanticFact::new(
            EntityKind::Constant,
            b"alpha",
            FactType::Primitive(PrimitiveType::Bool),
            SemanticProductConstructor::PRODUCT,
        ),
        SemanticFact::new(
            EntityKind::Function,
            b"beta",
            FactType::Opaque,
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
fn constructor_role_and_name_mutations_change_committed_fragment_bytes() -> Result<(), TestError> {
    let base = write(&admit_facts(base_facts())?)?;
    FragmentView::validate(&base)?;

    let mut constructor_mutation = base_facts();
    constructor_mutation[0] = SemanticFact::new(
        EntityKind::Constant,
        b"alpha",
        FactType::Primitive(PrimitiveType::Bool),
        SemanticProductConstructor::TUPLE,
    );
    let mut role_mutation = base_facts();
    role_mutation[1] = SemanticFact::new(
        EntityKind::Function,
        b"beta",
        FactType::Opaque,
        SemanticProductConstructor::function(1, 0),
    )
    .child(ProductChildRole::FunctionParameter, 0);
    let mut name_mutation = base_facts();
    name_mutation[0] = SemanticFact::new(
        EntityKind::Constant,
        b"gamma",
        FactType::Primitive(PrimitiveType::Bool),
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
        FactType::Primitive(PrimitiveType::Bool),
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
        FactType::Opaque,
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
            FactType::Opaque,
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
            FactType::Opaque,
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
            FactType::Opaque,
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
            FactType::Opaque,
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
        FactType::Opaque,
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
        FactType::Opaque,
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
            FactType::Opaque,
            SemanticProductConstructor::PRODUCT,
        );
        if ordinal > 0 {
            for _ in 0..MAX_FACT_CHILDREN {
                fact = fact.child(ProductChildRole::ProductMember, 0);
            }
        }
        maximal.push(fact).map_err(rejected)?;
    }

    // The maximal lane writes one complete validated fragment within the
    // conservation reservation, with its exact entity count committed.
    let mut output = vec![0xa5_u8; 65_536];
    let length = super::admit(&maximal, identity()?, recipe(), &mut output)
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
fn empty_fact_list_writes_the_exact_schema1_fragment_without_semantic_data() -> Result<(), TestError>
{
    let empty = FactSet::new();
    let mut output = [0xa5_u8; OUTPUT_CAPACITY];
    let length = super::admit(&empty, identity()?, recipe(), &mut output)
        .map_err(TestError::Admission)?
        .len();

    // Byte-exact schema-1 compatibility: the empty lane is the legacy
    // prepared fragment with no SemanticData section at all.
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

const DIGITS: [u8; 10] = *b"0123456789";
