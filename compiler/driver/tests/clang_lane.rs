//! Type rows are an emission-ordered log: pooled anonymous rows and fact rows
//! interleave, so these assertions locate rows by their decoded content.

use compiler_driver::{
    AuthorityFailure, CompileControl, CompileFailure, CompileOutput, CompileRequest,
    CompileScratch, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
    compile_ir,
};
use compiler_ir::{
    EntityKind, FragmentView, LanguageExtensionWireFact, NominalRef, PrimitiveShape,
    SemanticTypeTag,
};
use compiler_vocabulary::{CStandard, CxxStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::{ContentId, ToolchainDomain};
use std::{
    mem::size_of,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Distinguishes concurrent test threads that read the same wall-clock nonce.
static WORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn database_argument_borrow_array_is_pointer_sized_and_stack_safe() {
    use compiler_languages_clang::MAX_DATABASE_ARGUMENTS;
    use core::ffi::CStr;

    assert_eq!(
        size_of::<[&CStr; MAX_DATABASE_ARGUMENTS]>(),
        MAX_DATABASE_ARGUMENTS * size_of::<&CStr>()
    );
    assert!(size_of::<[&CStr; MAX_DATABASE_ARGUMENTS]>() <= 4096);
}
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error("clang compile failed")]
    Compile,
    #[error("clang lowering terminal: {0:?}")]
    Lowering(compiler_vocabulary::LoweringUnsupported),
    #[error("{0}")]
    Check(&'static str),
}

fn lower_with<'output>(
    profile: LanguageProfile,
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
            profile,
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
    .map_err(|failure| match failure {
        compiler_driver::CompileFailure::LoweringUnsupported { cause, .. } => {
            TestError::Lowering(cause)
        }
        _ => TestError::Compile,
    })?;
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

fn type_fact_rows(bytes: &[u8]) -> Result<usize, TestError> {
    let declared = usize::try_from(word(bytes, 0)?)
        .map_err(|_| TestError::Check("declared row count overflow"))?;
    let computed = usize::try_from(word(bytes, 4)?)
        .map_err(|_| TestError::Check("computed row count overflow"))?;
    declared
        .checked_add(computed)
        .ok_or(TestError::Check("row count overflow"))
}

fn child_target(view: &FragmentView<'_>) -> Result<u32, TestError> {
    let payload = view
        .type_fact_payload()
        .ok_or(TestError::Check("missing type facts"))?;
    let count = type_fact_rows(payload)?;
    let mut at = 8_usize;
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
    let child_count = word(payload, at)?;
    if child_count == 0 {
        return Err(TestError::Check("missing child"));
    }
    at += 4;
    if payload.get(at).copied() != Some(0) {
        return Err(TestError::Check("child is not local"));
    }
    word(payload, at + 1)
}

fn inspect<F>(source: &[u8], check: F) -> Result<(), TestError>
where
    F: FnOnce(&FragmentView<'_>) -> Result<(), TestError>,
{
    inspect_with(LanguageProfile::C(CStandard::C23), source, check)
}

fn inspect_cxx<F>(source: &[u8], check: F) -> Result<(), TestError>
where
    F: FnOnce(&FragmentView<'_>) -> Result<(), TestError>,
{
    inspect_with(LanguageProfile::Cxx(CxxStandard::Cxx23), source, check)
}

fn inspect_with<F>(profile: LanguageProfile, source: &[u8], check: F) -> Result<(), TestError>
where
    F: FnOnce(&FragmentView<'_>) -> Result<(), TestError>,
{
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let serial = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{nonce}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut output = vec![0xa5_u8; 65_536];
    let result = lower_with(profile, source, &mut output, &work).and_then(|view| check(&view));
    // libclang may still hold the native work dir's files briefly after the
    // authority is dropped; retry the bounded removal before failing.
    let mut removed = std::fs::remove_dir_all(&work);
    for attempt in 0..10 {
        if removed.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50u64 + 25u64 * attempt));
        removed = std::fs::remove_dir_all(&work);
    }
    result.and(removed.map_err(|_| TestError::Check("remove native work")))
}

fn inspect_ir<F>(source: &[u8], check: F) -> Result<(), TestError>
where
    F: FnOnce(&compiler_ir::Ir) -> Result<(), TestError>,
{
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let work = std::env::temp_dir().join(format!("nudox-clang-ir-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let cancelled = AtomicBool::new(false);
    let toolchain = ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_identity(
            NativeTool::Clang,
            Path::new("/usr/bin/clang"),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-semantic-lane-toolchain"),
        )
        .map_err(|_| TestError::Check("toolchain path was rejected"))?,
    );
    let mut diagnostic_output = [0_u8; 4096];
    let result = compile_ir(
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
            native_work: &work,
        },
    )
    .map_err(|_| TestError::Compile)
    .and_then(|compiled| check(&compiled.ir));
    let removed = std::fs::remove_dir_all(&work).or_else(|error| {
        (error.kind() == std::io::ErrorKind::NotFound)
            .then_some(())
            .ok_or(error)
    });
    result.and(removed.map_err(|_| TestError::Check("remove native work")))
}

fn override_occurrences<'a>(
    view: &'a FragmentView<'a>,
) -> Result<Vec<compiler_ir::DecodedOccurrence<'a>>, TestError> {
    view.occurrences()
        .ok_or(TestError::Check("occurrences"))?
        .collect::<Result<Vec<_>, _>>()
        .map(|rows| {
            rows.into_iter()
                .filter(|row| row.occurrence.kind == compiler_ir::ReferenceKind::Overrides)
                .collect()
        })
        .map_err(|_| TestError::Check("occurrence decode"))
}

fn entities<'a>(view: &'a FragmentView<'a>) -> Vec<(&'a [u8], EntityKind)> {
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    view.entities()
        .map(|entity| {
            (
                atoms.get(entity.name.raw as usize).copied().unwrap_or(&[]),
                entity.kind,
            )
        })
        .collect()
}

fn rows<'a>(
    view: &'a FragmentView<'a>,
) -> Result<Vec<compiler_ir::DecodedTypeFact<'a>>, TestError> {
    view.type_facts()
        .ok_or(TestError::Check("missing type facts"))?
        .collect::<Result<_, _>>()
        .map_err(|_| TestError::Check("type row decode"))
}

fn extension(
    view: &FragmentView<'_>,
    ordinal: usize,
) -> Result<compiler_ir::ClangFacts, TestError> {
    let payload = view
        .language_extension_payload()
        .ok_or(TestError::Check("missing extensions"))?;
    // Schema cell of the section header: 1 rows are 24 bytes, 2 rows carry
    // the leading owner ordinal and are 28 bytes.
    let schema = u32::from(
        payload
            .get(4..6)
            .and_then(|cell| <[u8; 2]>::try_from(cell).ok())
            .map(u16::from_le_bytes)
            .ok_or(TestError::Check("schema cell truncated"))?,
    );
    let stride = match schema {
        1 => 24,
        2 => 32,
        _ => return Err(TestError::Check("unknown extension schema")),
    };
    let directory = 16 + 6 * 20;
    let row_count = usize::try_from(word(payload, directory + 4)?)
        .map_err(|_| TestError::Check("row count overflow"))?;
    let fact_count = word(payload, directory + 8)?;
    let offset = usize::try_from(word(payload, directory + 12)?)
        .map_err(|_| TestError::Check("fact offset overflow"))?;
    let index = word(payload, offset + ordinal * 4)?;
    if index == u32::MAX || fact_count == 0 {
        return Err(TestError::Check("extension row absent"));
    }
    let at = offset
        + row_count * 4
        + usize::try_from(index).map_err(|_| TestError::Check("fact index overflow"))? * stride;
    compiler_ir::ClangFacts::decode(payload, at)
        .ok_or(TestError::Check("extension row undecodable"))
}

fn override_identity(
    view: &FragmentView<'_>,
    ordinal: usize,
) -> Result<Option<[u8; 16]>, TestError> {
    let payload = view
        .language_extension_payload()
        .ok_or(TestError::Check("missing extensions"))?;
    let directory = 16 + 6 * 20;
    let rows = usize::try_from(word(payload, directory + 4)?)
        .map_err(|_| TestError::Check("row count overflow"))?;
    let offset = usize::try_from(word(payload, directory + 12)?)
        .map_err(|_| TestError::Check("fact offset overflow"))?;
    let fact = usize::try_from(word(payload, offset + ordinal * 4)?)
        .map_err(|_| TestError::Check("fact index overflow"))?;
    if fact == usize::try_from(u32::MAX).unwrap_or(usize::MAX) {
        return Ok(None);
    }
    let identity_ordinal = word(payload, offset + rows * 4 + fact * 32 + 28)?;
    if identity_ordinal == u32::MAX {
        return Ok(None);
    }
    let pool = view
        .extension_pool_payload()
        .ok_or(TestError::Check("missing extension pool"))?;
    let mut at = 4;
    let parameter_count = usize::try_from(word(pool, 0)?)
        .map_err(|_| TestError::Check("parameter count overflow"))?;
    for _ in 0..parameter_count {
        let length = usize::try_from(word(pool, at + 1)?)
            .map_err(|_| TestError::Check("parameter length overflow"))?;
        at += 5 + length + 10;
    }
    for _ in 0..3 {
        let list_count = usize::try_from(word(pool, at)?)
            .map_err(|_| TestError::Check("list count overflow"))?;
        at += 4;
        for _ in 0..list_count {
            let length = usize::try_from(word(pool, at)?)
                .map_err(|_| TestError::Check("list length overflow"))?;
            at += 4 + length * 4;
        }
    }
    let count = usize::try_from(word(pool, at)?)
        .map_err(|_| TestError::Check("identity count overflow"))?;
    let identity_index = usize::try_from(identity_ordinal)
        .map_err(|_| TestError::Check("identity index overflow"))?;
    if identity_index >= count {
        return Err(TestError::Check("identity index outside pool"));
    }
    let start = at + 4 + identity_index * 16;
    let bytes = pool
        .get(start..start + 16)
        .ok_or(TestError::Check("identity cell truncated"))?;
    let identity =
        <[u8; 16]>::try_from(bytes).map_err(|_| TestError::Check("identity cell width"))?;
    Ok((identity != [0; 16]).then_some(identity))
}

fn children<'a>(
    view: &'a FragmentView<'a>,
    row: &compiler_ir::DecodedTypeFact<'a>,
) -> Result<Vec<(u8, u32, &'a [u8])>, TestError> {
    let payload = view
        .type_fact_payload()
        .ok_or(TestError::Check("missing type payload"))?;
    let count = type_fact_rows(payload)?;
    let mut at = 8;
    for _ in 0..count {
        at += 13;
        skip_name(payload, &mut at)?;
        skip_name(payload, &mut at)?;
        at += match payload.get(at).copied() {
            Some(0) => 1,
            Some(1) => 5,
            Some(2) => 21,
            _ => return Err(TestError::Check("invalid nominal cell")),
        } + 8;
    }
    let child_base = at
        .checked_add(4)
        .ok_or(TestError::Check("offset overflow"))?;
    let start = usize::try_from(row.record.children.start)
        .map_err(|_| TestError::Check("child start overflow"))?;
    let length = usize::try_from(row.record.children.length)
        .map_err(|_| TestError::Check("child length overflow"))?;
    // Child cells are variable-width on the wire: one target tag (local =
    // 1+4, external = 1+16+4, text = 1), then the optional name cell
    // (absent = 1, present = 1+4+len), then one flags byte.
    let mut cursor = child_base;
    let mut result = Vec::new();
    for position in 0..(start + length) {
        let tag = payload
            .get(cursor)
            .copied()
            .ok_or(TestError::Check("child tag truncated"))?;
        let target = match tag {
            0 => word(payload, cursor + 1)?,
            1 => word(payload, cursor + 1 + 16)?,
            2 => 0,
            _ => return Err(TestError::Check("invalid child tag")),
        };
        cursor += match tag {
            0 => 5,
            1 => 21,
            2 => 1,
            _ => 0,
        };
        let name = match payload.get(cursor).copied() {
            Some(0) => {
                cursor += 1;
                &[][..]
            }
            Some(1) => {
                let len = usize::try_from(word(payload, cursor + 1)?)
                    .map_err(|_| TestError::Check("child name overflow"))?;
                let bytes = payload
                    .get(cursor + 5..cursor + 5 + len)
                    .ok_or(TestError::Check("child name truncated"))?;
                cursor += 5 + len;
                bytes
            }
            _ => return Err(TestError::Check("invalid child name")),
        };
        cursor += 1; // flags
        if position >= start {
            result.push((tag, target, name));
        }
    }
    Ok(result)
}

/// The shared profile entry keeps trunk's typed terminal for a source with
/// zero supported declarations; the lane's database entry admits empty
/// translation units (asserted in clang_lifecycle.rs), which is the surface
/// the whole-TU lifecycle publishes through.
#[test]
fn empty_source_is_a_typed_no_declaration_terminal() -> Result<(), TestError> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let serial = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{nonce}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut output = vec![0xa5_u8; 65_536];
    let outcome = lower_with(LanguageProfile::C(CStandard::C23), b"", &mut output, &work);
    let mut removed = std::fs::remove_dir_all(&work);
    for attempt in 0..10 {
        if removed.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50u64 + 25u64 * attempt));
        removed = std::fs::remove_dir_all(&work);
    }
    removed.map_err(|_| TestError::Check("remove native work"))?;
    match outcome {
        Err(TestError::Lowering(
            compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
        )) => Ok(()),
        Err(other) => Err(other),
        Ok(_) => Err(TestError::Check("empty source was admitted as a fragment")),
    }
}

#[test]
fn mutual_recursion_collapses_forwards_and_names_pointer_children() -> Result<(), TestError> {
    let source = b"struct B; struct A { struct B *b; }; struct B { struct A *a; };";
    inspect(source, |view| {
        let got = entities(view);
        if got
            != [
                (&b"B"[..], EntityKind::Record),
                (&b"A"[..], EntityKind::Record),
                (&b"b"[..], EntityKind::Field),
                (&b"a"[..], EntityKind::Field),
            ]
        {
            return Err(TestError::Check("mutual entity order"));
        }
        let facts = rows(view)?;
        if facts.len() != 4 {
            return Err(TestError::Check("mutual row count"));
        }
        if !facts.iter().any(|row| {
            row.owner.raw == 0
                && row.record.nominal == Some(NominalRef::Local(compiler_ir::EntityId::new(0)))
        }) {
            return Err(TestError::Check("B self nominal"));
        }
        for (owner, target_name) in [(2, &b"B"[..]), (3, &b"A"[..])] {
            let row = facts
                .iter()
                .find(|row| {
                    row.owner.raw == owner
                        && row.record.tag == SemanticTypeTag::Primitive
                        && row.record.payload0 == u32::from(PrimitiveShape::MutPointer)
                        && row.record.payload1 == 0
                        && row.record.children.length == 1
                })
                .ok_or(TestError::Check("pointer row"))?;
            let child = children(view, row)?
                .into_iter()
                .next()
                .ok_or(TestError::Check("pointer child"))?;
            if !child.2.is_empty() {
                return Err(TestError::Check("pointer child name"));
            }
            let target = got
                .iter()
                .position(|(name, _)| *name == target_name)
                .ok_or(TestError::Check("target entity"))? as u32;
            let target_row = facts
                .iter()
                .find(|candidate| candidate.owner.raw == target)
                .ok_or(TestError::Check("target row"))?;
            if child.0 != 0 || child.1 != target_row.owner.raw {
                return Err(TestError::Check("pointer target"));
            }
        }
        Ok(())
    })
}

#[test]
fn build_ir_preserves_authority_members_and_parents() -> Result<(), TestError> {
    inspect_ir(b"struct Pair { int left; int right; };", |ir| {
        let pair = ir
            .items()
            .find(|item| item.name() == b"Pair")
            .ok_or(TestError::Check("Pair item"))?;
        let members = pair.members();
        if members.len() != 2
            || members[0].index() >= ir.entity_count()
            || members[1].index() >= ir.entity_count()
        {
            return Err(TestError::Check("Pair members"));
        }
        let left = ir.item(members[0]).ok_or(TestError::Check("left item"))?;
        let right = ir.item(members[1]).ok_or(TestError::Check("right item"))?;
        if left.name() != b"left"
            || right.name() != b"right"
            || left.parent() != Some(pair.id())
            || right.parent() != Some(pair.id())
        {
            return Err(TestError::Check("member parents/order"));
        }
        Ok(())
    })
}

#[test]
fn c_render_is_visibility_free_qualified_and_byte_stable() -> Result<(), TestError> {
    let source = b"struct RenderNode { const struct RenderNode *next; int value; };";
    let render = || {
        let mut rendered = String::new();
        inspect_ir(source, |ir| {
            let item = ir
                .items_named(b"RenderNode")
                .next()
                .ok_or(TestError::Check("RenderNode item"))?;
            let signature = ir
                .signature(item.id())
                .ok_or(TestError::Check("RenderNode signature"))?;
            rendered = signature.to_string();
            Ok(())
        })?;
        Ok::<String, TestError>(rendered)
    };
    let first = render()?;
    if first
        .lines()
        .any(|line| line.trim_start().starts_with("pub "))
    {
        return Err(TestError::Check("C rendering has pub prefix"));
    }
    if !first.starts_with("struct RenderNode {") {
        return Err(TestError::Check("C struct opening"));
    }
    if first != "struct RenderNode {\n    const struct RenderNode *next;\n    int value;\n};" {
        return Err(TestError::Check("C qualified member rendering"));
    }
    let second = render()?;
    if first != second {
        return Err(TestError::Check("C rendering changed across runs"));
    }
    Ok(())
}

#[test]
fn enumerators_and_typedef_have_content_addressed_rows() -> Result<(), TestError> {
    inspect(
        b"enum Color { RED, GREEN };\ntypedef enum Color ColorAlias;\n",
        |view| {
            let got = entities(view);
            if got.iter().map(|(_, kind)| *kind).collect::<Vec<_>>()
                != [
                    EntityKind::Enum,
                    EntityKind::Variant,
                    EntityKind::Variant,
                    EntityKind::Alias,
                ]
            {
                return Err(TestError::Check("enum kinds"));
            }
            let red = got
                .iter()
                .position(|(name, _)| *name == b"RED")
                .ok_or(TestError::Check("RED entity"))? as u32;
            let row = rows(view)?
                .into_iter()
                .find(|row| row.owner.raw == red)
                .ok_or(TestError::Check("RED row"))?;
            if row.record.tag != SemanticTypeTag::Primitive
                || row.record.payload0 != u32::from(PrimitiveShape::Integer)
                || row.record.payload1
                    != (32 << 1) | compiler_ir::SemanticTypeRecord::INTEGER_SIGNED_FLAG
            {
                return Err(TestError::Check("RED integer payload"));
            }
            Ok(())
        },
    )
}

#[test]
fn signatures_and_local_call_are_content_addressed() -> Result<(), TestError> {
    let source = b"int add(int a, int b);\nint use(void) { return add(1, 2); }\n";
    inspect(source, |view| {
        let got = entities(view);
        for name in [&b"a"[..], &b"b"[..]] {
            if !got.contains(&(name, EntityKind::Parameter)) {
                return Err(TestError::Check("parameter"));
            }
        }
        let add = got
            .iter()
            .position(|(name, kind)| *name == b"add" && *kind == EntityKind::Function)
            .ok_or(TestError::Check("add"))? as u32;
        let use_ordinal = got
            .iter()
            .position(|(name, kind)| *name == b"use" && *kind == EntityKind::Function)
            .ok_or(TestError::Check("use"))? as u32;
        let facts = rows(view)?;
        let function = facts
            .iter()
            .find(|row| row.owner.raw == add)
            .ok_or(TestError::Check("add fact"))?;
        if function.record.tag != SemanticTypeTag::FunctionPointer
            || function.record.payload1 != compiler_ir::SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || function.record.children.length != 3
        {
            return Err(TestError::Check("add signature"));
        }
        let call = view
            .occurrences()
            .ok_or(TestError::Check("occurrences"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TestError::Check("occurrence decode"))?
            .into_iter()
            .find(|row| row.occurrence.kind == compiler_ir::ReferenceKind::FunctionCall)
            .ok_or(TestError::Check("call"))?;
        // The call is the last `add` spelling in the fixture; the occurrence
        // span is relative to the owner's extent start.
        let call_offset = source
            .windows(3)
            .rposition(|window| window == b"add")
            .ok_or(TestError::Check("call spelling"))?;
        let use_start = source
            .windows(7)
            .position(|window| window == b"int use")
            .ok_or(TestError::Check("owner extent"))?;
        let relative = call_offset
            .checked_sub(use_start)
            .ok_or(TestError::Check("span underflow"))?;
        if call.owner.raw != use_ordinal
            || call.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
            || call.occurrence.span.start
                != u32::try_from(relative).map_err(|_| TestError::Check("span overflow"))?
        {
            return Err(TestError::Check("call ownership/span"));
        }
        if call.occurrence.target
            != compiler_ir::OccurrenceTarget::Local(compiler_ir::EntityId::new(add))
        {
            return Err(TestError::Check("call target"));
        }
        Ok(())
    })
}

#[test]
fn qualifiers_storage_and_incomplete_layout_are_extension_facts() -> Result<(), TestError> {
    inspect(
        b"static const int limit = 10;\nstruct Incomplete;\nextern volatile int flag;\n",
        |view| {
            let got = entities(view);
            let limit = got
                .iter()
                .position(|(name, _)| *name == b"limit")
                .ok_or(TestError::Check("limit"))?;
            let flag = got
                .iter()
                .position(|(name, _)| *name == b"flag")
                .ok_or(TestError::Check("flag"))?;
            let incomplete = got
                .iter()
                .position(|(name, _)| *name == b"Incomplete")
                .ok_or(TestError::Check("Incomplete"))?;
            let limit_fact = extension(view, limit)?;
            if !limit_fact.qualifiers.is_const
                || limit_fact.storage != compiler_ir::ClangStorageClass::Static
                || limit_fact.layout.size_bits != Some(32)
                || limit_fact.layout.align_bits != Some(32)
            {
                return Err(TestError::Check("limit extension"));
            }
            let flag_fact = extension(view, flag)?;
            if !flag_fact.qualifiers.is_volatile
                || flag_fact.storage != compiler_ir::ClangStorageClass::Extern
            {
                return Err(TestError::Check("flag extension"));
            }
            let incomplete_fact = extension(view, incomplete)?;
            if incomplete_fact.layout.size_bits.is_some()
                || incomplete_fact.layout.align_bits.is_some()
            {
                return Err(TestError::Check("incomplete layout"));
            }
            Ok(())
        },
    )
}

#[test]
fn template_parameter_is_pooled_and_field_is_typevar() -> Result<(), TestError> {
    let source =
        b"template<typename T> struct Box { T value; };\nstruct User { struct Box<int> box; };\n";
    inspect_cxx(source, |view| {
        let got = entities(view);
        if got.iter().any(|(name, _)| *name == b"T") {
            return Err(TestError::Check("T became entity"));
        }
        let box_ordinal = got
            .iter()
            .position(|(name, _)| *name == b"Box")
            .ok_or(TestError::Check("Box"))?;
        let fact = extension(view, box_ordinal)?;
        let pool = view
            .extension_pool_payload()
            .ok_or(TestError::Check("extension pool"))?;
        let parameter_index = fact.templates.raw as usize;
        let count =
            usize::try_from(word(pool, 0)?).map_err(|_| TestError::Check("parameter count"))?;
        if parameter_index >= count {
            return Err(TestError::Check("parameter index"));
        }
        let mut at = 4;
        let mut parameter = &[][..];
        for index in 0..count {
            if pool.get(at) != Some(&1) {
                return Err(TestError::Check("parameter presence"));
            }
            let length = usize::try_from(word(pool, at + 1)?)
                .map_err(|_| TestError::Check("parameter length"))?;
            let name = pool
                .get(at + 5..at + 5 + length)
                .ok_or(TestError::Check("parameter text"))?;
            if index == parameter_index {
                parameter = name;
            }
            at += 5 + length + 10;
        }
        if parameter != b"T" {
            return Err(TestError::Check("pooled T"));
        }
        let value = got
            .iter()
            .position(|(name, _)| *name == b"value")
            .ok_or(TestError::Check("value"))? as u32;
        let row = rows(view)?
            .into_iter()
            .find(|row| row.owner.raw == value)
            .ok_or(TestError::Check("value row"))?;
        if row.record.tag != SemanticTypeTag::TypeVar || row.record.text != Some(b"T") {
            return Err(TestError::Check("value TypeVar"));
        }
        Ok(())
    })
}

#[test]
fn anonymous_struct_names_its_field_child() -> Result<(), TestError> {
    inspect(b"struct { int x; } point;\n", |view| {
        let got = entities(view);
        if got
            != [
                (&b"x"[..], EntityKind::Field),
                (&b"point"[..], EntityKind::Static),
            ]
        {
            return Err(TestError::Check("anonymous entities"));
        }
        let point = 1_u32;
        let facts = rows(view)?;
        let row = facts
            .iter()
            .find(|row| row.owner.raw == point)
            .ok_or(TestError::Check("anonymous row"))?;
        if row.record.tag != SemanticTypeTag::AnonymousRecord
            || row.record.payload0 != 0
            || row.record.payload1 != 0
            || row.record.children.length != 1
        {
            return Err(TestError::Check("anonymous record"));
        }
        let child = children(view, row)?
            .into_iter()
            .next()
            .ok_or(TestError::Check("anonymous child"))?;
        if child.0 != 0 || child.1 != 0 || child.2 != b"x" {
            return Err(TestError::Check("anonymous child content"));
        }
        Ok(())
    })
}

#[test]
fn include_atoms_share_one_extension_pool_list() -> Result<(), TestError> {
    inspect(
        b"#include <stdio.h>\n#include \"local.h\"\nint x;\n",
        |view| {
            let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
            if !atoms.contains(&&b"stdio.h"[..]) || !atoms.contains(&&b"local.h"[..]) {
                return Err(TestError::Check("include atoms"));
            }
            let fact = extension(view, 0)?;
            let pool = view
                .extension_pool_payload()
                .ok_or(TestError::Check("extension pool"))?;
            let parameter_count =
                usize::try_from(word(pool, 0)?).map_err(|_| TestError::Check("pool count"))?;
            let mut at = 4;
            for _ in 0..parameter_count {
                let length = usize::try_from(word(pool, at + 1)?)
                    .map_err(|_| TestError::Check("pool parameter"))?;
                at += 5 + length + 10;
            }
            let list_count =
                usize::try_from(word(pool, at)?).map_err(|_| TestError::Check("list count"))?;
            at += 4;
            let index = fact.includes.raw as usize;
            if index >= list_count {
                return Err(TestError::Check("include list index"));
            }
            let mut list = Vec::new();
            for list_index in 0..list_count {
                let length = usize::try_from(word(pool, at)?)
                    .map_err(|_| TestError::Check("list length"))?;
                at += 4;
                if list_index == index {
                    for item in 0..length {
                        list.push(word(pool, at + item * 4)?);
                    }
                }
                at += length * 4;
            }
            if list.len() != 2
                || !list.iter().all(|coordinate| {
                    atoms
                        .get(*coordinate as usize)
                        .is_some_and(|atom| *atom == b"stdio.h" || *atom == b"local.h")
                })
            {
                return Err(TestError::Check("shared include list"));
            }
            Ok(())
        },
    )
}

#[test]
fn doxygen_ref_is_a_local_link_with_text_fragments() -> Result<(), TestError> {
    let source = b"/// Adds one.\n/// See @ref add and foreign things.\nint add(int a);\n";
    inspect(source, |view| {
        let docs = view
            .docs()
            .ok_or(TestError::Check("docs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TestError::Check("doc decode"))?;
        let add = entities(view)
            .iter()
            .position(|(name, kind)| *name == b"add" && *kind == EntityKind::Function)
            .ok_or(TestError::Check("add entity"))? as u32;
        let mut link = false;
        for doc in &docs {
            if let compiler_ir::DocFragmentInput::Link { label, target } = &doc.fragment {
                if *label == b"add"
                    && *target == compiler_ir::DocLinkTarget::Local(compiler_ir::EntityId::new(add))
                {
                    link = true;
                }
            }
        }
        if docs.len() < 4 || !link {
            return Err(TestError::Check("doxygen fragments/link"));
        }
        Ok(())
    })
}

#[test]
fn macro_definition_and_invocation_are_typed_facts() -> Result<(), TestError> {
    inspect(b"#define LIMIT 100\nint x = LIMIT;\n", |view| {
        let got = entities(view);
        if !got.contains(&(&b"LIMIT"[..], EntityKind::Constant)) {
            return Err(TestError::Check("LIMIT constant"));
        }
        let invocation = view
            .occurrences()
            .ok_or(TestError::Check("occurrences"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TestError::Check("occurrence decode"))?
            .into_iter()
            .find(|row| row.occurrence.kind == compiler_ir::ReferenceKind::MacroInvocation)
            .ok_or(TestError::Check("macro invocation"))?;
        if invocation.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle {
            return Err(TestError::Check("macro confidence"));
        }
        Ok(())
    })
}

#[test]
fn capacity_terminal_preserves_clang_scratch_capacity_cause() -> Result<(), TestError> {
    let mut source = String::new();
    for ordinal in 0..1025 {
        source.push_str(&format!("struct value_{ordinal};\n"));
    }
    let work = std::env::temp_dir().join(format!("nudox-clang-capacity-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut output = vec![0xa5_u8; 65_536];
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
    let result = compile(
        CompileRequest {
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
            source: source.as_bytes(),
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain,
            authority: SemanticAuthorityInput::None,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic_output,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    if let Err(error) = std::fs::remove_dir_all(&work) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(TestError::Check("remove native work"));
        }
    }
    match result {
        Err(CompileFailure::Authority {
            failure:
                AuthorityFailure::Clang {
                    cause:
                        compiler_languages_clang::CollectError::ScratchCapacity {
                            lane: compiler_languages_clang::ScratchLane::Declarations,
                            capacity: 1024,
                            required: 1025,
                        },
                    ..
                },
            ..
        }) => Ok(()),
        Ok(_) => Err(TestError::Check("capacity admitted")),
        Err(_) => Err(TestError::Check("wrong capacity terminal")),
    }
}

#[test]
fn recursive_pointer_rows_are_content_addressed_and_mutation_changes_shape() -> Result<(), TestError>
{
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{}",
        std::process::id(),
        WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut output = vec![0xa5_u8; 65_536];
    let committed = {
        let view = lower_with(
            LanguageProfile::C(CStandard::C23),
            b"struct Node { struct Node *next; };",
            &mut output,
            &work,
        )?;
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
        if pointers[0].record.payload1 != 0 {
            return Err(TestError::Check("mutable pointer payload"));
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
        let view = lower_with(
            LanguageProfile::C(CStandard::C23),
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
    if let Err(error) = std::fs::remove_dir_all(&work) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(TestError::Check("remove native work"));
        }
    }
    if committed == mutated {
        return Err(TestError::Check("mutation did not change bytes"));
    }
    Ok(())
}

#[test]
fn local_virtual_override_is_backward_oracle_occurrence() -> Result<(), TestError> {
    inspect_cxx(
        b"struct Base { virtual int f(); };\nstruct Derived : Base { int f() override; };\n",
        |view| {
            let rows = override_occurrences(view)?;
            if rows.len() != 1 {
                return Err(TestError::Check("local override count"));
            }
            let row = &rows[0];
            let derived = row.owner.raw;
            let compiler_ir::OccurrenceTarget::Local(base_entity) = row.occurrence.target else {
                return Err(TestError::Check("local override target"));
            };
            let base = base_entity.raw;
            let entities = entities(view);
            let derived_index = usize::try_from(derived)
                .map_err(|_| TestError::Check("derived ordinal overflow"))?;
            let base_index =
                usize::try_from(base).map_err(|_| TestError::Check("base ordinal overflow"))?;
            if entities.get(derived_index) != Some(&(&b"f"[..], EntityKind::Function))
                || entities.get(base_index) != Some(&(&b"f"[..], EntityKind::Function))
            {
                return Err(TestError::Check("override method owners"));
            }
            if row.owner.raw != derived
                || base >= derived
                || row.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
                || row.occurrence.span.start >= row.occurrence.span.end
            {
                return Err(TestError::Check("local override content"));
            }
            Ok(())
        },
    )
}

#[test]
fn virtual_override_absence_and_plain_shadowing_are_distinct_bytes() -> Result<(), TestError> {
    let virtual_source =
        b"struct Base { virtual int f(); };\nstruct Derived : Base { int f() override; };\n";
    let plain_source = b"struct Base { int f(); };\nstruct Derived : Base { int f(); };\n";
    let mut virtual_bytes = vec![0xa5; 65_536];
    let mut plain_bytes = vec![0xa5; 65_536];
    let work = std::env::temp_dir().join(format!("nudox-clang-overrides-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let (virtual_has_override, virtual_snapshot) = {
        let view = lower_with(
            LanguageProfile::Cxx(CxxStandard::Cxx23),
            virtual_source,
            &mut virtual_bytes,
            &work,
        )?;
        (
            !override_occurrences(&view)?.is_empty(),
            view.as_ref().to_vec(),
        )
    };
    let (plain_has_override, plain_snapshot) = {
        let view = lower_with(
            LanguageProfile::Cxx(CxxStandard::Cxx23),
            plain_source,
            &mut plain_bytes,
            &work,
        )?;
        let plain_method = entities(&view)
            .iter()
            .rposition(|(name, kind)| *name == b"f" && *kind == EntityKind::Function)
            .ok_or(TestError::Check("plain method"))?;
        if override_identity(&view, plain_method)?.is_some() {
            return Err(TestError::Check("plain override identity"));
        }
        (
            !override_occurrences(&view)?.is_empty(),
            view.as_ref().to_vec(),
        )
    };
    if plain_has_override || virtual_snapshot == plain_snapshot {
        return Err(TestError::Check("virtual mutation falsifier"));
    }
    if let Err(error) = std::fs::remove_dir_all(&work) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(TestError::Check("remove native work"));
        }
    }
    if !virtual_has_override {
        return Err(TestError::Check("virtual override absent"));
    }
    Ok(())
}

#[test]
fn foreign_virtual_override_is_recorded_schema_two_deferral() -> Result<(), TestError> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-foreign-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let header = work.join("base.h");
    std::fs::write(&header, b"struct Base { virtual int f(); };\n")
        .map_err(|_| TestError::Check("write base header"))?;
    let source = format!(
        "#include \"{}\"\nstruct Derived : Base {{ int f() override; }};\n",
        header.display()
    );
    let mut output = vec![0xa5; 65_536];
    let view = lower_with(
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        source.as_bytes(),
        &mut output,
        &work,
    )?;
    let method = entities(&view)
        .iter()
        .rposition(|(name, kind)| *name == b"f" && *kind == EntityKind::Function)
        .ok_or(TestError::Check("foreign method"))?;
    if override_identity(&view, method)?.is_none() {
        return Err(TestError::Check("foreign identity pool"));
    }
    if !override_occurrences(&view)?.is_empty() {
        return Err(TestError::Check("foreign override fabricated"));
    }
    drop(view);
    std::fs::remove_dir_all(&work).map_err(|_| TestError::Check("remove native work"))
}

/// A translation unit whose include spellings exceed the shared emission
/// lane's pooled-list element bound must fail with the exact typed rejection
/// (ordinal, cause `RefListElements`), never the cause-erased unsupported
/// declaration terminal.  Sixteen includes stay representable; seventeen
/// cross the pooled row and name the wall precisely.
#[test]
fn include_list_over_the_pooled_bound_names_the_exact_cause() -> Result<(), TestError> {
    use compiler_driver::{DatabaseCompileFailure, compile_database_translation_unit};
    use std::path::Path;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let serial = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{nonce}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(work.join("include"))
        .map_err(|_| TestError::Check("create include dir"))?;
    for index in 0..17u32 {
        std::fs::write(
            work.join("include").join(format!("header_{index}.h")),
            format!("int included_{index};\n"),
        )
        .map_err(|_| TestError::Check("write header"))?;
    }
    let mut source = String::new();
    for index in 0..17u32 {
        source.push_str(&format!("#include \"header_{index}.h\"\n"));
    }
    source.push_str("int included_total;\n");
    let source = source.into_bytes();
    std::fs::write(work.join("src.c"), &source).map_err(|_| TestError::Check("write source"))?;
    let arguments = format!(
        "[{{\"directory\":\"{}\",\"file\":\"src.c\",\"arguments\":[\"clang\",\"-I\",\"include\",\"-c\",\"src.c\"]}}]",
        work.display()
    );
    std::fs::write(work.join("compile_commands.json"), arguments)
        .map_err(|_| TestError::Check("write database"))?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0xa5_u8; 4 << 20];
    let toolchain = ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-lane-pooled-bound"),
    )
    .map_err(|_| TestError::Check("toolchain"))?;
    let result = compile_database_translation_unit(
        &work,
        Path::new("src.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        &source,
        toolchain,
        &cancelled,
        &mut output,
    );
    match result {
        Err(DatabaseCompileFailure::Rejected { rejected, .. }) => {
            if rejected.cause != compiler_driver::FactFault::RefListElements {
                return Err(TestError::Check("wrong pooled-bound cause"));
            }
        }
        Err(_) => return Err(TestError::Check("wrong pooled-bound terminal")),
        Ok(_) => return Err(TestError::Check("pooled-bound wall admitted")),
    }
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Check("rejected compile touched the output"));
    }
    std::fs::remove_dir_all(&work).map_err(|_| TestError::Check("remove native work"))
}

/// A function whose signature exceeds the lane's fixed child width is an
/// exact typed rejection — never a silently shortened signature. Sixteen
/// parameters remain representable; seventeen cross the lane and name the
/// cause.
#[test]
fn signature_beyond_the_child_width_is_an_exact_rejection() -> Result<(), TestError> {
    use compiler_driver::{DatabaseCompileFailure, compile_database_translation_unit};
    use std::path::Path;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let serial = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{nonce}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let mut source = String::new();
    let mut call_arguments = String::new();
    for index in 0..17u32 {
        source.push_str(&format!("int parameter_{index};\n"));
        if index > 0 {
            call_arguments.push_str(", ");
        }
        call_arguments.push_str(&format!("parameter_{index}"));
    }
    source.push_str(&format!(
        "int wide({args}) {{ return parameter_0; }}\nint narrow(int only) {{ return only; }}\n",
        args = call_arguments
    ));
    std::fs::write(work.join("src.c"), &source).map_err(|_| TestError::Check("write source"))?;
    let arguments = format!(
        "[{{\"directory\":\"{}\",\"file\":\"src.c\",\"arguments\":[\"clang\",\"-c\",\"src.c\"]}}]",
        work.display()
    );
    std::fs::write(work.join("compile_commands.json"), arguments)
        .map_err(|_| TestError::Check("write database"))?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0xa5_u8; 4 << 20];
    let toolchain = ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-lane-signature-width"),
    )
    .map_err(|_| TestError::Check("toolchain"))?;
    let result = compile_database_translation_unit(
        &work,
        Path::new("src.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        source.as_bytes(),
        toolchain,
        &cancelled,
        &mut output,
    );
    match result {
        Err(DatabaseCompileFailure::Rejected { rejected, .. }) => {
            if rejected.cause != compiler_driver::FactFault::ChildCapacity {
                return Err(TestError::Check("wrong signature-width cause"));
            }
        }
        Err(other) => return Err(TestError::Check("wrong signature-width terminal")),
        Ok(_) => return Err(TestError::Check("signature-width wall admitted")),
    }
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Check("rejected compile touched the output"));
    }
    std::fs::remove_dir_all(&work).map_err(|_| TestError::Check("remove native work"))
}

/// An admission fault on the database compile path keeps its exact cause and
/// operands: an output buffer too small for the fragment is a Write-class
/// terminal naming the source and recipe, not a cause-erased unsupported
/// declaration.
#[test]
fn admission_fault_names_its_cause_on_the_database_path() -> Result<(), TestError> {
    use compiler_driver::{DatabaseCompileFailure, compile_database_translation_unit};
    use std::path::Path;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TestError::Check("clock before epoch"))?
        .as_nanos();
    let serial = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-lane-{}-{nonce}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).map_err(|_| TestError::Check("create native work"))?;
    let source = b"struct Admitted { int field; };\nint admitted_value;\n";
    std::fs::write(work.join("src.c"), source).map_err(|_| TestError::Check("write source"))?;
    let arguments = format!(
        "[{{\"directory\":\"{}\",\"file\":\"src.c\",\"arguments\":[\"clang\",\"-c\",\"src.c\"]}}]",
        work.display()
    );
    std::fs::write(work.join("compile_commands.json"), arguments)
        .map_err(|_| TestError::Check("write database"))?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0xa5_u8; 64];
    let toolchain = ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-lane-admission-cause"),
    )
    .map_err(|_| TestError::Check("toolchain"))?;
    let result = compile_database_translation_unit(
        &work,
        Path::new("src.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        source,
        toolchain,
        &cancelled,
        &mut output,
    );
    match result {
        Err(
            DatabaseCompileFailure::Write { .. }
            | DatabaseCompileFailure::Prepare { .. }
            | DatabaseCompileFailure::Canonical { .. },
        ) => {}
        Err(DatabaseCompileFailure::Lowering(cause)) => {
            // The old cause-erased terminal: the reviewer's M5 mutant.
            return Err(TestError::Lowering(cause));
        }
        other => {
            let _ = other;
            return Err(TestError::Check("wrong admission terminal"));
        }
    }
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestError::Check("failed compile touched the output"));
    }
    std::fs::remove_dir_all(&work).map_err(|_| TestError::Check("remove native work"))
}
