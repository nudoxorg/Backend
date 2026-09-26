//! Cross-file Rust field reads retarget through the defining module path.

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
    DecodedOccurrence, EntityKind, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SERVICE: &str = r#"pub struct Workout { pub note: u8 }
"#;

const LIB: &str = r#"mod service;
pub fn drive(item: service::Workout) { let _n = item.note; }
"#;

const LOCAL_LIB: &str = r#"pub struct Workout { pub note: u8 }
pub fn local(item: Workout) { let _n = item.note; }
"#;

const PATH_CALL_LIB: &str = r#"mod service;
pub fn drive() {
    let _f = service::set_note;
}
"#;

struct Lane<'a> {
    view: FragmentView<'a>,
    occurrences: Vec<DecodedOccurrence<'a>>,
}

fn project_root() -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-field-read-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"field_read_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
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
        b"rust-field-read-fixture",
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

fn package_field_reads<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::FieldAccess
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
fn cross_file_field_read_retargets_to_defining_module_path() -> Result<(), String> {
    let root = project_root()?;
    let bytes = compile_source(&root, "src/lib.rs", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let note_reads = package_field_reads(&lane)
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
                && key.path == "note"
                && key.display == "note"
                && key.kind == Some(EntityKind::Field)
        })
        .collect::<Vec<_>>();
    if note_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted note field read, got {}",
            note_reads.len()
        ));
    }

    let mention = note_reads[0];
    if mention.kind != ReferenceKind::FieldAccess {
        return Err("retargeted occurrence must be a field access".to_owned());
    }
    let OccurrenceTarget::Foreign(key) = mention.target else {
        return Err("retargeted field read must be foreign".to_owned());
    };
    if key.path.contains("::") || key.display.contains("::") {
        return Err("package key must use the name token note, not the qualified path".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::FieldAccess
                && std::ptr::eq(&row.occurrence, mention)
        })
        .ok_or("field read occurrence row absent")?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"drive" {
        return Err(format!(
            "field read must be owned by drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }

    let span_len = row.occurrence.span.end - row.occurrence.span.start;
    if span_len != 4 {
        return Err(format!(
            "field read span must cover note (4 bytes), not item.note, got {span_len}"
        ));
    }
    let drive_start = LIB
        .find("pub fn drive")
        .ok_or("drive function absent from fixture source")?;
    let drive_body = LIB.get(drive_start..).ok_or("drive body absent")?;
    let start = usize::try_from(row.occurrence.span.start).map_err(|_| "span start")?;
    let end = usize::try_from(row.occurrence.span.end).map_err(|_| "span end")?;
    let slice = drive_body
        .get(start..end)
        .ok_or("field read span out of bounds within drive")?;
    if slice != "note" {
        return Err(format!(
            "field read span bytes must be note, observed {:?}",
            slice
        ));
    }

    let stray_project_packages = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.name.starts_with("src/")
                && !(
                    lineage.name == "src/service"
                        && ((key.path == "note" && key.kind == Some(EntityKind::Field))
                            || (key.path == "Workout" && key.kind == Some(EntityKind::Record)))
                )
        })
        .count();
    if stray_project_packages > 0 {
        return Err("only note and Workout may retarget to src/service".to_owned());
    }

    Ok(())
}

#[test]
fn cross_file_field_read_from_mod_rs_retargets_to_module_path() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-field-read-mod-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src/service")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"field_read_mod_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service/mod.rs"), SERVICE).map_err(|error| error.to_string())?;
    let lib = "mod service;\npub fn drive(item: service::Workout) { let _n = item.note; }\n";
    fs::write(root.join("src/lib.rs"), lib).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", lib)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let read = package_field_reads(&lane)
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
                && key.path == "note"
                && key.display == "note"
        })
        .ok_or("oracle field read absent")?;
    if read.kind != ReferenceKind::FieldAccess {
        return Err("retargeted occurrence must be a field access".to_owned());
    }
    Ok(())
}

#[test]
fn same_file_field_read_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-field-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_field_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", LOCAL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let read = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .find(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::FieldAccess
                && matches!(occurrence.target, OccurrenceTarget::Local(_))
        })
        .ok_or("oracle local note field read absent")?;
    if matches!(
        read.target,
        OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
            origin: backend_semantic::ir::ForeignOrigin::Package(_),
            ..
        })
    ) {
        return Err("same-file note field read must not use a package key".to_owned());
    }
    Ok(())
}

#[test]
fn variable_read_does_not_retarget_to_package_field() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-variable-read-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"variable_read_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), "pub fn set_note() {}\n")
        .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), PATH_CALL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", PATH_CALL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    if package_field_reads(&lane)
        .into_iter()
        .any(|occurrence| {
            let OccurrenceTarget::Foreign(key) = occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(lineage) = key.origin else {
                return false;
            };
            lineage.name == "src/service" && key.kind == Some(EntityKind::Field)
        })
    {
        return Err("variable read must not retarget to a package field key".to_owned());
    }
    Ok(())
}
