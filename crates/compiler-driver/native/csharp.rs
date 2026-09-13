//! Defines native csharp behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native csharp invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{path::Path, process::Command};

use compiler_vocabulary::CSharpVersion;

use crate::{
    native::{
        frontend::NativeFrontend,
        work::{create_artifact_directory, remove_directory_if_present, write_artifact},
    },
    types::{NativeArtifactRole, NativeWorkError, ResolvedToolchain},
};

const SOURCE_FILE: &str = "CompilerProbe.cs";
const PROJECT_FILE: &str = "CompilerProbe.csproj";
const NUGET_CONFIG_FILE: &str = "NuGet.Config";
const BUILD_DIRECTORY: &str = "artifacts";
const DOTNET_HOME_DIRECTORY: &str = "dotnet-home";
const NUGET_PACKAGES_DIRECTORY: &str = "nuget-packages";
const WORK_DIRECTORY: &str = "csharp";
const PROJECT: &[u8] = br#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <OutputType>Library</OutputType>
    <ImplicitUsings>disable</ImplicitUsings>
    <Nullable>disable</Nullable>
  </PropertyGroup>
</Project>
"#;
const NO_FEED_NUGET_CONFIG: &[u8] = br#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
  </packageSources>
</configuration>
"#;

/// Native .NET SDK compilation over one owned no-feed library project.
pub(super) struct CSharpFrontend;

impl NativeFrontend for CSharpFrontend {
    type Profile = CSharpVersion;

    fn prepare(
        _profile: Self::Profile,
        native_work: &Path,
        source: &[u8],
    ) -> Result<(), NativeWorkError> {
        let work = native_work.join(WORK_DIRECTORY);
        create_artifact_directory(&work, NativeArtifactRole::CSharpWork)?;
        write_artifact(
            &work.join(SOURCE_FILE),
            source,
            NativeArtifactRole::CSharpSource,
        )?;
        write_artifact(
            &work.join(PROJECT_FILE),
            PROJECT,
            NativeArtifactRole::CSharpProject,
        )?;
        write_artifact(
            &work.join(NUGET_CONFIG_FILE),
            NO_FEED_NUGET_CONFIG,
            NativeArtifactRole::CSharpNuGetConfig,
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
                "build",
                PROJECT_FILE,
                "--nologo",
                "--disable-build-servers",
                "--configfile",
                NUGET_CONFIG_FILE,
                "--artifacts-path",
                BUILD_DIRECTORY,
            ])
            .arg(format!("-p:LangVersion={}", language_version(profile)))
            .current_dir(native_work.join(WORK_DIRECTORY))
            .env_clear()
            .env(
                "DOTNET_CLI_HOME",
                native_work.join(WORK_DIRECTORY).join(DOTNET_HOME_DIRECTORY),
            )
            .env(
                "NUGET_PACKAGES",
                native_work
                    .join(WORK_DIRECTORY)
                    .join(NUGET_PACKAGES_DIRECTORY),
            )
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1");
        command
    }

    fn source_via_stdin() -> bool {
        false
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_directory_if_present(
            native_work.join(WORK_DIRECTORY),
            NativeArtifactRole::CSharpWork,
        )
    }
}

const fn language_version(profile: CSharpVersion) -> &'static str {
    match profile {
        CSharpVersion::CSharp10 => "10.0",
        CSharpVersion::CSharp11 => "11.0",
        CSharpVersion::CSharp12 => "12.0",
        CSharpVersion::CSharp13 => "13.0",
        CSharpVersion::CSharp14 => "14.0",
    }
}
