//! Focused repro for the `jemalloc` audit row.
//!
//! The selected file (`src/ctl.c`) generates whole families of ctl functions
//! through macros like `CTL_RO_NL_CGEN(c, n, v, t)`. Every expanded parameter
//! projects onto the single invocation span, so each parameter's projected
//! name is the exact invocation text and several parameters of one function
//! share both that name and an identical type graph (`void *oldp` and
//! `void *newp`; `size_t miblen` and the int result carrier). Every
//! flattened identity plane then collided and the identity build raised
//! `DuplicateDeclarationIdentity`. The lane now frames the authority's
//! canonical USR identity — and, when a synthetic carrier carries none, the
//! count of already-pushed identical carriers — for exactly such twins, so
//! each source declaration keeps a distinct coordinate-free identity while
//! every non-twin variant stays byte-identical.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_clang::ClangProject;
use backend_semantic::vocabulary::{CxxStandard, LanguageProfile, NativeTool, Stage};
use backend_version::ContentId;

static WORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn compile_project(source: &[u8], name: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!(
        "nudox-clang-macro-twin-{pid}-{name}",
        pid = std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|cause| cause.to_string())?;
    let entry = dir.join(format!("{name}.c"));
    std::fs::write(&entry, source).map_err(|cause| cause.to_string())?;
    let project = ClangProject::open(&dir, &entry).map_err(|cause| format!("{cause:?}"))?;
    let work = dir.join("work");
    std::fs::create_dir_all(&work).map_err(|cause| cause.to_string())?;
    let mut output = vec![0_u8; 32 << 20];
    let mut diagnostic_output = vec![0_u8; 8192];
    let cancelled = AtomicBool::new(false);
    let identity =
        ContentId::from_canonical_bytes(b"clang-macro-signature-twin-repro");
    let toolchain = ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_identity(NativeTool::Clang, Path::new("/usr/bin/clang"), identity)
            .map_err(|cause| format!("{cause:?}"))?,
    );
    let result = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain,
            authority: SemanticAuthorityInput::Clang { project: &project },
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
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|_| ()).map_err(|failure| format!("{failure:?}"))
}

/// A generated function whose parameters repeat one type under one projected
/// invocation name — the exact collision shape the corpus row surfaces.
#[test]
fn macro_generated_signature_twins_keep_distinct_identities() {
    let source = b"\
typedef struct tsd_s tsd_t;
#define RO_GEN(prefix, val_type) \
static int prefix##_ctl(tsd_t *tsd, const val_type *mib, \
    void *oldp, void *newp) { \
    (void)tsd; (void)mib; (void)oldp; (void)newp; return 0; \
}
RO_GEN(alpha, int)
RO_GEN(beta, long)
int use(void) { return alpha_ctl(0, 0, 0, 0) + beta_ctl(0, 0, 0, 0); }
";
    let outcome = compile_project(source, "macro_twins");
    eprintln!("MACRO-TWINS => {outcome:?}");
    assert!(
        outcome.is_ok(),
        "macro-generated twin parameters must keep distinct identities, got {outcome:?}"
    );
}

/// A written (non-macro) function with repeated parameter types never
/// collided — its parameters carry real written names. The twin framing must
/// stay off for it.
#[test]
fn written_same_typed_parameters_stay_unchanged() {
    let source = b"\
int pair(void *left, void *right) { return left == right; }
";
    let outcome = compile_project(source, "written_params");
    eprintln!("WRITTEN-PARAMS => {outcome:?}");
    assert!(outcome.is_ok(), "the written shape must keep lowering, got {outcome:?}");
}

/// A function template expanded by a macro never reaches an executable's
/// signature framing: its anchor is a pass-one template and its parameters
/// fall to the orphan-parameter path. Two parameters of one expansion share
/// the invocation-text projected name and one dependent frame, so the orphan
/// path must apply the same twin discrimination (`type_safe`'s
/// `TYPE_SAFE_DETAIL_MAKE_OP` comparison operators are the measured case).
#[test]
fn macro_generated_template_parameter_twins_keep_distinct_identities() {
    let source = b"\
template <typename T> struct box { T value; };
#define MAKE_OP(op) \
template <typename T> bool operator op (const box<T>& lhs, const box<T>& rhs) { \
    return lhs.value op rhs.value; \
}
MAKE_OP(==)
MAKE_OP(!=)
MAKE_OP(<)
int use = (box<int>{1} == box<int>{2}) - (box<int>{1} < box<int>{2});
";
    let outcome = compile_project(source, "macro_template_twins");
    eprintln!("MACRO-TEMPLATE-TWINS => {outcome:?}");
    assert!(
        outcome.is_ok(),
        "macro-generated template parameter twins must keep distinct identities, got {outcome:?}"
    );
}

/// The corpus-guarded half: the exact audited file, end to end.
#[test]
fn clang_type_safe_optional_lowers() {
    let Some(root) = std::env::var_os("NUDOX_CLANG_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("NUDOX_CLANG_CORPUS_DIR unset; skipping the corpus-guarded row");
        return;
    };
    let package = root.join("type_safe");
    let entry = package.join("include/type_safe/optional.hpp");
    let Ok(bytes) = std::fs::read(&entry) else {
        panic!("corpus pinned entry must exist at {}", entry.display());
    };
    assert!(
        bytes
            .windows(b"TYPE_SAFE_DETAIL_MAKE_OP(".len())
            .filter(|window| window == b"TYPE_SAFE_DETAIL_MAKE_OP(")
            .count()
            > 0,
        "the pinned corpus entry must still carry its generated operator family"
    );
    let outcome = compile_project(&bytes, "optional");
    eprintln!("TYPE-SAFE optional.hpp => {outcome:?}");
    assert!(
        outcome.is_ok(),
        "the audited type_safe entry must lower, got {outcome:?}"
    );
}

/// The corpus-guarded half: the exact audited file, end to end.
#[test]
fn clang_jemalloc_ctl_lowers() {
    let Some(root) = std::env::var_os("NUDOX_CLANG_CORPUS_DIR").map(PathBuf::from) else {
        eprintln!("NUDOX_CLANG_CORPUS_DIR unset; skipping the corpus-guarded row");
        return;
    };
    let package = root.join("jemalloc");
    let entry = package.join("src/ctl.c");
    let Ok(bytes) = std::fs::read(&entry) else {
        panic!("corpus pinned entry must exist at {}", entry.display());
    };
    assert!(
        bytes
            .windows(b"CTL_RO_NL_CGEN(".len())
            .filter(|window| window == b"CTL_RO_NL_CGEN(")
            .count()
            > 0,
        "the pinned corpus entry must still carry its generated ctl family"
    );
    let outcome = compile_project(&bytes, "ctl");
    eprintln!("JEMALLOC ctl.c => {outcome:?}");
    assert!(
        outcome.is_ok(),
        "the audited jemalloc entry must lower, got {outcome:?}"
    );
}
