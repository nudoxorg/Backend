//! Defines native java behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native java invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use compiler_vocabulary::JavaRelease;

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const ARGUMENTS_FILE: &str = "compiler-probe.javac.args";
const SOURCE_FILE: &str = "CompilerProbe.java";
const WORK_DIRECTORY: &str = "java";

/// Native Java compiler admission over one caller-owned source path.
pub(super) struct JavaFrontend;

impl NativeFrontend for JavaFrontend {
    type Profile = JavaRelease;

    fn prepare(
        _profile: Self::Profile,
        native_work: &Path,
        source: &[u8],
    ) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::JavaWork)?;
        let source_file = source_file();
        // javac has no source-stdin mode. The argument file keeps the selected public type's
        // exact source filename out of the command shell.
        let mut argument_bytes = String::from(source_file);
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

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "-proc:none",
                "--release",
                java_release(profile),
                "-Xprint",
                "@compiler-probe.javac.args",
            ])
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

const fn java_release(profile: JavaRelease) -> &'static str {
    match profile {
        JavaRelease::Java8 => "8",
        JavaRelease::Java11 => "11",
        JavaRelease::Java17 => "17",
        JavaRelease::Java21 => "21",
        JavaRelease::Java25 => "25",
    }
}

/// Returns the fixed filename for single-buffer Java parser admission.
///
/// Semantic Java admission requires a caller-selected project/image authority;
/// this parser input must not derive a public type name from source text.
fn source_file() -> &'static str {
    SOURCE_FILE
}
