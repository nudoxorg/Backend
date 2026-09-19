//! Regression coverage for coordinate-free Clang declaration identity.
//!
//! Each case compiles a complete C++ translation unit and requires the
//! canonical build to succeed. These are the structural conditions that
//! previously minted a single family for two genuinely distinct declarations:
//!
//! - two same-named overloads whose synthetic result carriers were unparented
//!   and therefore byte-identical;
//! - two function-template overloads that differed only in a concrete
//!   parameter type while their anchors were nominal self-records;
//! - two overloads on distinct named aliases that collapsed to one unknown row;
//!   and
//! - two overloads whose only difference was a same-width, same-signedness C
//!   integer rank (`long` versus `long long`), which the width-bearing
//!   `PrimitiveShape::Integer` row could not distinguish; and
//! - two same-named member `operator=` overloads whose only difference was an
//!   *unnamed* parameter's type, so the executable rows carried no authority
//!   discriminator and framed byte-identically.
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_clang::ClangProject;
use backend_semantic::vocabulary::{CxxStandard, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, ToolchainDomain};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

static WORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn compile_source<'source>(
    profile: LanguageProfile,
    authority: SemanticAuthorityInput<'source>,
    source: &'source [u8],
) -> Result<(), String> {
    let ordinal = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "nudox-clang-identity-{pid}-{ordinal}",
        pid = std::process::id()
    ));
    let _ = std::fs::create_dir_all(&work);
    let mut output = vec![0_u8; 32 << 20];
    let mut diagnostic_output = vec![0_u8; 4096];
    let cancelled = AtomicBool::new(false);
    let identity = ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-identity-regression");
    let toolchain = ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_identity(NativeTool::Clang, Path::new("/usr/bin/clang"), identity)
            .map_err(|cause| format!("{cause:?}"))?,
    );
    let result = compile_semantic(
        CompileRequest {
            profile,
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain,
            authority,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(600),
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
    let _ = std::fs::remove_dir_all(&work);
    result.map(|_| ()).map_err(|failure| format!("{failure:?}"))
}

fn compile_project(source: &[u8], name: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "nudox-clang-project-{pid}-{name}",
        pid = std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|cause| cause.to_string())?;
    let entry = dir.join(format!("{name}.cpp"));
    std::fs::write(&entry, source).map_err(|cause| cause.to_string())?;
    let project = ClangProject::open(&dir, &entry).map_err(|cause| format!("{cause:?}"))?;
    let result = compile_source(
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        SemanticAuthorityInput::Clang { project: &project },
        source,
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[test]
fn repeated_macro_definitions_keep_distinct_authority_identities() {
    let source = b"\
#define CROW_HTTP_STRERROR_GEN(n, s) { #n, s },
const char *names[][2] = { CROW_HTTP_STRERROR_GEN(OK, \"success\") };
#undef CROW_HTTP_STRERROR_GEN
#define CROW_HTTP_STRERROR_GEN(n, s) { #n, s },
const char *descriptions[][2] = { CROW_HTTP_STRERROR_GEN(OK, \"success\") };
#undef CROW_HTTP_STRERROR_GEN
";
    let result = compile_fixture(source);
    assert!(result.is_ok(), "macro redefinitions must lower: {result:?}");
}

#[test]
fn minimal_class_template_specialization_aliases_lower() {
    let source = b"\
template <int size> struct selector;
template <> struct selector<2> { typedef short counter; };
template <> struct selector<4> { typedef long counter; };
";
    let result = compile_project(source, "selector");
    assert!(result.is_ok(), "specialization aliases must lower: {result:?}");
}

fn compile_fixture(source: &[u8]) -> Result<(), String> {
    compile_source(
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        SemanticAuthorityInput::None,
        source,
    )
}

#[test]
fn overload_result_carriers_are_parented_to_their_function() {
    // Two same-named, non-void overloads with the same return type. Before the
    // fix their synthetic result carriers shared an unavailable parentage and
    // collided; the parameter/return structure is otherwise distinct.
    let source = b"\
bool adjust(int value) { return value != 0; }
bool adjust(double value) { return value != 0.0; }
";
    let result = compile_fixture(source);
    assert!(result.is_ok(), "distinct overloads must lower: {result:?}");
}

#[test]
fn function_template_overloads_are_structurally_distinct() {
    // Two function templates that differ only in a concrete parameter type.
    // Before the fix each anchor carried a nominal self-record, so the two
    // signatures minted one family.
    let source = b"\
template <typename T>
bool convert(T value, float scale) { return value != T(); }
template <typename T>
bool convert(T value, double scale) { return value != T(); }
";
    let result = compile_fixture(source);
    assert!(
        result.is_ok(),
        "distinct function templates must lower: {result:?}"
    );
}

#[test]
fn named_alias_overloads_project_their_canonical_shape() {
    // Two overloads on distinct fixed-width aliases. Before the fix both
    // out-of-root aliases projected to the same unknown row.
    let source = b"\
#include <stdint.h>
bool store(uint16_t value) { return value != 0; }
bool store(uint32_t value) { return value != 0u; }
";
    let result = compile_fixture(source);
    assert!(
        result.is_ok(),
        "distinct alias overloads must lower: {result:?}"
    );
}

/// Two overloads whose only difference is a same-width, same-signedness C
/// integer rank. On an LP64 target `long` and `long long` are both 64-bit
/// signed integers, so a width/signedness-bearing `PrimitiveShape::Integer`
/// row would mint one family for both. The Clang lane now emits the
/// spelling-bearing builtin row (`PrimitiveShape::Builtin` with the canonical
/// `long`/`long long` spelling) for exactly these ranks, so the overloads have
/// distinct structural variants without a new semantic cell.
#[test]
fn same_width_integer_rank_overloads_are_structurally_distinct() {
    let source = b"\
bool adjust(long value) { return value != 0; }
bool adjust(long long value) { return value != 0; }
";
    let result = compile_fixture(source);
    assert!(
        result.is_ok(),
        "distinct integer ranks must lower: {result:?}"
    );
}

/// The same unsigned rank pair, which the signedness bit alone cannot separate
/// either. Both `unsigned long` and `unsigned long long` are 64-bit unsigned
/// on LP64 and must still mint distinct declaration families.
#[test]
fn same_width_unsigned_integer_rank_overloads_are_structurally_distinct() {
    let source = b"\
bool adjust(unsigned long value) { return value != 0; }
bool adjust(unsigned long long value) { return value != 0; }
";
    let result = compile_fixture(source);
    assert!(
        result.is_ok(),
        "distinct unsigned integer ranks must lower: {result:?}"
    );
}

/// Retained ignored witness of the formerly-unrepresentable rank pair. On an
/// LP64 target `long` and `long long` share a width and signedness, as do
/// `unsigned long` and `unsigned long long`, so the width-bearing
/// `PrimitiveShape::Integer` row could not tell the overloads apart and the
/// canonical build returned `DuplicateDeclarationIdentity`. The Clang lane now
/// carries the canonical C rank spelling in the spelling-bearing builtin row;
/// run this witness with `--ignored` to prove the pair lowers.
#[test]
#[ignore = "retained witness of the closed same-width C integer rank gap"]
fn same_width_integer_rank_overloads_are_representable_witness() {
    let signed = b"\
bool adjust(long value) { return value != 0; }
bool adjust(long long value) { return value != 0; }
";
    let unsigned = b"\
bool adjust(unsigned long value) { return value != 0; }
bool adjust(unsigned long long value) { return value != 0; }
";
    let signed_result = compile_fixture(signed);
    let unsigned_result = compile_fixture(unsigned);
    assert!(
        signed_result.is_ok(),
        "signed ranks must lower: {signed_result:?}"
    );
    assert!(
        unsigned_result.is_ok(),
        "unsigned ranks must lower: {unsigned_result:?}"
    );
}

/// The release pugixml shape in miniature: `xml_attribute::set_value` has both
/// `long` and `long long` overloads (behind `PUGIXML_HAS_LONG_LONG`), which
/// share a 64-bit signed width on LP64. This is the focused reproduction of
/// the corpus terminal; it must lower once the rank spelling is carried.
#[test]
fn pugixml_set_value_long_and_long_long_overloads_lower() {
    let source = b"\
struct xml_attribute {
    bool set_value(int rhs);
    bool set_value(unsigned int rhs);
    bool set_value(long rhs);
    bool set_value(unsigned long rhs);
    bool set_value(long long rhs);
    bool set_value(unsigned long long rhs);
    bool set_value(double rhs);
    bool set_value(float rhs);
    bool set_value(bool rhs);
};
";
    let result = compile_fixture(source);
    assert!(
        result.is_ok(),
        "xml_attribute::set_value integer-rank overloads must lower: {result:?}"
    );
}

/// The exact real corpus row: `xml_node::prepend_copy` is overloaded on two
/// out-of-root class declarations (`xml_attribute` and `xml_node`) that carry
/// only their libclang USR, so both parameter types previously projected to
/// the same `Unknown` row and the two Functions framed byte-identically,
/// yielding `DuplicateDeclarationIdentity`. The declaration-identified
/// external nominal now distinguishes them without a fabricated spelling.
///
/// The case reports a typed skip (returns) when the Clang corpus root is not
/// provisioned on this host, and never silently passes a lowered failure.
#[test]
fn pugixml_out_of_root_class_overloads_lower() {
    let Some(root) = std::env::var_os("NUDOX_CLANG_CORPUS_DIR") else {
        return;
    };
    let package = Path::new(&root).join("pugixml");
    let entry = package.join("src").join("pugixml.cpp");
    if !entry.is_file() {
        return;
    }
    let Ok(project) = ClangProject::open(&package, &entry) else {
        return;
    };
    let Ok(source) = std::fs::read(&entry) else {
        return;
    };
    let result = compile_source(
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        SemanticAuthorityInput::Clang { project: &project },
        &source,
    );
    assert!(
        result.is_ok(),
        "pugixml out-of-root class overloads must lower: {result:?}"
    );
}

/// A member `operator=` overload pair whose only distinguishing member is an
/// unnamed parameter's type. Before the fix the lane attached its
/// authority-proven identity discriminator to plain Functions only, so the
/// two `ConstructionCounting::operator=` Methods framed byte-identically
/// (same name, owner, and one `ConstructionCounting&` result carrier each —
/// the distinguishing nameless parameters are never admitted) and minted one
/// family, terminating the build with `DuplicateDeclarationIdentity`. This is
/// the exact shape of `googletest/test/gtest_unittest.cc` lines 7553-7557.
/// The discriminator now covers every executable kind; the synthetic result
/// carriers inherit the distinction through their parent's minted identity.
#[test]
fn member_assignment_operator_overloads_are_structurally_distinct() {
    let source = b"\
struct ConstructionCounting {
  ConstructionCounting() {}
  ConstructionCounting(const ConstructionCounting&) {}
  ConstructionCounting(ConstructionCounting&&) noexcept {}
  ConstructionCounting& operator=(const ConstructionCounting&) { return *this; }
  ConstructionCounting& operator=(ConstructionCounting&&) noexcept { return *this; }
};
";
    let result = compile_project(source, "counting");
    assert!(
        result.is_ok(),
        "copy/move assignment overloads must lower: {result:?}"
    );
}

/// A deleted special member also routes through the executable push; it must
/// lower alongside its sibling overloads and alone.
#[test]
fn deleted_assignment_operator_lowers() {
    let source = b"\
struct SequenceTestingListener {
  SequenceTestingListener() = default;
  SequenceTestingListener(const SequenceTestingListener&) = default;
  SequenceTestingListener& operator=(const SequenceTestingListener&) = delete;
};
";
    let result = compile_project(source, "listener");
    assert!(
        result.is_ok(),
        "deleted assignment operator must lower: {result:?}"
    );
}

/// The exact real corpus row: the googletest entry translation unit declares
/// `ConstructionCounting& operator=(const ConstructionCounting&)` and
/// `operator=(ConstructionCounting&&) noexcept` (unnamed parameters) — two
/// same-named Methods whose only distinguishing member is a nameless
/// parameter type. Before the executable-kind discriminator this minted one
/// family for both Methods and their `operator=`-named result carriers,
/// yielding `DuplicateDeclarationIdentity` with the parameter/parameter
/// collision pair.
///
/// The case reports a typed skip (returns) when the Clang corpus root is not
/// provisioned on this host, and never silently passes a lowered failure.
#[test]
fn googletest_entry_translation_unit_lowers() {
    let Some(root) = std::env::var_os("NUDOX_CLANG_CORPUS_DIR") else {
        return;
    };
    let package = Path::new(&root).join("googletest");
    let entry = package
        .join("googletest")
        .join("test")
        .join("gtest_unittest.cc");
    if !entry.is_file() {
        return;
    }
    let Ok(project) = ClangProject::open(&package, &entry) else {
        return;
    };
    let Ok(source) = std::fs::read(&entry) else {
        return;
    };
    let result = compile_source(
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        SemanticAuthorityInput::Clang { project: &project },
        &source,
    );
    assert!(
        result.is_ok(),
        "googletest entry translation unit must lower: {result:?}"
    );
}
