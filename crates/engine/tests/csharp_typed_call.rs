//! Cross-file C# method calls emit namespace foreign keys that name the declaring type.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod csharp_support;

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_csharp::legacy::{CSharpImage, ReferenceTag};
use backend_semantic::ir::{
    ForeignOrigin, FragmentView, OccurrenceConfidence, OccurrenceTarget, ReferenceKind,
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

const WORKOUT_SERVICE: &str = "namespace Demo
{
    public class WorkoutService
    {
        public void SetNote(string note) {}
        public void Apply() { this.SetNote(\"z\"); }
    }
}
";

const WEEKS: &str = "namespace Demo
{
    public class Weeks
    {
        private readonly WorkoutService service;
        public Weeks(WorkoutService service) { this.service = service; }
        public void Sync()
        {
            this.service.SetNote(\"x\");
            System.Console.WriteLine(\"y\");
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
        source: backend_semantic::ir::FragmentError,
    },
    #[error("image rejected: {cause}")]
    Image { cause: String },
    #[error("{message}")]
    Fact { message: String },
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = csharp_support::dotnet_executable()?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = std::process::Command::new(&*path)
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
    let service_path = root.join("WorkoutService.cs");
    let weeks_path = root.join("Weeks.cs");
    fs::write(&service_path, WORKOUT_SERVICE).map_err(io)?;
    fs::write(&weeks_path, WEEKS).map_err(io)?;
    Ok((service_path, weeks_path))
}

fn compile_fixture(
    source: &[u8],
    image: &[u8],
    work: &Path,
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
            toolchain: ToolchainSelection::ResolvedNative(toolchain()?),
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

fn owner_decl_start(image: &[u8], owner_name: &str) -> Result<u32, TestError> {
    let image = CSharpImage::open(image).map_err(|error| TestError::Image {
        cause: format!("{error:?}"),
    })?;
    for declaration in image.declarations() {
        let declaration = declaration.map_err(|error| TestError::Image {
            cause: format!("{error:?}"),
        })?;
        if declaration.name.bytes == owner_name.as_bytes() {
            return Ok(declaration.decl_start);
        }
    }
    Err(TestError::Fact {
        message: format!("owner declaration {owner_name} is absent from the authority image"),
    })
}

fn namespace_method_calls<'fragment>(
    view: &FragmentView<'fragment>,
) -> Result<Vec<(ForeignOrigin<'fragment>, &'fragment str, &'fragment str)>, TestError> {
    let mut calls = Vec::new();
    let Some(mut rows) = view.occurrences() else {
        return Ok(calls);
    };
    while let Some(row) = rows.next() {
        let row = row.map_err(|error| TestError::Fact {
            message: format!("occurrence decode failed: {error}"),
        })?;
        if row.occurrence.kind != ReferenceKind::MethodCall {
            continue;
        }
        if let OccurrenceTarget::Foreign(key) = row.occurrence.target
            && let ForeignOrigin::Namespace { .. } = key.origin
        {
            calls.push((key.origin, key.path, key.display));
        }
    }
    Ok(calls)
}

#[test]
fn weeks_cross_file_set_note_emits_namespace_foreign_key_with_method_span() -> Result<(), TestError> {
    let root = csharp_support::fresh_dir("typed-call")?;
    let (service_path, weeks_path) = write_fixture(&root)?;
    let roots = [root.as_path()];
    let service_image = csharp_support::authority_image(
        &roots,
        "typed-call",
        &service_path,
        Instant::now() + ORACLE_DEADLINE,
    )?;
    let weeks_image = csharp_support::authority_image(
        &roots,
        "typed-call",
        &weeks_path,
        Instant::now() + ORACLE_DEADLINE,
    )?;
    let weeks_source = fs::read(&weeks_path).map_err(io)?;
    let work = csharp_support::fresh_dir("typed-call-compile")?;
    let weeks_fragment = compile_fixture(weeks_source.as_slice(), &weeks_image, &work)?;
    let view = FragmentView::validate(weeks_fragment.as_slice())
        .map_err(|source| TestError::Fragment { source })?;

    let namespace_calls = namespace_method_calls(&view)?;
    let set_note_calls = namespace_calls
        .iter()
        .filter(|(origin, path, display)| {
            matches!(
                origin,
                ForeignOrigin::Namespace {
                    ecosystem: "nuget",
                    namespace: "Demo.WorkoutService",
                }
            ) && *path == "SetNote" && *display == "SetNote"
        })
        .count();
    if set_note_calls != 1 {
        return Err(TestError::Fact {
            message: format!("expected one Demo.WorkoutService SetNote MethodCall, saw {set_note_calls}"),
        });
    }
    let write_line_calls = namespace_calls
        .iter()
        .filter(|(origin, path, display)| {
            matches!(
                origin,
                ForeignOrigin::Namespace {
                    ecosystem: "nuget",
                    namespace: "System.Console",
                }
            ) && *path == "WriteLine" && *display == "WriteLine"
        })
        .count();
    if write_line_calls != 1 {
        return Err(TestError::Fact {
            message: format!("expected one System.Console WriteLine MethodCall, saw {write_line_calls}"),
        });
    }
    for (_, path, display) in &namespace_calls {
        if *path == "this.service.SetNote" || *display == "this.service.SetNote" {
            return Err(TestError::Fact {
                message: "whole-expression universe spelling must not survive as a MethodCall key"
                    .to_owned(),
            });
        }
        if *path == "service.SetNote" || *display == "service.SetNote" {
            return Err(TestError::Fact {
                message: "receiver-qualified universe spelling must not survive as a MethodCall key"
                    .to_owned(),
            });
        }
    }

    let sync_start = owner_decl_start(&weeks_image, "Sync")?;
    let Some(mut rows) = view.occurrences() else {
        return Err(TestError::Fact {
            message: "weeks fragment has no occurrence lane".to_owned(),
        });
    };
    let mut set_note_span = None;
    while let Some(row) = rows.next() {
        let row = row.map_err(|error| TestError::Fact {
            message: format!("occurrence decode failed: {error}"),
        })?;
        if row.occurrence.kind != ReferenceKind::MethodCall {
            continue;
        }
        if let OccurrenceTarget::Foreign(key) = row.occurrence.target
            && matches!(
                key.origin,
                ForeignOrigin::Namespace {
                    ecosystem: "nuget",
                    namespace: "Demo.WorkoutService",
                }
            )
            && key.path == "SetNote"
            && key.display == "SetNote"
            && row.occurrence.confidence == OccurrenceConfidence::Oracle
        {
            set_note_span = Some(row.occurrence.span);
            break;
        }
    }
    let span = set_note_span.ok_or_else(|| TestError::Fact {
        message: "SetNote namespace MethodCall occurrence is absent".to_owned(),
    })?;
    let start = sync_start + span.start;
    let end = sync_start + span.end;
    let start = usize::try_from(start).map_err(|_| TestError::Fact {
        message: "SetNote span start does not fit usize".to_owned(),
    })?;
    let end = usize::try_from(end).map_err(|_| TestError::Fact {
        message: "SetNote span end does not fit usize".to_owned(),
    })?;
    let slice = weeks_source
        .get(start..end)
        .ok_or_else(|| TestError::Fact {
            message: format!("SetNote span {start}..{end} is outside the Weeks source"),
        })?;
    if slice != b"SetNote" {
        return Err(TestError::Fact {
            message: format!(
                "SetNote span bytes are {:?}, not SetNote",
                String::from_utf8_lossy(slice)
            ),
        });
    }

    let service_source = fs::read(&service_path).map_err(io)?;
    let service_fragment =
        compile_fixture(service_source.as_slice(), &service_image, &work)?;
    let service_view = FragmentView::validate(service_fragment.as_slice())
        .map_err(|source| TestError::Fragment { source })?;
    let mut local_set_note = 0usize;
    let mut foreign_set_note = 0usize;
    let Some(mut rows) = service_view.occurrences() else {
        return Err(TestError::Fact {
            message: "WorkoutService fragment has no occurrence lane".to_owned(),
        });
    };
    while let Some(row) = rows.next() {
        let row = row.map_err(|error| TestError::Fact {
            message: format!("occurrence decode failed: {error}"),
        })?;
        if row.occurrence.kind != ReferenceKind::MethodCall {
            continue;
        }
        match row.occurrence.target {
            OccurrenceTarget::Local(_) => local_set_note += 1,
            OccurrenceTarget::Foreign(key)
                if matches!(
                    key.origin,
                    ForeignOrigin::Namespace {
                        ecosystem: "nuget",
                        namespace: "Demo.WorkoutService",
                    }
                ) && key.path == "SetNote" =>
            {
                foreign_set_note += 1;
            }
            _ => {}
        }
    }
    if local_set_note != 1 {
        return Err(TestError::Fact {
            message: format!(
                "expected one local this.SetNote MethodCall on WorkoutService, saw {local_set_note}"
            ),
        });
    }
    if foreign_set_note != 0 {
        return Err(TestError::Fact {
            message: format!(
                "same-file this.SetNote must not emit a foreign key, saw {foreign_set_note}"
            ),
        });
    }

    let image = CSharpImage::open(&weeks_image).map_err(|error| TestError::Image {
        cause: format!("{error:?}"),
    })?;
    for reference in image.references() {
        let reference = reference.map_err(|error| TestError::Image {
            cause: format!("{error:?}"),
        })?;
        if reference.kind != ReferenceTag::Invocation {
            continue;
        }
        if reference.spelling.bytes.contains(&b'.') {
            let spelling = std::str::from_utf8(reference.spelling.bytes).map_err(|_| {
                TestError::Fact {
                    message: "invocation spelling is not UTF-8".to_owned(),
                }
            })?;
            if spelling == "this.service.SetNote" || spelling == "service.SetNote" {
                return Err(TestError::Fact {
                    message: format!("authority image still carries whole-expression spelling {spelling}"),
                });
            }
        }
    }

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(work);
    Ok(())
}
