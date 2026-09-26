//! Cross-file C++ method calls must join on the declaring header's package key.

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_clang::ClangProject;
use backend_semantic::ir::{
    EntityKind, ForeignOrigin, FragmentView, OccurrenceTarget,
};
use backend_semantic::vocabulary::{CxxStandard, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, ToolchainDomain};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error("compile failed")]
    Compile,
    #[error("{0}")]
    Missing(&'static str),
    #[error("fragment validation failed")]
    Validate,
}

fn clang_available() -> bool {
    Command::new("/usr/bin/clang")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

struct TempProject {
    base: PathBuf,
}

impl TempProject {
    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.base.join(path)
    }
}

impl AsRef<Path> for TempProject {
    fn as_ref(&self) -> &Path {
        &self.base
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn unique_temp_project() -> Result<TempProject, TestError> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "nudox-clang-cpp-method-{pid}-{ordinal}",
        pid = std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("include"))
        .and_then(|()| fs::create_dir_all(base.join("src")))
        .map_err(|_| TestError::Missing("temp project"))?;
    Ok(TempProject { base })
}

fn lower_with_authority<'output>(
    source: &[u8],
    project: &ClangProject,
    work: &Path,
    output: &'output mut [u8],
) -> Result<FragmentView<'output>, TestError> {
    let cancelled = AtomicBool::new(false);
    let toolchain = ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_identity(
            NativeTool::Clang,
            Path::new("/usr/bin/clang"),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"clang-cpp-method-call-test"),
        )
        .map_err(|_| TestError::Missing("toolchain"))?,
    );
    let mut diagnostic_output = [0_u8; 4096];
    let compiled = compile(
        CompileRequest {
            profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain,
            authority: SemanticAuthorityInput::Clang { project },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic_output,
            native_work: work,
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|_| TestError::Compile)?;
    let length = compiled.fragment.as_ref().len();
    drop(compiled);
    FragmentView::validate(&output[..length]).map_err(|_| TestError::Validate)
}

fn entity_of(
    view: &FragmentView<'_>,
    name: &[u8],
    kind: EntityKind,
) -> Result<backend_semantic::ir::EntityId, TestError> {
    for entity in view.entities() {
        if entity.kind != kind {
            continue;
        }
        let atom = view
            .atoms()
            .nth(entity.name.raw as usize)
            .ok_or(TestError::Missing("entity atom"))?;
        if atom.bytes == name {
            return Ok(entity.entity);
        }
    }
    Err(TestError::Missing("entity by name"))
}

#[test]
fn cross_file_cpp_method_call_joins_on_header_package_key() -> Result<(), TestError> {
    if !clang_available() {
        eprintln!("clang unavailable; skipping cross_file_cpp_method_call_joins_on_header_package_key");
        return Ok(());
    }

    let root = unique_temp_project()?;
    let header = root.join("include/workout.h");
    let entry = root.join("src/drive.cpp");
    let source = b"#include \"workout.h\"\nvoid drive(Workout item) { item.set_note(); }\n";
    fs::write(&header, b"struct Workout { void set_note(); };\n")
        .map_err(|_| TestError::Missing("header write"))?;
    fs::write(&entry, source).map_err(|_| TestError::Missing("entry write"))?;
    let database = format!(
        "[{{\"directory\":\"{directory}\",\"file\":\"src/drive.cpp\",\"arguments\":[\"clang++\",\"-Iinclude\",\"-std=c++23\",\"-c\",\"src/drive.cpp\"]}}]",
        directory = root.base.display(),
    );
    fs::write(root.join("compile_commands.json"), database)
        .map_err(|_| TestError::Missing("database write"))?;

    let work = root.join("work");
    fs::create_dir_all(&work).map_err(|_| TestError::Missing("work dir"))?;
    let project =
        ClangProject::open(&root, &entry).map_err(|_| TestError::Missing("project open"))?;
    let mut output = vec![0xa5_u8; 65_536];
    let view = lower_with_authority(source, &project, &work, &mut output)?;

    let drive_fn = entity_of(&view, b"drive", EntityKind::Function)?;

    let mut method_call = false;
    for row in view
        .occurrences()
        .ok_or(TestError::Missing("occurrences"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TestError::Missing("occurrence decode"))?
    {
        if row.owner != drive_fn {
            continue;
        }
        if row.occurrence.kind != backend_semantic::ir::ReferenceKind::FunctionCall {
            continue;
        }
        let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
            return Err(TestError::Missing(
                "method call must be oracle package FunctionCall on include/workout.h with path set_note",
            ));
        };
        let ForeignOrigin::Package(lineage) = key.origin else {
            return Err(TestError::Missing("package foreign origin"));
        };
        if lineage.ecosystem != "c" || lineage.name != "include/workout.h" {
            return Err(TestError::Missing("c:include/workout.h package"));
        }
        if key.path != "set_note" || key.display != "set_note" {
            return Err(TestError::Missing("set_note path/display"));
        }
        if key.kind != Some(EntityKind::Function) {
            return Err(TestError::Missing("function entity kind"));
        }
        if row.occurrence.confidence != backend_semantic::ir::OccurrenceConfidence::Oracle {
            return Err(TestError::Missing("oracle confidence"));
        }
        method_call = true;
    }

    if !method_call {
        return Err(TestError::Missing(
            "cross-file method call occurrence on include/workout.h:set_note",
        ));
    }
    Ok(())
}
