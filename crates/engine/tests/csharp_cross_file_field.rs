//! Cross-file C# field reads lower to nuget namespace keys the field join can follow.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod csharp_support;

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_csharp::legacy::{CSharpImage, DeclarationKind, ReferenceTag};
use backend_semantic::ir::{
    EntityKind, ForeignOrigin, FragmentError, FragmentView, OccurrenceConfidence,
    OccurrenceTarget, ReferenceKind,
};
use backend_semantic::vocabulary::{CSharpVersion, LanguageProfile, Stage};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
const ORACLE_DEADLINE: Duration = Duration::from_secs(120);

const WORKOUT_SERVICE: &str = r"namespace Demo {
    public class WorkoutService {
        public int Note;
        public void SetNote() {}
    }
}
";

const WEEKS: &str = r"namespace Demo {
    public class Weeks {
        public int Note;

        public void Drive(WorkoutService service) {
            var cross = service.Note;
            service.SetNote();
            var local = Note;
        }
    }
}
";

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] csharp_support::Error),
    #[error("I/O failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain resolution failed: {source}")]
    Toolchain {
        #[source]
        source: backend_engine::driver::ToolchainResolutionError,
    },
    #[error("compile failed: {cause}")]
    Compile { cause: String },
    #[error("fragment validation failed: {source}")]
    Fragment {
        #[source]
        source: FragmentError,
    },
    #[error("image rejected: {cause}")]
    Image { cause: String },
    #[error("{message}")]
    Fact { message: String },
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn resolve_toolchain(path: &Path) -> Result<ResolvedToolchain<'_>, TestError> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::CSharpCompiler, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

fn write_fixture(root: &Path) -> Result<(PathBuf, PathBuf), TestError> {
    let workout = root.join("WorkoutService.cs");
    let weeks = root.join("Weeks.cs");
    fs::write(&workout, WORKOUT_SERVICE).map_err(io)?;
    fs::write(&weeks, WEEKS).map_err(io)?;
    Ok((workout, weeks))
}

fn compile_fixture(
    source: &[u8],
    image: &[u8],
    work: &Path,
    toolchain: ResolvedToolchain<'_>,
) -> Result<Vec<u8>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let fragment = compile(
        CompileRequest {
            profile: PROFILE,
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::CSharp { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile {
        cause: format!("{failure:?}"),
    })?;
    Ok(fragment.fragment.as_ref().to_vec())
}

fn entity_name<'fragment>(
    view: &FragmentView<'fragment>,
    ordinal: u32,
) -> Result<&'fragment [u8], TestError> {
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    view.entities()
        .find(|entity| entity.entity.raw == ordinal)
        .and_then(|entity| atoms.get(entity.name.raw as usize).copied())
        .ok_or_else(|| TestError::Fact {
            message: "entity name is absent from the fragment".to_owned(),
        })
}

fn drive_owner<'fragment>(view: &FragmentView<'fragment>) -> Result<u32, TestError> {
    for entity in view.entities() {
        if entity.kind == EntityKind::Function
            && entity_name(view, entity.entity.raw)? == b"Drive"
        {
            return Ok(entity.entity.raw);
        }
    }
    Err(TestError::Fact {
        message: "Drive owner is absent from the fragment".to_owned(),
    })
}

fn drive_decl_start(image: &[u8]) -> Result<u32, TestError> {
    let image = CSharpImage::open(image).map_err(|error| TestError::Image {
        cause: format!("{error:?}"),
    })?;
    for declaration in image.declarations() {
        let declaration = declaration.map_err(|error| TestError::Image {
            cause: format!("{error:?}"),
        })?;
        if declaration.kind == DeclarationKind::Method && declaration.name.bytes == b"Drive" {
            return Ok(declaration.decl_start);
        }
    }
    Err(TestError::Fact {
        message: "Drive declaration is absent from the authority image".to_owned(),
    })
}

fn cross_file_authority_reference(image: &[u8]) -> Result<(u32, u32), TestError> {
    let image = CSharpImage::open(image).map_err(|error| TestError::Image {
        cause: format!("{error:?}"),
    })?;
    let drive_index = image
        .declarations()
        .enumerate()
        .find_map(|(index, declaration)| {
            let declaration = declaration.ok()?;
            if declaration.kind == DeclarationKind::Method && declaration.name.bytes == b"Drive" {
                u32::try_from(index).ok()
            } else {
                None
            }
        })
        .ok_or_else(|| TestError::Fact {
            message: "Drive declaration row is absent from the authority image".to_owned(),
        })?;
    for reference in image.references() {
        let reference = reference.map_err(|error| TestError::Image {
            cause: format!("{error:?}"),
        })?;
        if reference.owner == drive_index
            && reference.target.is_none()
            && reference.kind == ReferenceTag::MemberAccess
            && reference.spelling.bytes == b"Demo.WorkoutService.Note"
        {
            return Ok((reference.start, reference.end));
        }
    }
    Err(TestError::Fact {
        message: "qualified absent field reference is absent from the authority image".to_owned(),
    })
}

#[test]
fn cross_file_field_read_targets_workout_service_namespace_key() -> Result<(), TestError> {
    let root = csharp_support::fresh_dir("cross-file-field-fixture")?;
    let (_, weeks_path) = write_fixture(&root)?;
    let weeks_source = fs::read(&weeks_path).map_err(io)?;
    let weeks_image = csharp_support::authority_image(
        &[root.as_path()],
        "Demo",
        &weeks_path,
        Instant::now() + ORACLE_DEADLINE,
    )?;
    let dotnet = csharp_support::dotnet_executable()?;
    let dotnet = dotnet.canonicalize().map_err(io)?;
    let toolchain = resolve_toolchain(&dotnet)?;
    let work = csharp_support::fresh_dir("cross-file-field-compile")?;
    let weeks_fragment = compile_fixture(weeks_source.as_slice(), &weeks_image, &work, toolchain)?;
    let view = FragmentView::validate(weeks_fragment.as_slice())
        .map_err(|source| TestError::Fragment { source })?;

    let drive = drive_owner(&view)?;
    let (note_start, note_end) = {
        let before = WEEKS
            .find("service.Note")
            .ok_or_else(|| TestError::Fact {
                message: "cross-file Note site is absent from the fixture source".to_owned(),
            })?;
        let start = u32::try_from(before + "service.".len()).map_err(|_| TestError::Fact {
            message: "Note span start does not fit u32".to_owned(),
        })?;
        let end = start
            + u32::try_from("Note".len()).map_err(|_| TestError::Fact {
                message: "Note span end does not fit u32".to_owned(),
            })?;
        (start, end)
    };
    let (auth_start, auth_end) = cross_file_authority_reference(&weeks_image)?;
    if auth_start != note_start || auth_end != note_end {
        return Err(TestError::Fact {
            message: "authority image kept the Note name-token span".to_owned(),
        });
    }
    let start = usize::try_from(auth_start).map_err(|_| TestError::Fact {
        message: "authority span start does not fit usize".to_owned(),
    })?;
    let end = usize::try_from(auth_end).map_err(|_| TestError::Fact {
        message: "authority span end does not fit usize".to_owned(),
    })?;
    if weeks_source.get(start..end) != Some(b"Note") {
        return Err(TestError::Fact {
            message: "authority span bytes must be Note, not service.Note".to_owned(),
        });
    }

    let drive_start = drive_decl_start(&weeks_image)?;
    let Some(mut rows) = view.occurrences() else {
        return Err(TestError::Fact {
            message: "weeks fragment has no occurrence lane".to_owned(),
        });
    };
    let mut cross_file = None;
    while let Some(row) = rows.next() {
        let row = row.map_err(|error| TestError::Fact {
            message: format!("occurrence decode failed: {error}"),
        })?;
        if row.owner.raw != drive || row.occurrence.kind != ReferenceKind::FieldAccess {
            continue;
        }
        if let OccurrenceTarget::Foreign(key) = row.occurrence.target
            && key.path == "Note"
            && key.display == "Note"
            && key.kind == Some(EntityKind::Field)
            && matches!(
                key.origin,
                ForeignOrigin::Namespace {
                    ecosystem: "nuget",
                    namespace: "Demo.WorkoutService",
                }
            )
            && row.occurrence.confidence == OccurrenceConfidence::Oracle
        {
            cross_file = Some(row.occurrence.span);
            break;
        }
    }
    let span = cross_file.ok_or_else(|| TestError::Fact {
        message: "cross-file Demo.WorkoutService.Note FieldAccess occurrence is absent".to_owned(),
    })?;
    if drive_start + span.start != note_start || drive_start + span.end != note_end {
        return Err(TestError::Fact {
            message: "occurrence span must cover the Note name token only".to_owned(),
        });
    }
    Ok(())
}

#[test]
fn same_file_note_stays_local_and_set_note_stays_method() -> Result<(), TestError> {
    let root = csharp_support::fresh_dir("cross-file-field-local")?;
    let (_, weeks_path) = write_fixture(&root)?;
    let weeks_source = fs::read(&weeks_path).map_err(io)?;
    let weeks_image = csharp_support::authority_image(
        &[root.as_path()],
        "Demo",
        &weeks_path,
        Instant::now() + ORACLE_DEADLINE,
    )?;
    let dotnet = csharp_support::dotnet_executable()?;
    let dotnet = dotnet.canonicalize().map_err(io)?;
    let toolchain = resolve_toolchain(&dotnet)?;
    let work = csharp_support::fresh_dir("cross-file-field-local-compile")?;
    let weeks_fragment = compile_fixture(weeks_source.as_slice(), &weeks_image, &work, toolchain)?;
    let view = FragmentView::validate(weeks_fragment.as_slice())
        .map_err(|source| TestError::Fragment { source })?;

    let drive = drive_owner(&view)?;
    let Some(mut rows) = view.occurrences() else {
        return Err(TestError::Fact {
            message: "weeks fragment has no occurrence lane".to_owned(),
        });
    };
    let mut local_reads = 0_usize;
    let mut method_calls = 0_usize;
    let mut set_note_kind = None;
    while let Some(row) = rows.next() {
        let row = row.map_err(|error| TestError::Fact {
            message: format!("occurrence decode failed: {error}"),
        })?;
        if row.owner.raw != drive {
            continue;
        }
        if row.occurrence.kind == ReferenceKind::VariableUse
            && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
        {
            local_reads += 1;
        }
        if row.occurrence.kind == ReferenceKind::MethodCall {
            method_calls += 1;
            if let OccurrenceTarget::Foreign(key) = row.occurrence.target {
                set_note_kind = key.kind;
            }
        }
    }
    if local_reads != 1 {
        return Err(TestError::Fact {
            message: format!("expected one same-file Note read, saw {local_reads}"),
        });
    }
    if method_calls != 1 {
        return Err(TestError::Fact {
            message: format!("expected one SetNote method call, saw {method_calls}"),
        });
    }
    if set_note_kind == Some(EntityKind::Field) {
        return Err(TestError::Fact {
            message: "SetNote must not lower as a Field foreign key".to_owned(),
        });
    }
    if set_note_kind != Some(EntityKind::Function) {
        return Err(TestError::Fact {
            message: format!("SetNote must lower as Function, saw {set_note_kind:?}"),
        });
    }
    Ok(())
}
