//! Packaging and environment contract for the vendored C# Roslyn oracle.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod csharp_support;

use backend_frontend_csharp::legacy::CSharpImage;
use std::{fs, path::PathBuf, process::Command};
use thiserror::Error;

const FIDELITY_IMAGE: &[u8] =
    include_bytes!("../../../frontends/csharp/tests/fixtures/producer/fidelity.ncaimg");

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(#[from] csharp_support::Error),
    #[error("I/O failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("dotnet {command} failed: {stderr}")]
    Dotnet {
        command: &'static str,
        stderr: String,
    },
    #[error("packaging fact falsified: {0}")]
    Fact(String),
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn helper() -> Result<PathBuf, TestError> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| TestError::Fact("manifest directory missing".into()))?;
    Ok(PathBuf::from(manifest).join("../../frontends/csharp/src/legacy/helper"))
}

fn run_dotnet(
    dotnet: &PathBuf,
    helper: &PathBuf,
    args: &[&str],
    label: &'static str,
) -> Result<(), TestError> {
    run_dotnet_with(dotnet, helper, args, &[], label)
}

/// Same as [`run_dotnet`], but with extra trailing MSBuild properties (e.g. an
/// isolated intermediate/output directory) appended to the invocation.
fn run_dotnet_with(
    dotnet: &PathBuf,
    helper: &PathBuf,
    args: &[&str],
    extra: &[std::ffi::OsString],
    label: &'static str,
) -> Result<(), TestError> {
    let output = Command::new(dotnet)
        .args(args)
        .args(extra)
        .current_dir(helper)
        .output()
        .map_err(io)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TestError::Dotnet {
            command: label,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[test]
fn tooling_unavailable_is_a_typed_terminal() -> Result<(), TestError> {
    if std::env::var_os("NUDOX_CSHARP_UNAVAILABLE_PROBE").is_none() {
        return Ok(());
    }
    match csharp_support::dotnet_executable() {
        Err(csharp_support::Error::Toolchain) => Ok(()),
        Err(other) => Err(TestError::Fact(format!("wrong typed error: {other}"))),
        Ok(path) => Err(TestError::Fact(format!(
            "nonexistent tool accepted: {path:?}"
        ))),
    }
}

#[test]
fn locked_roslyn_packaging_round_trips_fixture_and_rejects_tracked_outputs() -> Result<(), TestError>
{
    let helper = helper()?;
    let dotnet = csharp_support::dotnet_executable()?;
    // Every concurrently running C# test builds the same checked-in helper
    // project. `dotnet restore`/`build` write both intermediate build state
    // (`obj/`) and final output (`bin/`) under the project directory by
    // default, so without explicit, per-process `BaseIntermediateOutputPath`
    // and `BaseOutputPath` overrides, all concurrent processes race on the
    // same files and either fail outright or produce a subtly corrupted
    // `oracle.dll` (observed as spurious `OraclePublish`/"authority image
    // changed" failures under full-suite load). The restore and build steps
    // share the same isolated directories so the build sees what its own
    // restore produced.
    let build_root = csharp_support::fresh_dir("packaging-build")?;
    let intermediate = build_root.join("obj");
    let output_base = build_root.join("bin");
    let mut intermediate_arg = std::ffi::OsString::from("-p:BaseIntermediateOutputPath=");
    intermediate_arg.push(&intermediate);
    intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let mut output_arg = std::ffi::OsString::from("-p:BaseOutputPath=");
    output_arg.push(&output_base);
    output_arg.push(std::path::MAIN_SEPARATOR.to_string());
    run_dotnet_with(
        &dotnet,
        &helper,
        &["restore", "--locked-mode", "--nologo"],
        &[intermediate_arg.clone()],
        "restore",
    )?;
    run_dotnet_with(
        &dotnet,
        &helper,
        // `UseSharedCompilation=false` forces this build's own isolated
        // `csc` process instead of the ambient VBCSCompiler node MSBuild
        // shares across every concurrent `dotnet build`/`publish` on this
        // machine: under full-suite load, many test binaries build this
        // same checked-in helper project at once, and a shared compiler
        // server is exactly the kind of ambient, cross-process state that
        // isolated `obj/`/`bin/` directories alone cannot rule out as a
        // source of the "authority image changed" drift this test guards.
        &["build", "-c", "Release", "--nologo", "-p:UseSharedCompilation=false"],
        &[intermediate_arg, output_arg],
        "build",
    )?;

    let lock = fs::read_to_string(helper.join("packages.lock.json")).map_err(io)?;
    if !lock.contains("\"Microsoft.CodeAnalysis.CSharp\"")
        || !lock.contains("\"type\": \"Direct\"")
        || !lock.contains("\"requested\": \"[5.6.0, )\"")
        || !lock.contains("\"resolved\": \"5.6.0\"")
    {
        return Err(TestError::Fact(
            "lock file does not pin direct Roslyn CSharp 5.6.0".into(),
        ));
    }

    let tracked = Command::new("git")
        .args(["ls-files", "--", "obj", "bin"])
        .current_dir(&helper)
        .output()
        .map_err(io)?;
    if !tracked.stdout.is_empty() {
        return Err(TestError::Fact("helper build outputs are tracked".into()));
    }

    let producer = helper
        .join("../../../tests/fixtures/producer")
        .canonicalize()
        .map_err(io)?;
    let output_root = csharp_support::fresh_dir("packaging-image")?;
    let output = output_root.join("fidelity.ncaimg");
    let oracle = output_base.join("Release/net8.0/oracle.dll");
    let produced = Command::new(&dotnet)
        .arg(&oracle)
        .args([
            "--mode",
            "source",
            "--root",
            ".",
            "--authority-image",
            "--source-binding",
            "fidelity.cs",
            "--out",
        ])
        .arg(&output)
        .current_dir(&producer)
        .output()
        .map_err(io)?;
    if !produced.status.success() {
        return Err(TestError::Dotnet {
            command: "authority image",
            stderr: String::from_utf8_lossy(&produced.stderr).into_owned(),
        });
    }
    let produced = fs::read(&output).map_err(io)?;
    fs::remove_dir_all(output_root).map_err(io)?;
    let _ = fs::remove_dir_all(&build_root);
    if produced != FIDELITY_IMAGE {
        return Err(TestError::Fact(
            "Roslyn authority image changed in documented round-trip".into(),
        ));
    }
    let image = CSharpImage::open(FIDELITY_IMAGE)
        .map_err(|_| TestError::Fact("committed image rejected".into()))?;
    let bound_path = producer.join("fidelity.cs");
    let bound_path = bound_path.to_string_lossy();
    let docs = image
        .docs()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TestError::Fact("docs could not be decoded".into()))?;
    if !docs
        .iter()
        .all(|doc| doc.file.bytes == bound_path.as_bytes())
    {
        return Err(TestError::Fact(format!(
            "documentation rows do not retain documented binding: {:?}",
            docs.iter().map(|doc| doc.file.bytes).collect::<Vec<_>>()
        )));
    }
    let references = image
        .references()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TestError::Fact("references could not be decoded".into()))?;
    if !references
        .iter()
        .all(|reference| reference.file.bytes == bound_path.as_bytes())
    {
        return Err(TestError::Fact(
            "reference rows do not retain documented binding".into(),
        ));
    }

    let probe = std::env::current_exe().map_err(io)?;
    let status = Command::new(probe)
        .arg("tooling_unavailable_is_a_typed_terminal")
        .arg("--exact")
        .env("NUDOX_CSHARP_UNAVAILABLE_PROBE", "1")
        .env("COMPILER_CSHARP_COMPILER", "/nonexistent/nudox-dotnet")
        .status()
        .map_err(io)?;
    if !status.success() {
        return Err(TestError::Fact(
            "missing compiler did not produce typed tooling-unavailable terminal".into(),
        ));
    }
    Ok(())
}
