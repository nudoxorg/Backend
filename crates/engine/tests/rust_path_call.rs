//! Cross-file Rust path calls retarget through the defining module path.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_rust::legacy::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use backend_semantic::ir::{
    DecodedOccurrence, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SERVICE: &str = r#"pub fn set_note() {}
"#;

const LIB: &str = r#"mod service;
pub fn drive() {
    service::set_note();
    crate::service::set_note();
    vanish();
    let _f = service::set_note;
    std::mem::drop(1u8);
}
"#;

const LOCAL_LIB: &str = r#"pub fn set_note() {}
pub fn local() { set_note(); }
"#;

struct Lane<'a> {
    occurrences: Vec<DecodedOccurrence<'a>>,
}

fn project_root() -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-path-call-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"path_call_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), LIB).map_err(|error| error.to_string())?;
    Ok(root)
}

fn rustc() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    std::env::var_os("PATH")
        .ok_or_else(|| "PATH has no rustc".to_owned())?
        .to_str()
        .ok_or_else(|| "PATH is not UTF-8".to_owned())?
        .split(':')
        .map(PathBuf::from)
        .map(|directory| directory.join("rustc"))
        .find_map(|path| path.is_file().then(|| path.canonicalize().ok()).flatten())
        .ok_or_else(|| "rustc absent".to_owned())
}

fn compile_source(root: &PathBuf, relative: &str, source: &str) -> Result<Vec<u8>, String> {
    let source_path = root.join(relative);
    let tool = rustc()?;
    let toolchain =
        RustToolchain::discover(&tool).map_err(|error| format!("toolchain: {error:?}"))?;
    let project = RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)
        .map_err(|error| format!("project: {error:?}"))?;
    let resolved = ResolvedToolchain::from_version(
        backend_engine::driver::NativeTool::Rustc,
        &tool,
        b"rust-path-call-fixture",
    )
    .map_err(|error| format!("tool: {error:?}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut output = vec![0_u8; 262_144];
    compile(
        CompileRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
            source: source.as_bytes(),
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(resolved),
            authority: SemanticAuthorityInput::Rust {
                project: &project,
                maximum_source_bytes: SourceByteLimit::from(65_536),
                features: RustFeatureControl::default(),
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: root,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|error| format!("compile: {error:?}"))?;
    let length = output
        .get(8..12)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "header truncated".to_owned())? as usize;
    output
        .get(..length)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "declared length exceeds output".to_owned())
}

fn lane(bytes: &[u8]) -> Result<Lane<'_>, String> {
    let view = FragmentView::validate(bytes).map_err(|error| format!("validate: {error:?}"))?;
    let occurrences = view
        .occurrences()
        .map(|cursor| {
            cursor
                .map(|row| row.map_err(|error| format!("occurrence: {error:?}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Lane { occurrences })
}

fn function_calls<'a>(
    lane: &'a Lane<'a>,
) -> impl Iterator<Item = &'a backend_semantic::ir::Occurrence<'a>> + 'a {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| occurrence.kind == ReferenceKind::FunctionCall)
}

fn package_function_calls<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    function_calls(lane)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: backend_semantic::ir::ForeignOrigin::Package(_),
                        ..
                    })
                )
        })
        .collect()
}

#[test]
fn cross_file_path_call_retargets_to_defining_module_path() -> Result<(), String> {
    let root = project_root()?;
    let bytes = compile_source(&root, "src/lib.rs", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let retargeted = package_function_calls(&lane)
        .into_iter()
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "cargo"
                && lineage.name == "src/service"
                && key.path == "set_note"
                && key.display == "set_note"
        })
        .collect::<Vec<_>>();
    if retargeted.len() != 2 {
        return Err(format!(
            "expected two retargeted path calls, got {}",
            retargeted.len()
        ));
    }

    let vanish = function_calls(&lane)
        .find(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            matches!(
                key.origin,
                backend_semantic::ir::ForeignOrigin::Universe { ecosystem: "cargo" }
            ) && key.path == "vanish" && key.display == "vanish"
        })
        .ok_or("vanish call absent")?;
    if vanish.confidence != OccurrenceConfidence::Syntactic {
        return Err("vanish call must stay syntactic".to_owned());
    }

    let value_use = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .find(|occurrence| occurrence.kind == ReferenceKind::VariableUse)
        .ok_or("value use absent")?;
    let OccurrenceTarget::Foreign(key) = value_use.target else {
        return Err("value use must stay foreign".to_owned());
    };
    if matches!(key.origin, backend_semantic::ir::ForeignOrigin::Package(_)) {
        return Err("value use must not retarget to src/service".to_owned());
    }

    let std_drop = function_calls(&lane)
        .find(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            matches!(
                key.origin,
                backend_semantic::ir::ForeignOrigin::Universe { ecosystem: "cargo" }
            ) && key.path == "std::mem::drop" && key.display == "std::mem::drop"
        })
        .ok_or("std::mem::drop call absent")?;
    if std_drop.confidence != OccurrenceConfidence::Oracle {
        return Err("std::mem::drop call must stay oracle".to_owned());
    }

    Ok(())
}

#[test]
fn cross_file_path_call_from_mod_rs_retargets_to_module_path() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-path-call-mod-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src/service")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"path_call_mod_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service/mod.rs"), SERVICE).map_err(|error| error.to_string())?;
    let lib = "mod service;\npub fn drive() { service::set_note(); }\n";
    fs::write(root.join("src/lib.rs"), lib).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", lib)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let call = package_function_calls(&lane)
        .into_iter()
        .find(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "cargo"
                && lineage.name == "src/service"
                && key.path == "set_note"
                && key.display == "set_note"
        })
        .ok_or("oracle path call absent")?;
    if call.kind != ReferenceKind::FunctionCall {
        return Err("retargeted occurrence must be a function call".to_owned());
    }
    Ok(())
}

#[test]
fn same_file_path_call_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-path-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_path_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", LOCAL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let call = function_calls(&lane)
        .find(|occurrence| occurrence.confidence == OccurrenceConfidence::Oracle)
        .ok_or("oracle path call absent")?;
    if !matches!(call.target, OccurrenceTarget::Local(_)) {
        return Err("same-file path call must stay local".to_owned());
    }
    Ok(())
}
