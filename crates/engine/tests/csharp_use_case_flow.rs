//! C# explicit-interface event use-case flow: same-named events stay distinct in IR.

#[path = "use_case_support/mod.rs"]
mod use_case_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_csharp::legacy::DeclarationKind;
use backend_semantic::ir::{EntityKind, Ir};
use backend_semantic::vocabulary::{CSharpVersion, LanguageProfile, Stage};

const PROFILE: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);

/// Two explicit interface events share `IEventSymbol.Name`. A second type
/// references the host so both declarations stay live in one compile.
const EXPLICIT_INTERFACE_EVENTS: &[u8] = br#"
namespace FlowProbe;

public interface I1
{
    event System.Action Changed;
}

public interface I2
{
    event System.Action Changed;
}

public sealed class Host : I1, I2
{
    event System.Action I1.Changed { add { } remove { } }
    event System.Action I2.Changed { add { } remove { } }
}

public sealed class Subscriber
{
    public void Attach(Host host, I1 one, I2 two)
    {
        one.Changed += () => { };
        two.Changed += () => { };
    }
}
"#;

fn dotnet() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").as_deref().and_then(|paths| {
        std::env::split_paths(paths)
            .map(|dir| dir.join("dotnet"))
            .find(|candidate| candidate.is_file())
    })?;
    let status = Command::new(&path).arg("--version").status().ok()?;
    status.success().then_some(path)
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "nudox-csharp-use-case-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("fixture directory");
    path
}

fn published_oracle(dotnet: &Path) -> Option<&'static PathBuf> {
    static ORACLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ORACLE
        .get_or_init(|| {
            let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../frontends/csharp/src/legacy/helper");
            let root = fresh_dir("publish");
            let publish = root.join("publish");
            fs::create_dir_all(&publish).ok()?;
            let intermediate = publish.join("obj");
            let mut intermediate_arg = std::ffi::OsString::from("-p:BaseIntermediateOutputPath=");
            intermediate_arg.push(&intermediate);
            intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
            let output_base = publish.join("bin");
            let mut output_arg = std::ffi::OsString::from("-p:BaseOutputPath=");
            output_arg.push(&output_base);
            output_arg.push(std::path::MAIN_SEPARATOR.to_string());
            let status = Command::new(dotnet)
                .args([
                    "publish",
                    "oracle.csproj",
                    "-c",
                    "Release",
                    "--nologo",
                    "-p:UseSharedCompilation=false",
                ])
                .arg(&intermediate_arg)
                .arg(&output_arg)
                .arg("-o")
                .arg(&publish)
                .current_dir(&helper)
                .status()
                .ok()?;
            (status.success() && publish.join("oracle.dll").is_file())
                .then_some(publish.join("oracle.dll"))
        })
        .as_ref()
}

fn oracle_image(dotnet: &Path, source: &[u8]) -> Option<Vec<u8>> {
    let oracle = published_oracle(dotnet)?;
    let root = fresh_dir("oracle");
    let source_root = root.join("root");
    fs::create_dir_all(&source_root).ok()?;
    let binding = source_root.join("Package.cs");
    fs::write(&binding, source).ok()?;
    let image = root.join("authority.image");
    let status = Command::new(dotnet)
        .arg("exec")
        .arg(oracle)
        .arg("--mode")
        .arg("source")
        .arg("--assembly-name")
        .arg("FlowProbe")
        .arg("--authority-image")
        .arg("--source-binding")
        .arg(&binding)
        .arg("--out")
        .arg(&image)
        .arg("--root")
        .arg(&source_root)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    fs::read(&image).ok()
}

fn compile_flow(dotnet: &Path, source: &[u8]) -> Result<(Ir, Duration), String> {
    let image = oracle_image(dotnet, source).ok_or_else(|| "Roslyn authority image".to_owned())?;
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).map_err(|error| error.to_string())?;
    let changed: Vec<_> = authority
        .declarations()
        .filter_map(|declared| declared.ok())
        .filter(|declared| declared.kind == DeclarationKind::Event)
        .filter(|declared| declared.name.bytes == b"Changed")
        .collect();
    if changed.len() != 2 {
        return Err(format!(
            "authority must carry both explicit Changed events, found {changed:?}"
        ));
    }

    let toolchain = ResolvedToolchain::from_version(
        NativeTool::CSharpCompiler,
        dotnet,
        b"csharp-use-case-flow",
    )
    .map_err(|error| error.to_string())?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let work = fresh_dir("lower");
    let compile_start = Instant::now();
    let compiled = compile_ir(
        CompileRequest {
            profile: PROFILE,
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::CSharp { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
    )
    .map_err(|error| format!("{error:?}"))?;
    let _ = fs::remove_dir_all(&work);
    Ok((compiled.ir, compile_start.elapsed()))
}

#[test]
fn explicit_events_flow() {
    let Some(dotnet) = dotnet() else {
        use_case_support::skip("csharp", "explicit-events", "dotnet-missing")
            .expect("use-case bench report");
        return;
    };

    let (ir, compile_elapsed) = match compile_flow(&dotnet, EXPLICIT_INTERFACE_EVENTS) {
        Ok(compiled) => compiled,
        Err(error) => panic!("explicit interface events must lower: {error}"),
    };

    let changed: Vec<_> = ir
        .items()
        .filter(|item| item.kind() == EntityKind::Field && item.name() == b"Changed")
        .collect();
    assert_eq!(
        changed.len(),
        2,
        "both explicit Changed events must survive lowering"
    );
    assert_ne!(
        changed[0].version().identity(),
        changed[1].version().identity(),
        "same-named explicit events must keep distinct declaration identities"
    );
    assert_eq!(
        use_case_support::count_named(&ir, EntityKind::Record, b"Host"),
        1,
        "the event host type must lower"
    );
    assert_eq!(
        use_case_support::count_named(&ir, EntityKind::Record, b"Subscriber"),
        1,
        "the consumer type must lower"
    );

    use_case_support::finish("csharp", "explicit-events", compile_elapsed, &ir)
        .expect("use-case bench report");
}
