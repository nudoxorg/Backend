//! Exercises the `compiler-ir` tests rlib-surface contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env,
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use thiserror::Error;

#[derive(Debug, Error)]
enum ProbeError {
    #[error("cannot resolve the current test executable")]
    CurrentExecutable(#[source] io::Error),
    #[error("the current test executable has no dependency directory")]
    MissingDependencyDirectory,
    #[error("cannot read the dependency directory")]
    ReadDependencyDirectory(#[source] io::Error),
    #[error("cannot read a dependency directory entry")]
    ReadDependencyEntry(#[source] io::Error),
    #[error("no compatible compiler IR rlib was found")]
    MissingCompatibleArtifacts,
    #[error("cannot spawn the consumer compiler")]
    SpawnCompiler(#[source] io::Error),
    #[error("the consumer compiler did not expose its standard input")]
    MissingCompilerInput,
    #[error("cannot write the consumer probe")]
    WriteCompilerInput(#[source] io::Error),
    #[error("cannot collect the consumer compiler result")]
    WaitForCompiler(#[source] io::Error),
    #[error("a consumer expected to be rejected compiled successfully")]
    UnexpectedCompileSuccess,
    #[error("the rejected consumer diagnostic omitted {expected}: {stderr}")]
    MissingDiagnostic {
        expected: &'static str,
        stderr: String,
    },
    #[error("the legal consumer was rejected: {stderr}")]
    LegalConsumerRejected { stderr: String },
}

struct RejectedProbe {
    source: &'static [u8],
    code: &'static str,
    symbols: &'static [&'static str],
}

fn is_rlib(path: &Path, crate_name: &str) -> bool {
    let prefix = format!("lib{crate_name}-");
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rlib"))
}

fn compatible_artifacts(directory: &Path) -> Result<Vec<PathBuf>, ProbeError> {
    let formats = fs::read_dir(directory).map_err(ProbeError::ReadDependencyDirectory)?;
    let mut compatible = Vec::new();
    for format_entry in formats {
        let format_path = format_entry
            .map_err(ProbeError::ReadDependencyEntry)?
            .path();
        if !is_rlib(&format_path, "compiler_ir") {
            continue;
        }
        let probe = compile(
            b"use compiler_ir::{AtomId, EntityId, TypeId}; fn compatible() { let _ = (AtomId::new(0), EntityId::new(0), TypeId::new(0)); }",
            directory,
            &format_path,
        )?;
        if !probe.status.success() {
            continue;
        }
        compatible.push(format_path);
    }
    if compatible.is_empty() {
        Err(ProbeError::MissingCompatibleArtifacts)
    } else {
        Ok(compatible)
    }
}

fn compile(
    source: &[u8],
    dependencies: &Path,
    format_artifact: &Path,
) -> Result<Output, ProbeError> {
    let mut format_extern = OsString::from("compiler_ir=");
    format_extern.push(format_artifact);
    let compiler = match env::var_os("RUSTC") {
        Some(compiler) => compiler,
        None => OsString::from("rustc"),
    };
    let mut child = Command::new(compiler)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "lib",
            "--emit=metadata=-",
            "-L",
        ])
        .arg(dependencies)
        .arg("--extern")
        .arg(format_extern)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProbeError::SpawnCompiler)?;
    let mut input = child.stdin.take().ok_or(ProbeError::MissingCompilerInput)?;
    input
        .write_all(source)
        .map_err(ProbeError::WriteCompilerInput)?;
    drop(input);
    child
        .wait_with_output()
        .map_err(ProbeError::WaitForCompiler)
}

fn require_rejected(
    output: &Output,
    code: &'static str,
    symbols: &'static [&'static str],
) -> Result<(), ProbeError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        return Err(ProbeError::UnexpectedCompileSuccess);
    }
    for expected in core::iter::once(code).chain(symbols.iter().copied()) {
        if !stderr.contains(expected) {
            return Err(ProbeError::MissingDiagnostic {
                expected,
                stderr: stderr.into_owned(),
            });
        }
    }
    Ok(())
}

#[test]
fn exported_rlib_keeps_views_private_typed_and_caller_borrowing() -> Result<(), ProbeError> {
    let executable = env::current_exe().map_err(ProbeError::CurrentExecutable)?;
    let dependencies = executable
        .parent()
        .ok_or(ProbeError::MissingDependencyDirectory)?;
    let format_artifacts = compatible_artifacts(dependencies)?;

    let rejected = [
        RejectedProbe {
            source: b"use compiler_ir::FragmentView; const BAD: FragmentView<'static> = FragmentView { envelope: &[], entities: &[], type_nodes: &[] };",
            code: "cannot construct",
            symbols: &["envelope", "entities", "type_nodes"],
        },
        RejectedProbe {
            source: b"use compiler_ir::FragmentView; fn bad() { let _: FragmentView<'_> = (&[][..]).into(); }",
            code: "error[E0277]",
            symbols: &["From", "FragmentView"],
        },
        RejectedProbe {
            source: b"use compiler_ir::{EntityId, TypeId}; fn entity_only(_: EntityId) {} fn bad() { entity_only(TypeId::new(0)); }",
            code: "error[E0308]",
            symbols: &["Entity", "Type"],
        },
        RejectedProbe {
            source: b"use compiler_ir::TypeNode; use compiler_ir::EntityId; fn bad() { let _ = TypeNode::Reference(EntityId::new(0)); }",
            code: "error[E0308]",
            symbols: &["Entity", "Type"],
        },
        RejectedProbe {
            source: b"use compiler_ir::{ComputedType, ConcreteState, GuardedType}; fn bad() { let _: GuardedType<ConcreteState> = GuardedType::computed(ComputedType::This); }",
            code: "error[E0308]",
            symbols: &["ComputedState", "ConcreteState"],
        },
        RejectedProbe {
            source: b"use compiler_ir::AtomInput; fn bad() -> AtomInput<'static> { let bytes = [1_u8]; AtomInput { bytes: &bytes } }",
            code: "error[E0515]",
            symbols: &["bytes"],
        },
    ];
    for format_artifact in format_artifacts {
        for probe in &rejected {
            let output = compile(probe.source, dependencies, &format_artifact)?;
            require_rejected(&output, probe.code, probe.symbols)?;
        }

        let legal = compile(
            b"use compiler_ir::{AtomId, EntityId, TypeId}; fn legal() -> u32 { AtomId::new(1).raw + EntityId::new(2).raw + TypeId::new(3).raw }",
            dependencies,
            &format_artifact,
        )?;
        if !legal.status.success() {
            return Err(ProbeError::LegalConsumerRejected {
                stderr: String::from_utf8_lossy(&legal.stderr).into_owned(),
            });
        }
    }
    Ok(())
}
