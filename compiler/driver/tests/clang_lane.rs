//! Type rows are an emission-ordered log: pooled anonymous rows and fact rows
//! interleave, so these assertions locate rows by their decoded content.

use compiler_driver::{
    compile, CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection,
};
use compiler_ir::{EntityKind, FragmentView, NominalRef, PrimitiveShape, SemanticTypeTag};
use compiler_vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::{ContentId, ToolchainDomain};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error("clang compile failed")]
    Compile,
    #[error("{0}")]
    Check(&'static str),
}

fn lower<'output>(
    source: &[u8],
    output: &'output mut [u8],
    native_work: &Path,
) -> Result<FragmentView<'output>, TestError> {
    output.fill(0xa5);
    let mut diagnostic_output = [0_u8; 4096];
    let cancelled = AtomicBool::new(false);
    let toolchain = ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_identity(
            NativeTool::Clang,
            Path::new("/usr/bin/clang"),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-semantic-lane-toolchain"),
        )
        .map_err(|_| TestError::Check("toolchain path was rejected"))?,
    );
    let compiled = compile(
        CompileRequest {
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
            source,
            toolchain,
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic_output,
            native_work,
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|_| TestError::Compile)?;
    let length = compiled.fragment.as_ref().len();
    drop(compiled);
    if output[length..].iter().any(|byte| *byte != 0xa5) {
        return Err(TestError::Check("output tail changed"));
    }
    FragmentView::validate(&output[..length]).map_err(|_| TestError::Check("fragment validation"))
}

fn word(bytes: &[u8], at: usize) -> Result<u32, TestError> {
    let end = at
        .checked_add(4)
        .ok_or(TestError::Check("offset overflow"))?;
    bytes
        .get(at..end)
        .and_then(|cell| <[u8; 4]>::try_from(cell).ok())
        .map(u32::from_le_bytes)
        .ok_or(TestError::Check("truncated type payload"))
}

fn skip_name(bytes: &[u8], at: &mut usize) -> Result<(), TestError> {
    match bytes.get(*at).copied() {
        Some(0) => *at += 1,
        Some(1) => {
            let length = usize::try_from(word(bytes, *at + 1)?)
                .map_err(|_| TestError::Check("name length overflow"))?;
            *at = at
                .checked_add(5 + length)
                .ok_or(TestError::Check("offset overflow"))?;
        }
        _ => return Err(TestError::Check("invalid name cell")),
    }
    Ok(())
}

fn child_target(view: &FragmentView<'_>) -> Result<u32, TestError> {
    let payload = view
        .type_fact_payload()
        .ok_or(TestError::Check("missing type facts"))?;
    let count =
        usize::try_from(word(payload, 0)?).map_err(|_| TestError::Check("row count overflow"))?;
    let mut at = 4_usize;
    for _ in 0..count {
        at = at
            .checked_add(13)
            .ok_or(TestError::Check("offset overflow"))?;
        skip_name(payload, &mut at)?;
        skip_name(payload, &mut at)?;
        match payload.get(at).copied() {
            Some(0) => at += 1,
            Some(1) => at += 5,
            Some(2) => at += 21,
            _ => return Err(TestError::Check("invalid nominal cell")),
        }
        at = at
            .checked_add(8)
            .ok_or(TestError::Check("offset overflow"))?;
    }
    if word(payload, at)? == 0 {
        return Err(TestError::Check("missing child"));
    }
    at += 4;
    if payload.get(at).copied() != Some(0) {
        return Err(TestError::Check("child is not local"));
    }
    word(payload, at + 1)
}

#[test]
fn recursive_pointer_rows_are_content_addressed_and_mutation_changes_shape() -> Result<(), TestError>
{
    let work = std::env::temp_dir().join(format!("nudox-clang-lane-{}", std::process::id()));
    std::fs::create_dir(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut output = vec![0xa5_u8; 65_536];
    let committed = {
        let view = lower(b"struct Node { struct Node *next; };", &mut output, &work)?;
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        let mut entities = Vec::new();
        for entity in view.entities() {
            let ordinal = usize::try_from(entity.name.raw)
                .map_err(|_| TestError::Check("atom coordinate overflow"))?;
            let name = atoms
                .get(ordinal)
                .copied()
                .ok_or(TestError::Check("entity atom absent"))?;
            entities.push((name, entity.kind));
        }
        if entities
            != [
                (&b"Node"[..], EntityKind::Record),
                (&b"next"[..], EntityKind::Field),
            ]
        {
            return Err(TestError::Check("entity content or order"));
        }
        let rows: Vec<_> = view
            .type_facts()
            .ok_or(TestError::Check("missing type facts"))?
            .collect::<Result<_, _>>()
            .map_err(|_| TestError::Check("type row decode"))?;
        if rows.len() < 2 {
            return Err(TestError::Check("type row count"));
        }
        let pointers: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.owner.raw == 1
                    && row.record.tag == SemanticTypeTag::Primitive
                    && row.record.payload0 == u32::from(PrimitiveShape::MutPointer)
                    && row.record.children.length == 1
            })
            .collect();
        if pointers.len() != 1 {
            return Err(TestError::Check("mutable pointer row count"));
        }
        let target = child_target(&view)?;
        let anchor = rows
            .get(
                usize::try_from(target)
                    .map_err(|_| TestError::Check("child coordinate overflow"))?,
            )
            .ok_or(TestError::Check("anchor row absent"))?;
        if target != 0
            || anchor.owner.raw != 0
            || anchor.record.tag != SemanticTypeTag::Nominal
            || anchor.record.nominal != Some(NominalRef::Local(compiler_ir::EntityId::new(0)))
        {
            return Err(TestError::Check("recursive row content"));
        }
        view.as_ref().to_vec()
    };
    output.fill(0xa5);
    let mutated = {
        let view = lower(
            b"struct Node { const struct Node *next; };",
            &mut output,
            &work,
        )?;
        let rows: Vec<_> = view
            .type_facts()
            .ok_or(TestError::Check("missing mutated facts"))?
            .collect::<Result<_, _>>()
            .map_err(|_| TestError::Check("mutated row decode"))?;
        if !rows.iter().any(|row| {
            row.owner.raw == 1
                && row.record.tag == SemanticTypeTag::Primitive
                && row.record.payload0 == u32::from(PrimitiveShape::ConstPointer)
        }) {
            return Err(TestError::Check("const pointer falsifier"));
        }
        view.as_ref().to_vec()
    };
    std::fs::remove_dir(&work).map_err(|_| TestError::Check("remove native work"))?;
    if committed == mutated {
        return Err(TestError::Check("mutation did not change bytes"));
    }
    Ok(())
}
