//! Cross-file Rust const, static, and enum-variant value reads retarget through
//! the defining module path.

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

const SERVICE: &str = r#"pub const LIMIT: u8 = 1;
"#;

const LIB: &str = r#"mod service;
pub fn drive() { let _n = service::LIMIT; }
"#;

const LOCAL_LIB: &str = r#"pub const LIMIT: u8 = 1;
pub fn local() { let _n = LIMIT; }
"#;

const VARIANT_SERVICE: &str = r#"pub enum Color { Red }
"#;

const VARIANT_LIB: &str = r#"mod service;
pub fn drive() { let _c = service::Color::Red; }
"#;

const ASSOC_SERVICE: &str = r#"pub struct Workout;
impl Workout {
    pub const LIMIT: i32 = 1;
}
"#;

const ASSOC_LIB: &str = r#"mod service;
pub fn drive() { let _n = service::Workout::LIMIT; }
"#;

const ASSOC_LOCAL_LIB: &str = r#"pub struct Workout;
impl Workout {
    pub const LIMIT: i32 = 1;
}
pub fn local() { let _n = Workout::LIMIT; }
"#;

const DUAL_VALUE_SERVICE: &str = r#"pub const LIMIT: u8 = 1;
pub struct Workout;
impl Workout {
    pub const LIMIT: i32 = 2;
}
"#;

const DUAL_VALUE_LIB: &str = r#"mod service;
pub fn drive() {
    let _free = service::LIMIT;
    let _assoc = service::Workout::LIMIT;
}
"#;

const COLLISION_SERVICE: &str = r#"pub struct Workout;
impl Workout {
    pub const LIMIT: i32 = 1;
}
pub struct Session;
impl Session {
    pub const LIMIT: i32 = 2;
}
"#;

const COLLISION_LIB: &str = r#"mod service;
pub fn drive() {
    let _workout = service::Workout::LIMIT;
    let _session = service::Session::LIMIT;
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
        "nudox-rust-value-read-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"value_read_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
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
        b"rust-value-read-fixture",
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

fn package_value_reads<'a>(
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
fn cross_file_const_read_retargets_to_defining_module_path() -> Result<(), String> {
    let root = project_root()?;
    let bytes = compile_source(&root, "src/lib.rs", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = package_value_reads(&lane)
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
                && key.path == "LIMIT"
                && key.display == "LIMIT"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted LIMIT value read, got {}",
            limit_reads.len()
        ));
    }

    let mention = limit_reads[0];
    if mention.kind != ReferenceKind::VariableUse {
        return Err("retargeted occurrence must be a variable use".to_owned());
    }
    let OccurrenceTarget::Foreign(key) = mention.target else {
        return Err("retargeted value read must be foreign".to_owned());
    };
    if key.path.contains("::") || key.display.contains("::") {
        return Err("package key must use the name token LIMIT, not the qualified path".to_owned());
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::VariableUse
                && std::ptr::eq(&row.occurrence, mention)
        })
        .ok_or("value read occurrence row absent")?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"drive" {
        return Err(format!(
            "value read must be owned by drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }

    let span_len = row.occurrence.span.end - row.occurrence.span.start;
    if span_len != 5 {
        return Err(format!(
            "value read span must cover LIMIT (5 bytes), not service::LIMIT, got {span_len}"
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
        .ok_or("value read span out of bounds within drive")?;
    if slice != "LIMIT" {
        return Err(format!(
            "value read span bytes must be LIMIT, observed {:?}",
            slice
        ));
    }

    Ok(())
}

#[test]
fn same_file_const_read_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
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
                && occurrence.kind == ReferenceKind::VariableUse
                && matches!(occurrence.target, OccurrenceTarget::Local(_))
        })
        .ok_or("oracle local LIMIT value read absent")?;
    if matches!(
        read.target,
        OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
            origin: backend_semantic::ir::ForeignOrigin::Package(_),
            ..
        })
    ) {
        return Err("same-file LIMIT value read must not use a package key".to_owned());
    }
    Ok(())
}

const PATH_VALUE_SERVICE: &str = r#"pub fn set_note() {}
"#;

const PATH_VALUE_CALL_LIB: &str = r#"mod service;
pub fn drive() {
    let _f = service::set_note;
    service::set_note();
}
"#;

const PATH_VALUE_LOCAL_LIB: &str = r#"pub fn set_note() {}
pub fn local() { let _f = set_note; }
"#;

#[test]
fn cross_file_function_value_read_retargets_to_defining_module_path() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-function-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"function_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), PATH_VALUE_SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), PATH_VALUE_CALL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", PATH_VALUE_CALL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let value_reads = package_value_reads(&lane)
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
                && key.kind == Some(EntityKind::Function)
        })
        .collect::<Vec<_>>();
    if value_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted set_note function value read, got {}",
            value_reads.len()
        ));
    }

    let calls = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && occurrence.kind == ReferenceKind::FunctionCall
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
            lineage.ecosystem == "cargo"
                && lineage.name == "src/service"
                && key.path == "set_note"
                && key.display == "set_note"
                && key.kind == Some(EntityKind::Function)
        })
        .collect::<Vec<_>>();
    if calls.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted set_note function call, got {}",
            calls.len()
        ));
    }

    let mention = value_reads[0];
    if mention.kind != ReferenceKind::VariableUse {
        return Err("retargeted occurrence must be a variable use".to_owned());
    }
    let OccurrenceTarget::Foreign(key) = mention.target else {
        return Err("retargeted function value read must be foreign".to_owned());
    };
    if key.path.contains("::") || key.display.contains("::") {
        return Err("package key must use the name token set_note, not service::set_note".to_owned());
    }
    Ok(())
}

#[test]
fn same_file_function_value_read_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-function-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_function_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), PATH_VALUE_LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", PATH_VALUE_LOCAL_LIB)?;
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
            key.path == "set_note" || key.display == "set_note"
        })
        .count();
    if package_set_note_keys != 0 {
        return Err(format!(
            "expected zero package set_note keys for same-file function value, got {package_set_note_keys}"
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
fn cross_file_variant_read_retargets_name_token_only() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-variant-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"variant_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), VARIANT_SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), VARIANT_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", VARIANT_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let red_reads = package_value_reads(&lane)
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
                && key.path == "Red"
                && key.display == "Red"
                && key.kind == Some(EntityKind::Variant)
        })
        .collect::<Vec<_>>();
    if red_reads.is_empty() {
        return Err(
            "rust-analyzer did not resolve service::Color::Red to a variant; skipping variant pin"
                .to_owned(),
        );
    }
    if red_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted Red variant read, got {}",
            red_reads.len()
        ));
    }
    let mention = red_reads[0];
    if mention.kind != ReferenceKind::VariableUse {
        return Err("retargeted occurrence must be a variable use".to_owned());
    }
    let OccurrenceTarget::Foreign(key) = mention.target else {
        return Err("retargeted variant read must be foreign".to_owned());
    };
    if key.path.contains("::") || key.display.contains("::") {
        return Err("package key must use the name token Red, not Color::Red".to_owned());
    }
    Ok(())
}

#[test]
fn cross_file_associated_const_read_retargets_to_defining_module_path() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-assoc-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"assoc_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), ASSOC_SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), ASSOC_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", ASSOC_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = package_value_reads(&lane)
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
                && key.path == "LIMIT"
                && key.display == "LIMIT"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 1 {
        return Err(format!(
            "expected exactly one retargeted associated LIMIT value read, got {}",
            limit_reads.len()
        ));
    }

    let mention = limit_reads[0];
    if mention.kind != ReferenceKind::VariableUse {
        return Err("retargeted occurrence must be a variable use".to_owned());
    }
    let OccurrenceTarget::Foreign(key) = mention.target else {
        return Err("retargeted value read must be foreign".to_owned());
    };
    if key.path.contains("::") || key.display.contains("::") {
        return Err(
            "package key must use the name token LIMIT, not Workout::LIMIT or service::Workout::LIMIT"
                .to_owned(),
        );
    }

    let row = lane
        .occurrences
        .iter()
        .find(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::VariableUse
                && std::ptr::eq(&row.occurrence, mention)
        })
        .ok_or("value read occurrence row absent")?;
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"drive" {
        return Err(format!(
            "value read must be owned by drive, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }

    let span_len = row.occurrence.span.end - row.occurrence.span.start;
    if span_len != 5 {
        return Err(format!(
            "value read span must cover LIMIT (5 bytes), not Workout::LIMIT, got {span_len}"
        ));
    }
    Ok(())
}

#[test]
fn same_file_associated_const_read_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-assoc-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_assoc_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), ASSOC_LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", ASSOC_LOCAL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let package_limit_keys = lane
        .occurrences
        .iter()
        .filter(|row| {
            let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
                return false;
            };
            let backend_semantic::ir::ForeignOrigin::Package(_) = key.origin else {
                return false;
            };
            key.path == "LIMIT" || key.display == "LIMIT"
        })
        .count();
    if package_limit_keys != 0 {
        return Err(format!(
            "expected zero package LIMIT keys for same-file associated const, got {package_limit_keys}"
        ));
    }

    let local_start = ASSOC_LOCAL_LIB
        .find("pub fn local")
        .ok_or("local function absent from fixture source")?;
    let local_body = ASSOC_LOCAL_LIB
        .get(local_start..)
        .ok_or("local body absent")?;

    let limit_local_reads: Vec<_> = lane
        .occurrences
        .iter()
        .filter(|row| {
            if row.occurrence.confidence != OccurrenceConfidence::Oracle
                || row.occurrence.kind != ReferenceKind::VariableUse
                || !matches!(row.occurrence.target, OccurrenceTarget::Local(_))
            {
                return false;
            }
            let span_len = row.occurrence.span.end - row.occurrence.span.start;
            if span_len != 5 {
                return false;
            }
            let Ok(start) = usize::try_from(row.occurrence.span.start) else {
                return false;
            };
            let Ok(end) = usize::try_from(row.occurrence.span.end) else {
                return false;
            };
            local_body.get(start..end) == Some("LIMIT")
        })
        .collect();
    if limit_local_reads.len() != 1 {
        return Err(format!(
            "expected exactly one oracle local LIMIT value read, got {}",
            limit_local_reads.len()
        ));
    }

    let row = limit_local_reads[0];
    let owner = owner_name(&lane, row.owner)?;
    if owner != b"local" {
        return Err(format!(
            "LIMIT value read must be owned by local, observed {:?}",
            core::str::from_utf8(&owner).unwrap_or("?")
        ));
    }
    Ok(())
}

#[test]
fn free_const_and_associated_const_reads_stay_distinct() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-dual-value-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"dual_value_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), DUAL_VALUE_SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), DUAL_VALUE_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", DUAL_VALUE_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = package_value_reads(&lane)
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
                && key.path == "LIMIT"
                && key.display == "LIMIT"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 2 {
        return Err(format!(
            "expected two distinct retargeted LIMIT value reads (free and associated), got {}",
            limit_reads.len()
        ));
    }
    Ok(())
}

#[test]
fn two_associated_consts_same_name_stay_distinct() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-assoc-collision-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"assoc_collision_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), COLLISION_SERVICE).map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), COLLISION_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", COLLISION_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let limit_reads = package_value_reads(&lane)
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
                && key.path == "LIMIT"
                && key.display == "LIMIT"
                && key.kind == Some(EntityKind::Constant)
        })
        .collect::<Vec<_>>();
    if limit_reads.len() != 2 {
        return Err(format!(
            "expected two distinct retargeted associated LIMIT value reads, got {}",
            limit_reads.len()
        ));
    }
    Ok(())
}
