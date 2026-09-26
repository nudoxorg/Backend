//! Cross-file Go const reads retarget through the module import path.

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

const SERVICE: &str = "package demo\n\nconst Limit = 1\n";

const LIB: &str = "package demo\n\nfunc Drive() { _ = Limit }\n";

const LOCAL_LIB: &str = "package demo\n\nconst Limit = 1\n\nfunc Local() { _ = Limit }\n";

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
        b"go-const-mention-fixture",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn project_root(service: &str, lib: &str) -> Result<std::path::PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-go-const-mention-{nonce}-{}-{}",
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

fn owner_name(lane: &Lane<'_>, owner: backend_semantic::ir::EntityId) -> Result<Vec<u8>, String> {
    let atom = lane
        .view
        .entities()
        .find(|entity| entity.entity.raw == owner.raw)
        .ok_or_else(|| format!("entity {} absent", owner.raw))?
        .name
        .raw;
    lane.view
        .atoms()
        .nth(atom as usize)
        .map(|atom| atom.bytes.to_vec())
        .ok_or_else(|| format!("entity {} name atom absent", owner.raw))
}

fn package_const_reads<'a>(
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
fn cross_file_const_read_retargets_to_module_import_path() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root(SERVICE, LIB)?;
    let bytes = compile_source(&root, "lib.go", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = package_const_reads(&lane)
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
                && key.path == "Limit"
                && key.display == "Limit"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted Limit const read, got {}",
            limit_reads.len()
        ));
    }

    let mention = limit_reads[0];
    let row = lane
        .occurrences
        .iter()
        .find(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::VariableUse
                && std::ptr::eq(&row.occurrence, mention)
        })
        .ok_or("const read occurrence row absent")?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Drive" {
        return Err(format!(
            "const read must be owned by Drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }

    Ok(())
}

#[test]
fn same_file_const_read_stays_local() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-go-same-file-const-{nonce}-{}-{}",
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
    let read = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .find(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::VariableUse
                && matches!(occurrence.target, OccurrenceTarget::Local(_))
        })
        .ok_or("oracle local Limit const read absent")?;
    if matches!(
        read.target,
        OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
            origin: backend_semantic::ir::ForeignOrigin::Package(_),
            ..
        })
    ) {
        return Err("same-file Limit const read must not use a package key".to_owned());
    }
    Ok(())
}
