//! Cross-file Rust type mentions retarget through the defining module path.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use backend_frontend_rust::legacy::{RustFeatureControl, RustProject, RustToolchain, SourceByteLimit};
use backend_semantic::ir::{
    DecodedOccurrence, EntityId, EntityKind, FragmentView, OccurrenceConfidence, OccurrenceTarget,
    ParentageAuthority, ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SERVICE: &str = r#"pub struct Workout { pub note: u8 }
"#;

const LIB: &str = r#"mod service;
pub fn drive(item: service::Workout) -> service::Workout { item }
use service::Workout;
pub fn local() { let _x: String = String::new(); }
"#;

const LOCAL_LIB: &str = r#"pub struct Workout { pub note: u8 }
pub fn local(item: Workout) -> Workout { item }
"#;

const PATH_CALL_LIB: &str = r#"mod service;
pub fn drive() {
    service::set_note();
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
        "nudox-rust-type-mention-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"type_mention_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
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
        b"rust-type-mention-fixture",
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

fn compile_ir_source(root: &PathBuf, relative: &str, source: &str) -> Result<backend_engine::driver::CompiledIr, String> {
    let source_path = root.join(relative);
    let tool = rustc()?;
    let toolchain =
        RustToolchain::discover(&tool).map_err(|error| format!("toolchain: {error:?}"))?;
    let project = RustProject::open_with_source(root, &source_path, &toolchain, RustEdition::Rust2024)
        .map_err(|error| format!("project: {error:?}"))?;
    let resolved = ResolvedToolchain::from_version(
        backend_engine::driver::NativeTool::Rustc,
        &tool,
        b"rust-type-mention-fixture",
    )
    .map_err(|error| format!("tool: {error:?}"))?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 0];
    compile_ir(
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
    )
    .map_err(|error| format!("compile_ir: {error:?}"))
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

fn entity_name(view: &FragmentView<'_>, ordinal: u32) -> Result<Vec<u8>, String> {
    let atom = view
        .entities()
        .find(|entity| entity.entity.raw == ordinal)
        .ok_or_else(|| format!("entity {ordinal} absent"))?
        .name
        .raw;
    view.atoms()
        .nth(atom as usize)
        .map(|atom| atom.bytes.to_vec())
        .ok_or_else(|| format!("entity {ordinal} name atom absent"))
}

fn entity_ordinal(view: &FragmentView<'_>, name: &[u8], kind: EntityKind) -> Result<u32, String> {
    for entity in view.entities() {
        if entity.kind != kind {
            continue;
        }
        let atom = view
            .atoms()
            .nth(entity.name.raw as usize)
            .ok_or("entity name atom absent")?;
        if atom.bytes == name {
            return Ok(entity.entity.raw);
        }
    }
    Err(format!(
        "entity {:?}/{} absent",
        kind,
        core::str::from_utf8(name).unwrap_or("?")
    ))
}

fn owner_name(lane: &Lane<'_>, owner: EntityId) -> Result<Vec<u8>, String> {
    entity_name(&lane.view, owner.raw)
}

fn package_type_mentions<'a>(
    lane: &'a Lane<'a>,
) -> Vec<&'a backend_semantic::ir::Occurrence<'a>> {
    lane.occurrences
        .iter()
        .map(|row| &row.occurrence)
        .filter(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    occurrence.kind,
                    ReferenceKind::TypeReference | ReferenceKind::Import
                )
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
fn cross_file_type_mention_retargets_to_defining_module_path() -> Result<(), String> {
    let root = project_root()?;
    let bytes = compile_source(&root, "src/lib.rs", LIB)?;
    let compiled = compile_ir_source(&root, "src/lib.rs", LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;

    let workout_mentions = package_type_mentions(&lane)
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
                && key.path == "Workout"
                && key.display == "Workout"
                && key.kind == Some(EntityKind::Record)
        })
        .collect::<Vec<_>>();
    if workout_mentions.is_empty() {
        return Err("expected at least one retargeted Workout type mention".to_owned());
    }

    let import_row = lane
        .occurrences
        .iter()
        .find(|row| {
            row.occurrence.confidence == OccurrenceConfidence::Oracle
                && row.occurrence.kind == ReferenceKind::Import
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        path: "Workout",
                        display: "Workout",
                        kind: Some(EntityKind::Record),
                        origin: backend_semantic::ir::ForeignOrigin::Package(lineage),
                        ..
                    }) if lineage.ecosystem == "cargo" && lineage.name == "src/service"
                )
        })
        .ok_or("use service::Workout must emit an oracle package Import")?;
    let import_owner = owner_name(&lane, import_row.owner)?;
    if import_owner != b"crate" {
        return Err(format!(
            "Import owner must be crate, observed {:?}",
            core::str::from_utf8(&import_owner).unwrap_or("?")
        ));
    }

    if lane.view.entities().any(|entity| {
        entity.kind == EntityKind::Reexport
            && lane
                .view
                .atoms()
                .nth(entity.name.raw as usize)
                .is_some_and(|atom| atom.bytes == b"Workout")
    }) {
        return Err("private use must not mint a Reexport named Workout".to_owned());
    }

    let signature_mentions = workout_mentions
        .iter()
        .filter(|mention| mention.kind == ReferenceKind::TypeReference)
        .collect::<Vec<_>>();
    if signature_mentions.is_empty() {
        return Err("expected a signature TypeReference for Workout".to_owned());
    }
    for mention in signature_mentions {
        let OccurrenceTarget::Foreign(key) = mention.target else {
            continue;
        };
        if key.path.contains("::") || key.display.contains("::") {
            return Err("package key must use the name token Workout, not the qualified path".to_owned());
        }
        let row = lane
            .occurrences
            .iter()
            .find(|row| {
                row.occurrence.confidence == OccurrenceConfidence::Oracle
                    && row.occurrence.kind == ReferenceKind::TypeReference
                    && std::ptr::eq(&row.occurrence, *mention)
            })
            .ok_or("signature occurrence row absent")?;
        let owner = owner_name(&lane, row.owner)?;
        if owner == b"crate" {
            return Err("signature TypeReference must be owned by drive, not crate".to_owned());
        }
        if owner != b"drive" {
            return Err(format!(
                "signature TypeReference must be owned by drive, observed {:?}",
                core::str::from_utf8(&owner).unwrap_or("?")
            ));
        }
    }

    let drive = entity_ordinal(&lane.view, b"drive", EntityKind::Function)?;
    let _crate_module = entity_ordinal(&lane.view, b"crate", EntityKind::Module)?;
    let parentage = compiled.ir.entity_authority_columns().parentage;
    if parentage[drive as usize] != ParentageAuthority::Root {
        return Err("drive must be a parentage root".to_owned());
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
            lineage.name.starts_with("src/") && !(lineage.name == "src/service" && key.path == "Workout")
        })
        .count();
    if stray_project_packages > 0 {
        return Err("only Workout may retarget to src/service".to_owned());
    }

    let string_call = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .find(|occurrence| occurrence.kind == ReferenceKind::FunctionCall)
        .ok_or("String::new call absent")?;
    let OccurrenceTarget::Foreign(key) = string_call.target else {
        return Err("String::new must stay foreign".to_owned());
    };
    if matches!(key.origin, backend_semantic::ir::ForeignOrigin::Package(_)) {
        return Err("String::new must not retarget to a project module path".to_owned());
    }

    Ok(())
}

#[test]
fn cross_file_type_mention_from_mod_rs_retargets_to_module_path() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-type-mention-mod-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src/service")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"type_mention_mod_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service/mod.rs"), SERVICE).map_err(|error| error.to_string())?;
    let lib = "mod service;\npub fn drive(item: service::Workout) -> service::Workout { item }\n";
    fs::write(root.join("src/lib.rs"), lib).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", lib)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let mention = package_type_mentions(&lane)
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
                && key.path == "Workout"
                && key.display == "Workout"
        })
        .ok_or("oracle type mention absent")?;
    if mention.kind != ReferenceKind::TypeReference {
        return Err("retargeted occurrence must be a type reference".to_owned());
    }
    Ok(())
}

#[test]
fn same_file_type_mention_stays_local() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-same-file-type-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"same_file_type_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), LOCAL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", LOCAL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    let mention = lane
        .occurrences
        .iter()
        .map(|row| &row.occurrence)
        .find(|occurrence| {
            occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(occurrence.kind, ReferenceKind::TypeReference)
                && matches!(occurrence.target, OccurrenceTarget::Local(_))
        })
        .ok_or("oracle local Workout type mention absent")?;
    if matches!(
        mention.target,
        OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
            origin: backend_semantic::ir::ForeignOrigin::Package(_),
            ..
        })
    ) {
        return Err("same-file Workout type mention must not use a package key".to_owned());
    }
    Ok(())
}

#[test]
fn crate_root_without_module_use_has_no_crate_module() -> Result<(), String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-no-crate-module-{nonce}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"no_crate_module_fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    fs::write(root.join("src/service.rs"), "pub fn set_note() {}\n")
        .map_err(|error| error.to_string())?;
    fs::write(root.join("src/lib.rs"), PATH_CALL_LIB).map_err(|error| error.to_string())?;
    let bytes = compile_source(&root, "src/lib.rs", PATH_CALL_LIB)?;
    let _ = fs::remove_dir_all(&root);
    let lane = lane(&bytes)?;
    if lane.view.entities().any(|entity| {
        entity.kind == EntityKind::Module
            && lane
                .view
                .atoms()
                .nth(entity.name.raw as usize)
                .is_some_and(|atom| atom.bytes == b"crate")
    }) {
        return Err("crate-root file without module-level use must not gain a crate module".to_owned());
    }
    Ok(())
}
