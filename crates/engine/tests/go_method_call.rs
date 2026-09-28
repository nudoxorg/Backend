//! Cross-file Go method calls and method values retarget through the receiver namespace.

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
    DecodedOccurrence, EntityId, EntityKind, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{GoVersion, LanguageProfile, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const MODULE_PATH: &str = "example.com/demo";

const SERVICE_METHOD_ONLY: &str = "package demo\n\nfunc (Workout) SetNote() {}\n";

const LIB_TYPE_AND_CALL: &str =
    "package demo\n\ntype Workout struct{}\n\nfunc Drive(item Workout) { item.SetNote() }\n";

const LIB_METHOD_AND_CALL: &str =
    "package demo\n\nfunc (Workout) SetNote() {}\n\nfunc Drive(item Workout) { item.SetNote() }\n";

const SERVICE_TYPE_ONLY: &str = "package demo\n\ntype Workout struct{}\n";

const SERVICE: &str = "package demo\n\ntype Workout struct{}\n\nfunc (Workout) SetNote() {}\n";

const LIB_CALL: &str = "package demo\n\nfunc Drive(item Workout) { item.SetNote() }\n";

const LIB_VALUE: &str = "package demo\n\nfunc Drive(item Workout) { f := item.SetNote; _ = f }\n";

const LOCAL: &str = "package demo\n\ntype Workout struct{}\n\nfunc (Workout) SetNote() {}\n\nfunc Local(item Workout) { item.SetNote() }\n";

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
        b"go-method-call-fixture",
    )
    .expect("toolchain resolution is deterministic for the pinned tool identity")
}

fn project_root() -> Result<std::path::PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-go-method-call-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    fs::write(
        root.join("go.mod"),
        format!("module {MODULE_PATH}\n\ngo 1.22\n"),
    )
    .map_err(|error| error.to_string())?;
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

fn target_entity_name(
    lane: &Lane<'_>,
    target: EntityId,
) -> Result<Vec<u8>, String> {
    let atom = lane
        .view
        .entities()
        .find(|entity| entity.entity.raw == target.raw)
        .ok_or_else(|| format!("target entity {} absent", target.raw))?
        .name
        .raw;
    lane.view
        .atoms()
        .nth(atom as usize)
        .map(|atom| atom.bytes.to_vec())
        .ok_or_else(|| format!("target entity {} name atom absent", target.raw))
}

fn namespace_set_note_calls<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            if occurrence.confidence != OccurrenceConfidence::Oracle
                || occurrence.kind != ReferenceKind::MethodCall
            {
                return false;
            }
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Namespace {
                ecosystem,
                namespace,
            } = key.origin
            else {
                return false;
            };
            ecosystem == "go"
                && namespace == "Workout"
                && key.path == "SetNote"
                && key.display == "SetNote"
                && key.kind == Some(EntityKind::Function)
        })
        .collect()
}

fn local_set_note_method_calls<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .filter_map(|row| {
            if row.occurrence.confidence != OccurrenceConfidence::Oracle
                || row.occurrence.kind != ReferenceKind::MethodCall
            {
                return None;
            }
            let OccurrenceTarget::Local(target) = row.occurrence.target else {
                return None;
            };
            let target_name = target_entity_name(lane, target).ok()?;
            if target_name != b"SetNote" {
                return None;
            }
            Some(&row.occurrence)
        })
        .collect()
}

fn local_method_calls<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::MethodCall
                && matches!(occurrence.target, OccurrenceTarget::Local(_))
        })
        .collect()
}

#[test]
fn type_in_lib_method_in_service_call_is_namespace() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root()?;
    fs::write(root.join("service.go"), SERVICE_METHOD_ONLY)
        .map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), LIB_TYPE_AND_CALL).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "lib.go", LIB_TYPE_AND_CALL)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let namespace = namespace_set_note_calls(&lane);
    if namespace.len() != 1 {
        return Err(format!(
            "expected exactly one namespace SetNote method call, got {}",
            namespace.len()
        ));
    }
    if local_method_calls(&lane).len() != 0 {
        return Err("expected zero local MethodCall occurrences".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| std::ptr::eq(&row.occurrence, namespace[0]))
        .ok_or_else(|| "method call occurrence row absent".to_owned())?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Drive" {
        return Err(format!(
            "method call must be owned by Drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    Ok(())
}

#[test]
fn type_in_service_method_in_lib_call_is_local() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root()?;
    fs::write(root.join("service.go"), SERVICE_TYPE_ONLY).map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), LIB_METHOD_AND_CALL).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "lib.go", LIB_METHOD_AND_CALL)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let local = local_set_note_method_calls(&lane);
    if local.len() != 1 {
        return Err(format!(
            "expected exactly one local SetNote method call, got {}",
            local.len()
        ));
    }
    if namespace_set_note_calls(&lane).len() != 0 {
        return Err("expected zero namespace keys displayed SetNote".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| std::ptr::eq(&row.occurrence, local[0]))
        .ok_or_else(|| "local method call occurrence row absent".to_owned())?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Drive" {
        return Err(format!(
            "local method call must be owned by Drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    let OccurrenceTarget::Local(target) = row.occurrence.target else {
        return Err("local SetNote method call must stay local".to_owned());
    };
    let target_name = target_entity_name(&lane, target)?;
    if target_name != b"SetNote" {
        return Err(format!(
            "local method call must target SetNote, observed {:?}",
            core::str::from_utf8(&target_name).unwrap_or("?")
        ));
    }
    Ok(())
}

#[test]
fn cross_file_method_call_retargets_to_workout_namespace() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root()?;
    fs::write(root.join("service.go"), SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), LIB_CALL).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "lib.go", LIB_CALL)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let namespace = namespace_set_note_calls(&lane);
    if namespace.len() != 1 {
        return Err(format!(
            "expected exactly one namespace SetNote method call, got {}",
            namespace.len()
        ));
    }
    if local_method_calls(&lane).len() != 0 {
        return Err("expected zero local MethodCall occurrences".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| std::ptr::eq(&row.occurrence, namespace[0]))
        .ok_or_else(|| "method call occurrence row absent".to_owned())?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Drive" {
        return Err(format!(
            "method call must be owned by Drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    Ok(())
}

#[test]
fn cross_file_method_value_retargets_to_workout_namespace() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root()?;
    fs::write(root.join("service.go"), SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("lib.go"), LIB_VALUE).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "lib.go", LIB_VALUE)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let namespace = namespace_set_note_calls(&lane);
    if namespace.len() != 1 {
        return Err(format!(
            "expected exactly one namespace SetNote method value, got {}",
            namespace.len()
        ));
    }
    if local_method_calls(&lane).len() != 0 {
        return Err("expected zero local MethodCall occurrences".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| std::ptr::eq(&row.occurrence, namespace[0]))
        .ok_or_else(|| "method value occurrence row absent".to_owned())?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Drive" {
        return Err(format!(
            "method value must be owned by Drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    Ok(())
}

#[test]
fn same_file_method_call_stays_local() -> Result<(), String> {
    if !go_available() {
        eprintln!("skip: go compiler unavailable");
        return Ok(());
    }

    let root = project_root()?;
    fs::write(root.join("lib.go"), LOCAL).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "lib.go", LOCAL)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let local = local_set_note_method_calls(&lane);
    if local.len() != 1 {
        return Err(format!(
            "expected exactly one local SetNote method call, got {}",
            local.len()
        ));
    }
    if namespace_set_note_calls(&lane).len() != 0 {
        return Err("expected zero namespace keys displayed SetNote".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| std::ptr::eq(&row.occurrence, local[0]))
        .ok_or_else(|| "local method call occurrence row absent".to_owned())?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"Local" {
        return Err(format!(
            "same-file method call must be owned by Local, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    let OccurrenceTarget::Local(target) = row.occurrence.target else {
        return Err("same-file SetNote method call must stay local".to_owned());
    };
    let target_name = target_entity_name(&lane, target)?;
    if target_name != b"SetNote" {
        return Err(format!(
            "same-file method call must target SetNote, observed {:?}",
            core::str::from_utf8(&target_name).unwrap_or("?")
        ));
    }
    Ok(())
}
