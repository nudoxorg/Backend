//! Exercises the single-owner compiler runtime through its public capability boundary.

use std::{
    fs,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use compiler_application::{
    LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
    LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
    LocalRuntimeToolchain,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, PythonVersion, Stage};
use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};
use interface_core::{
    CompilerCapability, CompilerRequest, CompilerTerminal, SemanticImageAccessError,
    SemanticImageAuthority,
};
use server_journal::PublicationLimits;

static RUNTIME_ORDINAL: AtomicUsize = AtomicUsize::new(0);

#[test]
fn worker_retains_exact_unavailable_toolchain_terminal_and_joins_on_last_client() {
    let ordinal = RUNTIME_ORDINAL.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "nudox-compiler-runtime-{}-{ordinal}",
        std::process::id()
    ));
    let native_work = root.join("native-work");
    fs::create_dir_all(&native_work).expect("runtime fixture directory is created");
    let paths =
        LocalCompilerRuntimePaths::new(root.join("artifacts"), root.join("journal"), native_work)
            .expect("fixture paths are absolute");
    let timeout =
        LocalCompilerTimeout::new(Duration::from_secs(2)).expect("fixture timeout is bounded");
    let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .expect("one-slot publisher limits are representable");
    let configuration = LocalCompilerRuntimeConfiguration::new(
        paths,
        vec![LocalRuntimeToolchain::unavailable(NativeTool::Python)].into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        timeout,
        limits,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(4 * 1024 * 1024).expect("fixture capacity is nonzero"),
        )
        .expect("planned package scratch is allocated once"),
    )
    .expect("runtime tables are in canonical order");
    let mut client = LocalCompilerClient::start(configuration)
        .expect("single compiler owner starts before the client becomes ready");
    let peer = client.clone();
    let terminal = client
        .generate(CompilerRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source: "answer = 42\n",
        })
        .expect_err("explicitly unavailable Python cannot produce a fabricated artifact");
    assert!(matches!(
        terminal,
        CompilerTerminal::Toolchain {
            language: backend_semantic::vocabulary::Language::Python,
            stage: Stage::LowerIr,
            selected: NativeTool::Python,
            configured: None,
            ..
        }
    ));
    let requested = SemanticImageAuthority {
        identity: ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
            b"not-a-semantic-image",
        ),
        byte_len: 20,
    };
    assert!(matches!(
        client.semantic_image_snapshot(requested),
        Err(SemanticImageAccessError::Superseded {
            requested: observed,
            retained: None,
        }) if observed == requested
    ));
    drop(client);
    drop(peer);
    fs::remove_dir_all(root).expect("last client joined the publisher before fixture cleanup");
}
