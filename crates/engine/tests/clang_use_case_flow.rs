//! Use-case flow: a `.h` entry in a C++ package (because a sibling `.cpp`
//! exists) parses with C++ arguments and lowers both the record and its
//! out-of-line method declaration.

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_clang::ClangProject;
use backend_semantic::ir::{EntityKind, Ir};
use backend_semantic::vocabulary::{CxxStandard, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, ToolchainDomain};
use use_case_support::{CompileTimer, count_named};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SESSION_H: &[u8] = b"struct Session { int id; void reset(); };";
const SESSION_CPP: &[u8] = b"void Session::reset() {}\n";

fn clang_available() -> bool {
    Command::new("/usr/bin/clang")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-header-and-source-use-case"),
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn stage_flow_package() -> Result<(PathBuf, PathBuf, Vec<u8>), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-clang-header-and-source-{nonce}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("include")).map_err(|error| error.to_string())?;
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(root.join("include/session.h"), SESSION_H).map_err(|error| error.to_string())?;
    fs::write(root.join("src/session.cpp"), SESSION_CPP).map_err(|error| error.to_string())?;
    let header = root.join("include/session.h");
    let source = fs::read(&header).map_err(|error| error.to_string())?;
    Ok((root, header, source))
}

fn assert_session_record_and_reset(ir: &Ir) {
    assert_eq!(
        count_named(ir, EntityKind::Record, b"Session"),
        1,
        "the Session record must lower from the C++ header entry"
    );
    let session = ir
        .items()
        .find(|item| item.kind() == EntityKind::Record && item.name() == b"Session")
        .expect("Session record present");
    let reset_methods = ir
        .items()
        .filter(|item| {
            item.kind() == EntityKind::Function
                && item.name() == b"reset"
                && item.parent() == Some(session.id())
        })
        .count();
    assert_eq!(
        reset_methods,
        1,
        "the reset method must lower as a member of Session"
    );
}

#[test]
fn header_and_source_lowers_session_record_and_reset_method() {
    if !clang_available() {
        use_case_support::skip("clang", "header-and-source", "clang-missing")
            .expect("skip report writes");
        return;
    }

    let (package_root, header, source) = stage_flow_package().expect("the header/cpp fixture stages");
    let project = ClangProject::open(&package_root, &header).expect("the C++ package opens");
    let arguments = project.arguments();
    assert!(
        arguments.ends_with(&[
            "-std=c++17".to_owned(),
            "-x".to_owned(),
            "c++".to_owned(),
        ]),
        "a .h entry in a C++ package must default to explicit C++ arguments: {arguments:?}"
    );

    let work = package_root.join("work");
    fs::create_dir_all(&work).expect("native work directory is writable");
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0; 64 * 1024];
    let mut output = vec![0_u8; 4 << 20];
    let request = CompileRequest {
        profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
        stage: Stage::LowerIr,
        source: &source,
        declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(toolchain()),
        authority: SemanticAuthorityInput::Clang { project: &project },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(300),
            cancelled: &cancelled,
        },
    };
    let timer = CompileTimer::start();
    let compiled = compile_semantic(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    let elapsed = timer.elapsed();
    let _ = fs::remove_dir_all(&package_root);

    let ir = match compiled {
        Ok(output) => output.ir,
        Err(error) => panic!("Session and reset must lower from the C++ header entry: {error:?}"),
    };

    assert_session_record_and_reset(&ir);

    use_case_support::finish("clang", "header-and-source", elapsed, &ir)
        .expect("use-case report writes");
}
