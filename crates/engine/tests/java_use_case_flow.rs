//! Java generic-box use-case flow: two types in one harness compile, then IR lower.

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_java::legacy::{
    JavaRelease as HarnessRelease,
    harness::{Harness, HarnessError, HarnessRequest, JavaSource, JdkToolchain},
};
use backend_semantic::ir::{EntityKind, Ir, ItemKind};
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, Stage};

const BOX_SOURCE: &[u8] = b"package flow;\npublic class Box<T> { public T value; }\n";
const USE_SOURCE: &[u8] =
    b"package flow;\npublic class Use { public Box<String> take(Box<String> in) { return in; } }\n";
const MISSING_IMPORT_SOURCE: &[u8] = b"package flow;\nimport com.google.common.base.Preconditions;\npublic final class Bad { public String f(String x) { return Preconditions.checkNotNull(x); } }\n";

fn jdk_ready() -> Option<JdkToolchain<'static>> {
    let root = std::env::var_os("NUDOX_JDK")?;
    JdkToolchain::from_owned_root(root.into()).ok()
}

fn compile_flow(jdk: &JdkToolchain<'_>, harness: &mut Harness) -> Result<(Ir, Duration), String> {
    let sources = [
        JavaSource {
            name: Path::new("flow/Use.java"),
            bytes: USE_SOURCE,
        },
        JavaSource {
            name: Path::new("flow/Box.java"),
            bytes: BOX_SOURCE,
        },
    ];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image(jdk, request, &mut image)
        .map_err(|error| error.to_string())?;

    let tool = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"java-use-case-flow",
    )
    .map_err(|error| error.to_string())?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0_u8; 1 << 20];
    let work = std::env::temp_dir().join(format!("java-use-case-{}", std::process::id()));
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let compile_start = Instant::now();
    let compiled = compile_ir(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source: USE_SOURCE,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
    )
    .map_err(|failure| format!("{failure:?}"))?;
    Ok((compiled.ir, compile_start.elapsed()))
}

fn take_declares_parameter(ir: &Ir) -> bool {
    let take = ir
        .items()
        .find(|item| item.kind() == ItemKind::Function && item.name() == b"take");
    let Some(take) = take else {
        return false;
    };
    ir.items().any(|item| {
        item.kind() == ItemKind::Parameter
            && item.name() == b"in"
            && item.parent() == Some(take.id())
    })
}

#[test]
fn generic_box_flow() {
    let Some(jdk) = jdk_ready() else {
        use_case_support::skip("java", "generic-box", "jdk-missing")
            .expect("use-case bench report");
        return;
    };
    let mut harness = match Harness::new() {
        Ok(harness) => harness,
        Err(_) => {
            use_case_support::skip("java", "generic-box", "jdk-missing")
                .expect("use-case bench report");
            return;
        }
    };
    if harness.prepare(&jdk).is_err() {
        use_case_support::skip("java", "generic-box", "jdk-missing")
            .expect("use-case bench report");
        return;
    }

    let (ir, compile_elapsed) = match compile_flow(&jdk, &mut harness) {
        Ok(compiled) => compiled,
        Err(error) => panic!("generic box flow must compile and lower: {error}"),
    };

    assert_eq!(
        use_case_support::count_named(&ir, EntityKind::Record, b"flow.Box"),
        1,
        "Box must lower as a declared type"
    );
    assert_eq!(
        use_case_support::count_named(&ir, EntityKind::Record, b"flow.Use"),
        1,
        "Use must lower as a declared type"
    );
    assert!(
        take_declares_parameter(&ir),
        "take must declare its Box<String> parameter"
    );

    let sources = [JavaSource {
        name: Path::new("flow/Bad.java"),
        bytes: MISSING_IMPORT_SOURCE,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let mut output = Vec::new();
    let error = harness
        .image(&jdk, request, &mut output)
        .expect_err("a missing import must not seal as success");
    match error {
        HarnessError::UnresolvedDependencies { packages, stderr, .. } => {
            assert!(
                packages.contains("com.google.common.base"),
                "the missing package must be named: {packages:?}"
            );
            assert!(
                stderr.contains("not weakened"),
                "the compiler must stay at full strictness: {stderr}"
            );
        }
        other => panic!("missing dependency must be UnresolvedDependencies, not {other:?}"),
    }
    assert!(output.is_empty(), "no authority image on unresolved imports");

    use_case_support::finish("java", "generic-box", compile_elapsed, &ir)
        .expect("use-case bench report");
}
