//! Defines native java behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native java invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use crate::{
    lower::java_top_level_type_name,
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{InvalidUtf8Fact, NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const ARGUMENTS_FILE: &str = "compiler-probe.javac.args";
const FALLBACK_SOURCE_STEM: &str = "CompilerProbe";
const WORK_DIRECTORY: &str = "java";

/// Native Java compiler admission over one caller-owned source path.
pub(super) struct JavaFrontend;

impl NativeFrontend for JavaFrontend {
    fn prepare(native_work: &Path, source: &[u8]) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::JavaWork)?;
        let source_file = source_file(source)?;
        // javac has no source-stdin mode. The argument file keeps the selected public type's
        // exact source filename out of the command shell.
        let mut argument_bytes = source_file.clone();
        argument_bytes.push('\n');
        write_artifact(
            &work.join(ARGUMENTS_FILE),
            argument_bytes.as_bytes(),
            NativeArtifactRole::JavaArguments,
        )?;
        write_artifact(
            &work.join(source_file),
            source,
            NativeArtifactRole::JavaSource,
        )
    }

    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args(["-proc:none", "-Xprint", "@compiler-probe.javac.args"])
            .current_dir(native_work.join(WORK_DIRECTORY))
            // javac honors CLASSPATH/JAVA_TOOL_OPTIONS when inherited.  The caller-resolved
            // executable and the explicit no-processor mode are the complete authority here.
            .env_clear();
        command
    }

    fn source_via_stdin() -> bool {
        false
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_directory_if_present(
            native_work.join(WORK_DIRECTORY),
            NativeArtifactRole::JavaWork,
        )
    }
}

/// Finds the first top-level Java type name so public classes can retain javac's filename rule.
/// The shared lowerer scanner ignores comments, literals, and nested braces; malformed source
/// still reaches javac and returns its bounded native diagnostic.
fn source_file(source: &[u8]) -> Result<String, NativeWorkError> {
    let stem = match java_top_level_type_name(source) {
        Some(name) => {
            core::str::from_utf8(name).map_err(|cause| NativeWorkError::ArtifactText {
                artifact: NativeArtifactRole::JavaSource,
                fact: InvalidUtf8Fact {
                    valid_up_to: cause.valid_up_to(),
                    error_len: cause.error_len(),
                },
            })?
        }
        None => FALLBACK_SOURCE_STEM,
    };
    let mut source_file = String::with_capacity(stem.len() + ".java".len());
    source_file.push_str(stem);
    source_file.push_str(".java");
    Ok(source_file)
}
