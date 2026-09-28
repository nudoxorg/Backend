//! Cross-file Go function value reads retarget through the module import path.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_go::legacy::GoOracle;
use backend_semantic::ir::{
    DecodedOccurrence, EntityKind, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const MODULE_PATH: &str = "example.com/demo";

const SERVICE: &str = "package demo\n\nfunc SetNote() {}\n";

const LIB: &str = "package demo\n\nfunc Drive() { f := SetNote; _ = f; SetNote() }\n";

const LOCAL_LIB: &str =
    "package demo\n\nfunc SetNote() {}\n\nfunc Local() { f := SetNote; _ = f }\n";

const CONST_SERVICE: &str = "package demo\n\nconst Limit = 1\n";

const CONST_LIB: &str = "package demo\n\nfunc Drive() { _ = Limit }\n";

const VAR_SERVICE: &str = "package demo\n\nvar Limit = 1\n";

const VAR_LIB: &str = "package demo\n\nfunc Drive() { _ = Limit }\n";

struct Lane<'a> {
    view: FragmentView<'a>,
    occurrences: Vec<DecodedOccurrence<'a>>,
}

fn go_available() -> bool {
    let compiler = std::env::var("COMPILER_GO_COMPILER").unwrap_or_else(|_| "go".to_owned());
    Command::new(compiler)
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn toolchain() -> ResolvedToolchain<'static> {
    ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/bin/true"),
        b"go-fn-value-fixture",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn project_root(service: &str, lib: &str) -> Result<std::path::PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-go-fn-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(
        root.join("go.mod"),
        format!("module {MODULE_PATH}\n\ngo 1.22\n"),
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("service.go"), service).map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), lib).map_err(|error| error.to_string())?;
    Ok(root)
}

fn compile_source(root: &Path, relative: &str, source: &str) -> Result<Vec<u8>, String> {
    let source_path = root.join(relative);
    fs::write(&source_path, source).map_err(|error| error.to_string())?;
    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(300),
    };
    let image_bytes = oracle
        .authority_image_for_package(&source_path, root)
        .map_err(|error| format!("oracle: {error:?}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    let mut output = vec![0_u8; 262_144];
    compile(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source: source.as_bytes(),
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain()),
            authority: SemanticAuthorityInput::Go {
                image: &image_bytes,
            },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(300),
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
    Ok(Lane { view, occurrences })
}

fn package_function_value_reads<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::VariableUse
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
fn cross_file_function_value_read_retargets_to_module_import_path() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root(SERVICE, LIB)?;
    let bytes = compile_source(&root, "lib.go", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let set_note_reads = package_function_value_reads(&lane)
        .into_iter()
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "go"
                && lineage.name == MODULE_PATH
                && key.path == "SetNote"
                && key.display == "SetNote"
                && key.kind == Some(EntityKind::Function)
        })
        .collect::<Vec<_>>();
    if set_note_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted SetNote function value read, got {}",
            set_note_reads.len()
        ));
    }

    let calls = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::FunctionCall
        })
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "go"
                && lineage.name == MODULE_PATH
                && key.path == "SetNote"
                && key.display == "SetNote"
                && key.kind == Some(EntityKind::Function)
        })
        .collect::<Vec<_>>();
    if calls.len() != 1 {
        return Err(format!(
            "expected exactly one package SetNote function call, got {}",
            calls.len()
        ));
    }
    if matches!(calls[0].target, OccurrenceTarget::Local(_)) {
        return Err("cross-file SetNote call must not resolve locally".to_owned());
    }

    let local_set_note_calls = lane
        .occurrences
        .iter()
        .filter(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::FunctionCall
                && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
        })
        .count();
    if local_set_note_calls != 0 {
        return Err(format!(
            "expected zero local SetNote function calls, got {local_set_note_calls}"
        ));
    }

    let value_reads = package_function_value_reads(&lane).len();
    let call_reads = calls.len();
    if value_reads != 1 || call_reads != 1 {
        return Err(format!(
            "expected one pinned package value read and one pinned package call, got {value_reads} value reads and {call_reads} calls"
        ));
    }

    Ok(())
}

#[test]
fn same_file_function_value_read_stays_local() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-go-same-file-fn-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(
        root.join("go.mod"),
        format!("module {MODULE_PATH}\n\ngo 1.22\n"),
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = match compile_source(&root, "lib.go", LOCAL_LIB) {
        Ok(bytes) => bytes,
        Err(error) if error.contains("ToolingUnavailable") => {
            let _ = fs::remove_dir_all(&root);
            eprintln!("skip: go compiler unavailable");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let package_set_note_keys = lane
        .occurrences
        .iter()
        .filter(|row| {
            let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(_) = key.origin else {
                return false;
            };
            key.path == "SetNote" || key.display == "SetNote"
        })
        .count();
    if package_set_note_keys != 0 {
        return Err(format!(
            "expected zero package SetNote keys for same-file function value, got {package_set_note_keys}"
        ));
    }

    let local_reads = lane
        .occurrences
        .iter()
        .filter(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::VariableUse
                && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
        })
        .count();
    if local_reads != 1 {
        return Err(format!(
            "expected exactly one oracle local function value read, got {local_reads}"
        ));
    }
    Ok(())
}

#[test]
fn cross_file_const_value_stays_constant_not_function() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root(CONST_SERVICE, CONST_LIB)?;
    let bytes = compile_source(&root, "lib.go", CONST_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::VariableUse
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: backend_semantic::ir::ForeignOrigin::Package(_),
                        ..
                    })
                )
        })
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "go"
                && lineage.name == MODULE_PATH
                && key.path == "Limit"
                && key.display == "Limit"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted Limit const value read, got {}",
            limit_reads.len()
        ));
    }
    Ok(())
}

#[test]
fn cross_file_var_value_stays_static_not_function() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root(VAR_SERVICE, VAR_LIB)?;
    let bytes = compile_source(&root, "lib.go", VAR_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::VariableUse
                && matches!(
                    occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: backend_semantic::ir::ForeignOrigin::Package(_),
                        ..
                    })
                )
        })
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.ecosystem == "go"
                && lineage.name == MODULE_PATH
                && key.path == "Limit"
                && key.display == "Limit"
                && key.kind == Some(EntityKind::Static)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted Limit var value read, got {}",
            limit_reads.len()
        ));
    }
    Ok(())
}
