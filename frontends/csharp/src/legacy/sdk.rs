//! Bounded SDK observation using a generated project; never evaluates user project XML.
//! The result is a claim, not proof of MSBuild closure or selected compiler authority.
use backend_platform::child_output::{CaptureCommand, CaptureEnvironment, CaptureLimits, capture};
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

// A deliberately narrow global.json grammar prevents custom SDK search paths
// and rejects duplicate keys before handing the same bytes to dotnet.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalJson {
    #[serde(rename = "sdk")]
    _sdk: Option<GlobalSdk>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalSdk {
    #[serde(rename = "version")]
    _version: Option<String>,
    #[serde(rename = "rollForward")]
    _roll_forward: Option<String>,
    #[serde(rename = "allowPrerelease")]
    _allow_prerelease: Option<bool>,
}

/// Supported built-in SDKs for the private observation project.
#[derive(Clone, Copy, Debug)]
pub enum CSharpSdkKind {
    Standard,
    Web,
}
/// One captured global.json path and contents, or an observed absence.
#[derive(Clone, Copy, Debug)]
pub struct CSharpSdkProbe<'a> {
    pub path: &'a str,
    pub text: Option<&'a str>,
}
/// Observed SDK targeting inputs; no complete project-input claim is implied.
#[derive(Debug)]
pub struct CSharpSdkObservation {
    pub sdk_directory: PathBuf,
    /// Framework requested of the SDK for this observation.
    pub target_framework: String,
    /// SDK-observed nullable compiler mode.
    pub nullable: String,
    /// SDK-observed unsafe compiler option.
    pub allow_unsafe: bool,
    /// SDK-observed language version.
    pub language_version: String,
    /// SDK-observed compiler output type.
    pub output_type: String,
    pub targeting_directories: Vec<PathBuf>,
    pub usings: Vec<String>,
    /// SDK-produced preprocessor symbols for this observation.
    pub defines: Vec<String>,
    /// Whether selection may differ because original ancestors outside the captured root are unknown.
    pub outside_root_unknown: bool,
}

/// Observes the selected installed SDK with a private, generated project and bounded process.
/// Callers must separately authenticate captures and admit the actual SDK/pack bytes.
pub fn observe_csharp_sdk(
    dotnet: &Path,
    project_path: &str,
    probes: &[CSharpSdkProbe<'_>],
    kind: CSharpSdkKind,
    target_framework: &str,
    implicit_usings: bool,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<CSharpSdkObservation, &'static str> {
    observe_csharp_sdk_with(
        dotnet,
        project_path,
        probes,
        kind,
        target_framework,
        implicit_usings,
        &CSharpSdkProjectProperties::default(),
        deadline,
        cancelled,
    )
}

/// Project properties the generated observation project carries over from the
/// captured project. Each is a closed enumeration; nothing free-form is forwarded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CSharpSdkProjectProperties {
    /// `Exe`, `Library` or `WinExe`.
    pub output_type: Option<String>,
    /// `enable`, `disable`, `warnings` or `annotations`.
    pub nullable: Option<String>,
    /// `true` or `false`.
    pub allow_unsafe_blocks: Option<String>,
}

impl CSharpSdkProjectProperties {
    fn xml(&self) -> Result<String, &'static str> {
        let mut xml = String::new();
        for (name, value, allowed) in [
            ("OutputType", &self.output_type, &["Exe", "Library", "WinExe"][..]),
            ("Nullable", &self.nullable, &["enable", "disable", "warnings", "annotations"][..]),
            ("AllowUnsafeBlocks", &self.allow_unsafe_blocks, &["true", "false"][..]),
        ] {
            if let Some(value) = value {
                if !allowed.contains(&value.as_str()) {
                    return Err("unsupported SDK project property");
                }
                xml.push_str(&format!("<{name}>{value}</{name}>"));
            }
        }
        Ok(xml)
    }
}

/// [`observe_csharp_sdk`] for a project that also sets a closed set of
/// compiler-affecting properties.
pub fn observe_csharp_sdk_with(
    dotnet: &Path,
    project_path: &str,
    probes: &[CSharpSdkProbe<'_>],
    kind: CSharpSdkKind,
    target_framework: &str,
    implicit_usings: bool,
    properties: &CSharpSdkProjectProperties,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<CSharpSdkObservation, &'static str> {
    if !dotnet.is_absolute()
        || project_path.len() > 4096
        || !project_path.ends_with(".csproj")
        || project_path.starts_with('/')
        || project_path.contains('\\')
        || project_path.bytes().any(|b| b.is_ascii_control())
        || (project_path.as_bytes().get(1) == Some(&b':')
            && project_path.as_bytes()[0].is_ascii_alphabetic())
        || project_path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || target_framework.len() > 32
        || !target_framework.starts_with("net")
        || !target_framework[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
        || target_framework.len() <= 3
    {
        return Err("invalid SDK request");
    }
    let Some((major, minor)) = target_framework[3..].split_once('.') else {
        return Err("unsupported SDK framework");
    };
    if major.parse::<u16>().ok().is_none_or(|major| major < 5) || minor.parse::<u16>().is_err() {
        return Err("unsupported SDK framework");
    }
    let mut expected = Vec::new();
    let mut parent = project_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    for _ in 0..8 {
        expected.push(if parent.is_empty() {
            "global.json".to_owned()
        } else {
            format!("{parent}/global.json")
        });
        if parent.is_empty() {
            break;
        }
        parent = parent.rsplit_once('/').map_or("", |(dir, _)| dir);
    }
    if expected.last().is_none_or(|path| path != "global.json")
        || probes.len() != expected.len()
        || probes
            .iter()
            .zip(&expected)
            .any(|(probe, path)| probe.path != path)
    {
        return Err("invalid SDK probes");
    }
    let mut size = 0usize;
    for probe in probes {
        size = size
            .checked_add(probe.path.len())
            .and_then(|v| v.checked_add(probe.text.map_or(0, str::len)))
            .ok_or("SDK probe limit")?;
        if size > 1024 * 1024 {
            return Err("SDK probe limit");
        }
        if let Some(text) = probe.text {
            let _: GlobalJson =
                serde_json::from_str(text).map_err(|_| "unsupported global.json")?;
        }
    }
    let scratch = tempfile::tempdir().map_err(|_| "SDK scratch unavailable")?;
    let root = scratch.path();
    for probe in probes {
        if let Some(text) = probe.text {
            let path = root.join(probe.path);
            fs::create_dir_all(path.parent().ok_or("invalid SDK probe path")?)
                .map_err(|_| "SDK scratch unavailable")?;
            fs::write(path, text).map_err(|_| "SDK scratch unavailable")?;
        }
    }
    // Prevent an unrelated global.json above the private root from selecting a different SDK.
    if probes.last().is_some_and(|probe| probe.text.is_none()) {
        fs::write(root.join("global.json"), "{}").map_err(|_| "SDK scratch unavailable")?;
    }
    let project = root.join(project_path);
    fs::create_dir_all(project.parent().ok_or("invalid SDK project")?)
        .map_err(|_| "SDK scratch unavailable")?;
    let sdk = match kind {
        CSharpSdkKind::Standard => "Microsoft.NET.Sdk",
        CSharpSdkKind::Web => "Microsoft.NET.Sdk.Web",
    };
    let enabled = if implicit_usings { "enable" } else { "disable" };
    let extra = properties.xml()?;
    let xml = format!(
        "<Project Sdk=\"{sdk}\"><PropertyGroup><TargetFramework>{target_framework}</TargetFramework><ImplicitUsings>{enabled}</ImplicitUsings>{extra}<ImportDirectoryBuildProps>false</ImportDirectoryBuildProps><ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup></Project>"
    );
    fs::write(&project, xml).map_err(|_| "SDK scratch unavailable")?;
    let command = CaptureCommand {
        program: dotnet.as_os_str().to_owned(),
        args: [
            "msbuild",
            project.to_str().ok_or("SDK path encoding")?,
            "-nologo",
            "-noAutoResponse",
            "-nodeReuse:false",
            "-maxcpucount:2",
            "-target:ProcessFrameworkReferences,AddImplicitDefineConstants",
            "-getProperty:MSBuildSDKsPath,NETCoreTargetingPackRoot,TargetFramework,DefineConstants,Nullable,AllowUnsafeBlocks,LangVersion,OutputType",
            "-getItem:TargetingPack,Using",
            "-p:RestoreSources=",
            "-p:MSBuildEnableWorkloadResolver=false",
            "-p:ImportDirectoryBuildProps=false",
            "-p:ImportDirectoryBuildTargets=false",
            "-p:ImportDirectoryPackagesProps=false",
            "-p:ImportByWildcardBeforeMicrosoftCommonProps=false",
            "-p:ImportByWildcardAfterMicrosoftCommonProps=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonProps=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonProps=false",
            "-p:ImportByWildcardBeforeMicrosoftCommonTargets=false",
            "-p:ImportByWildcardAfterMicrosoftCommonTargets=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonTargets=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonTargets=false",
            "-p:ImportByWildcardBeforeMicrosoftCSharpTargets=false",
            "-p:ImportByWildcardAfterMicrosoftCSharpTargets=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftCSharpTargets=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftCSharpTargets=false",
            "-p:ImportByWildcardBeforeMicrosoftCommonCrossTargetingTargets=false",
            "-p:ImportByWildcardAfterMicrosoftCommonCrossTargetingTargets=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftCommonCrossTargetingTargets=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftCommonCrossTargetingTargets=false",
            "-p:ImportByWildcardBeforeMicrosoftNetFrameworkProps=false",
            "-p:ImportByWildcardAfterMicrosoftNetFrameworkProps=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftNetFrameworkProps=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftNetFrameworkProps=false",
            "-p:ImportByWildcardBeforeMicrosoftNetFrameworkTargets=false",
            "-p:ImportByWildcardAfterMicrosoftNetFrameworkTargets=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftNetFrameworkTargets=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftNetFrameworkTargets=false",
            "-p:ImportByWildcardBeforeMicrosoftVisualBasicTargets=false",
            "-p:ImportByWildcardAfterMicrosoftVisualBasicTargets=false",
            "-p:ImportUserLocationsByWildcardBeforeMicrosoftVisualBasicTargets=false",
            "-p:ImportUserLocationsByWildcardAfterMicrosoftVisualBasicTargets=false",
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
        cwd: Some(
            project
                .parent()
                .ok_or("SDK project directory")?
                .to_path_buf(),
        ),
        environment: CaptureEnvironment::Clear,
        overrides: [
            ("HOME", root.as_os_str()),
            ("DOTNET_CLI_HOME", root.as_os_str()),
            ("NUGET_PACKAGES", root.as_os_str()),
            ("DOTNET_CLI_TELEMETRY_OPTOUT", std::ffi::OsStr::new("1")),
            (
                "DOTNET_SKIP_FIRST_TIME_EXPERIENCE",
                std::ffi::OsStr::new("1"),
            ),
            ("DOTNET_NOLOGO", std::ffi::OsStr::new("1")),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), Some(value.to_owned())))
        .collect(),
    };
    let output = capture(
        &command,
        CaptureLimits {
            deadline,
            stdout_bytes: 256 * 1024,
            stderr_bytes: 1024 * 1024,
        },
        cancelled,
    )
    .map_err(|_| "SDK observation failed")?;
    if !output.status.success() {
        return Err("SDK observation failed");
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "SDK observation malformed")?;
    let properties = value
        .get("Properties")
        .and_then(Value::as_object)
        .ok_or("SDK observation malformed")?;
    if properties.get("TargetFramework").and_then(Value::as_str) != Some(target_framework) {
        return Err("SDK framework mismatch");
    }
    let sdk_directory = fs::canonicalize(
        properties
            .get("MSBuildSDKsPath")
            .and_then(Value::as_str)
            .ok_or("SDK path missing")?,
    )
    .map_err(|_| "SDK path unavailable")?;
    let pack_root = fs::canonicalize(
        properties
            .get("NETCoreTargetingPackRoot")
            .and_then(Value::as_str)
            .ok_or("SDK pack root missing")?,
    )
    .map_err(|_| "SDK pack root unavailable")?;
    let nullable = match properties
        .get("Nullable")
        .and_then(Value::as_str)
        .ok_or("SDK nullable missing")?
    {
        "" | "disable" => "disable",
        "enable" => "enable",
        "warnings" => "warnings",
        "annotations" => "annotations",
        _ => return Err("SDK nullable unsupported"),
    }
    .to_owned();
    let allow_unsafe = match properties
        .get("AllowUnsafeBlocks")
        .and_then(Value::as_str)
        .ok_or("SDK unsafe option missing")?
    {
        "" | "false" | "False" => false,
        "true" | "True" => true,
        _ => return Err("SDK unsafe option unsupported"),
    };
    let language_version = properties
        .get("LangVersion")
        .and_then(Value::as_str)
        .ok_or("SDK language version missing")?;
    if language_version.is_empty()
        || language_version.len() > 32
        || !language_version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err("SDK language version unsupported");
    }
    let output_type = properties
        .get("OutputType")
        .and_then(Value::as_str)
        .ok_or("SDK output type missing")?;
    if !matches!(output_type, "Exe" | "Library" | "WinExe") {
        return Err("SDK output type unsupported");
    }
    let defines_text = properties
        .get("DefineConstants")
        .and_then(Value::as_str)
        .ok_or("SDK defines missing")?;
    if defines_text.len() > 64 * 1024 {
        return Err("SDK defines limit");
    }
    let mut defines = Vec::new();
    for symbol in defines_text.split(';').filter(|symbol| !symbol.is_empty()) {
        if symbol.len() > 256
            || !symbol
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !symbol.as_bytes()[0].is_ascii_alphabetic() && symbol.as_bytes()[0] != b'_'
            || defines.len() >= 1024
        {
            return Err("SDK define invalid");
        }
        defines.push(symbol.to_owned());
    }
    let items = value
        .get("Items")
        .and_then(Value::as_object)
        .ok_or("SDK observation malformed")?;
    let packs = items
        .get("TargetingPack")
        .and_then(Value::as_array)
        .ok_or("SDK packs missing")?;
    if packs.is_empty() || packs.len() > 16 {
        return Err("SDK packs invalid");
    }
    let mut targeting_directories = Vec::new();
    for pack in packs {
        if pack.get("TargetFramework").and_then(Value::as_str) != Some(target_framework) {
            return Err("SDK pack framework mismatch");
        }
        let path = fs::canonicalize(
            pack.get("Path")
                .and_then(Value::as_str)
                .ok_or("SDK pack unavailable")?,
        )
        .map_err(|_| "SDK pack unavailable")?;
        if !path.starts_with(&pack_root) {
            return Err("SDK pack outside selected root");
        }
        let reference = path.join("ref");
        let mut dirs =
            fs::read_dir(reference).map_err(|_| "SDK reference directory unavailable")?;
        let selected = dirs
            .next()
            .ok_or("SDK reference directory unavailable")?
            .map_err(|_| "SDK reference directory unavailable")?
            .path();
        if dirs.next().is_some() || !selected.is_dir() {
            return Err("SDK reference directory ambiguous");
        }
        let selected =
            fs::canonicalize(selected).map_err(|_| "SDK reference directory unavailable")?;
        if !selected.starts_with(&path) || targeting_directories.contains(&selected) {
            return Err("SDK reference directory invalid");
        }
        targeting_directories.push(selected);
    }
    let using_items = items
        .get("Using")
        .and_then(Value::as_array)
        .ok_or("SDK usings missing")?;
    if using_items.len() > 8192 {
        return Err("SDK usings limit");
    }
    let mut usings = Vec::new();
    for item in using_items {
        let name = item
            .get("Identity")
            .and_then(Value::as_str)
            .ok_or("SDK using invalid")?;
        if name.is_empty()
            || name.len() > 4096
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
            || item
                .get("Alias")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
            || item
                .get("Static")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty() && v != "false")
        {
            return Err("SDK using unsupported");
        }
        usings.push(name.to_owned());
    }
    Ok(CSharpSdkObservation {
        sdk_directory,
        target_framework: target_framework.to_owned(),
        nullable,
        allow_unsafe,
        language_version: language_version.to_owned(),
        output_type: output_type.to_owned(),
        targeting_directories,
        usings,
        defines,
        outside_root_unknown: probes.iter().all(|probe| probe.text.is_none()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn global_json_refuses_duplicate_and_unknown_sdk_keys() {
        assert!(
            serde_json::from_str::<GlobalJson>(
                r#"{"sdk":{"version":"10.0.403","rollForward":"disable"}}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<GlobalJson>(r#"{"sdk":{},"sdk":{"version":"10.0.403"}}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<GlobalJson>(
                r#"{"sdk":{"version":"10.0.403","version":"9.0.100"}}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<GlobalJson>(r#"{"sdk":{"paths":["/tmp"]}}"#).is_err());
        assert!(serde_json::from_str::<GlobalJson>(r#"{"sdk":{},"unknown":true}"#).is_err());
    }

    /// Run with NUDOX_TEST_DOTNET set to a private .NET 10 host.
    #[test]
    #[ignore = "requires explicitly selected private .NET SDK"]
    fn observes_real_web_packs_usings_and_global_sdk_failure() {
        let dotnet = PathBuf::from(std::env::var_os("NUDOX_TEST_DOTNET").expect("private dotnet"));
        let cancelled = AtomicBool::new(false);
        let probes = [
            CSharpSdkProbe {
                path: "src/global.json",
                text: None,
            },
            CSharpSdkProbe {
                path: "global.json",
                text: None,
            },
        ];
        let observation = observe_csharp_sdk(
            &dotnet,
            "src/App.csproj",
            &probes,
            CSharpSdkKind::Web,
            "net10.0",
            true,
            Instant::now() + Duration::from_secs(60),
            &cancelled,
        )
        .unwrap();
        assert_eq!(observation.targeting_directories.len(), 2);
        assert!(
            observation
                .usings
                .iter()
                .any(|item| item == "Microsoft.AspNetCore.Builder")
        );
        assert!(observation.outside_root_unknown);
        assert!(observation.defines.iter().any(|item| item == "NET10_0"));
        let missing = [
            CSharpSdkProbe {
                path: "src/global.json",
                text: Some(r#"{"sdk":{"version":"9999.0.100","rollForward":"disable"}}"#),
            },
            CSharpSdkProbe {
                path: "global.json",
                text: None,
            },
        ];
        assert!(
            observe_csharp_sdk(
                &dotnet,
                "src/App.csproj",
                &missing,
                CSharpSdkKind::Web,
                "net10.0",
                true,
                Instant::now() + Duration::from_secs(60),
                &cancelled
            )
            .is_err()
        );
        let custom_path = [
            CSharpSdkProbe {
                path: "src/global.json",
                text: Some(r#"{"sdk":{"paths":["/tmp"]}}"#),
            },
            CSharpSdkProbe {
                path: "global.json",
                text: None,
            },
        ];
        assert!(
            observe_csharp_sdk(
                &dotnet,
                "src/App.csproj",
                &custom_path,
                CSharpSdkKind::Web,
                "net10.0",
                true,
                Instant::now() + Duration::from_secs(60),
                &cancelled
            )
            .is_err()
        );
    }
}
