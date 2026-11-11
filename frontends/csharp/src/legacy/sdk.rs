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

/// Owning selected reference custody. Paths remain valid only while this
/// object is retained by the same compile attempt and replay witness.
#[derive(Debug)]
pub struct CSharpResolvedReferenceSnapshot {
    directory: tempfile::TempDir,
    references: Box<[PathBuf]>,
    digest: [u8; 32],
}

impl CSharpResolvedReferenceSnapshot {
    pub fn references(&self) -> &[PathBuf] {
        &self.references
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn root(&self) -> &Path {
        self.directory.path()
    }
}

/// Ask the selected SDK's ResolveAssemblyReference target to choose among
/// package/framework candidates. The generated project has no user imports or
/// restore inputs; only explicit, confined HintPath references are admitted.
/// Missing or ambiguous SDK metadata is a hard failure.
pub fn resolve_references_with_sdk(
    dotnet: &Path,
    target_framework: &str,
    candidates: &[PathBuf],
    assets_file: Option<&Path>,
    assets_digest: Option<[u8; 32]>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<CSharpResolvedReferenceSnapshot, &'static str> {
    if candidates.is_empty()
        || candidates.len() > 8192
        || !target_framework.starts_with("net")
        || target_framework.len() > 32
        || !target_framework[3..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err("invalid SDK reference candidates");
    }
    let scratch = tempfile::tempdir().map_err(|_| "SDK scratch unavailable")?;
    let candidate_root = scratch.path().join("candidates");
    fs::create_dir(&candidate_root).map_err(|_| "SDK snapshot unavailable")?;
    let mut captured_candidates = Vec::with_capacity(candidates.len());
    let mut captured_bytes = 0usize;
    for (index, source) in candidates.iter().enumerate() {
        if !source.is_absolute() || !source.is_file() {
            return Err("SDK candidate unavailable");
        }
        let bytes =
            backend_platform::durable::read_regular_bounded_stable(source, 32 * 1024 * 1024)
                .map_err(|_| "SDK candidate unavailable")?;
        captured_bytes = captured_bytes
            .checked_add(bytes.len())
            .ok_or("SDK snapshot limit")?;
        if captured_bytes > 256 * 1024 * 1024 {
            return Err("SDK snapshot limit");
        }
        let name = source.file_name().ok_or("SDK candidate filename")?;
        let slot = candidate_root.join(index.to_string());
        fs::create_dir(&slot).map_err(|_| "SDK snapshot unavailable")?;
        let destination = slot.join(name);
        fs::write(&destination, bytes).map_err(|_| "SDK snapshot unavailable")?;
        captured_candidates.push(destination);
    }
    let candidates = captured_candidates.as_slice();
    if let Some(assets_file) = assets_file {
        if !assets_file.is_absolute() || !assets_file.is_file() {
            return Err("SDK assets unavailable");
        }
        let obj = scratch.path().join("obj");
        fs::create_dir(&obj).map_err(|_| "SDK scratch unavailable")?;
        let bytes =
            backend_platform::durable::read_regular_bounded_stable(assets_file, 64 * 1024 * 1024)
                .map_err(|_| "SDK assets unavailable")?;
        if let Some(expected) = assets_digest {
            use sha2::{Digest, Sha256};
            if Sha256::digest(&bytes).as_slice() != expected {
                return Err("SDK assets digest changed");
            }
        }
        fs::write(obj.join("project.assets.json"), bytes).map_err(|_| "SDK scratch unavailable")?;
    }
    let project = scratch.path().join("resolve.csproj");
    let mut xml = format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>{target_framework}</TargetFramework><ImportDirectoryBuildProps>false</ImportDirectoryBuildProps><ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup><ItemGroup>"
    );
    for path in candidates {
        if !path.is_absolute() || !path.is_file() {
            return Err("SDK candidate unavailable");
        }
        let path = path.to_str().ok_or("SDK candidate path encoding")?;
        let name = path
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_suffix(".dll"))
            .ok_or("SDK candidate filename")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
        {
            return Err("SDK candidate filename");
        }
        let escaped_path = path
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        xml.push_str(&format!("<Reference Include=\"{name}\"><HintPath>{escaped_path}</HintPath><Private>false</Private></Reference>"));
    }
    xml.push_str("</ItemGroup></Project>");
    fs::write(&project, xml).map_err(|_| "SDK scratch unavailable")?;
    let args = vec![
        OsString::from("msbuild"),
        project.as_os_str().to_owned(),
        OsString::from("-nologo"),
        OsString::from("-noAutoResponse"),
        OsString::from("-nodeReuse:false"),
        OsString::from("-target:ResolveReferences"),
        OsString::from("-getItem:ReferencePath"),
        OsString::from("-p:RestoreSources="),
        OsString::from("-p:MSBuildEnableWorkloadResolver=false"),
        OsString::from("-p:ImportDirectoryBuildProps=false"),
        OsString::from("-p:ImportDirectoryBuildTargets=false"),
    ];
    let command = CaptureCommand {
        program: dotnet.as_os_str().to_owned(),
        args,
        cwd: Some(scratch.path().to_path_buf()),
        environment: CaptureEnvironment::Clear,
        overrides: [
            ("HOME".into(), Some(scratch.path().as_os_str().to_owned())),
            (
                "DOTNET_CLI_HOME".into(),
                Some(scratch.path().as_os_str().to_owned()),
            ),
            ("DOTNET_NOLOGO".into(), Some(OsString::from("1"))),
        ]
        .into_iter()
        .collect(),
    };
    let output = capture(
        &command,
        CaptureLimits {
            deadline,
            stdout_bytes: 512 * 1024,
            stderr_bytes: 1024 * 1024,
        },
        cancelled,
    )
    .map_err(|_| "SDK resolution failed")?;
    if !output.status.success() {
        return Err("SDK resolution failed");
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "SDK resolution malformed")?;
    let items = value
        .get("Items")
        .and_then(Value::as_object)
        .and_then(|o| o.get("ReferencePath"))
        .and_then(Value::as_array)
        .ok_or("SDK resolution missing ReferencePath")?;
    let mut selected = Vec::new();
    for item in items {
        let path = item
            .get("FullPath")
            .or_else(|| item.get("Identity"))
            .and_then(Value::as_str)
            .ok_or("SDK resolution path missing")?;
        let path = fs::canonicalize(path).map_err(|_| "SDK resolution path unavailable")?;
        if !candidates
            .iter()
            .any(|candidate| fs::canonicalize(candidate).ok().as_ref() == Some(&path))
        {
            return Err("SDK selected an unrequested reference");
        }
        if selected.contains(&path) {
            return Err("SDK selected duplicate reference");
        }
        selected.push(path);
    }
    if selected.is_empty() {
        return Err("SDK selected no references");
    }
    let selected_dir = scratch.path().join("selected");
    fs::create_dir(&selected_dir).map_err(|_| "SDK snapshot unavailable")?;
    let mut owned = Vec::with_capacity(selected.len());
    for source in selected {
        let name = source.file_name().ok_or("SDK snapshot filename")?;
        let destination = selected_dir.join(name);
        let bytes =
            backend_platform::durable::read_regular_bounded_stable(&source, 32 * 1024 * 1024)
                .map_err(|_| "SDK snapshot unavailable")?;
        fs::write(&destination, bytes).map_err(|_| "SDK snapshot unavailable")?;
        owned.push(destination);
    }
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"csharp.sdk-reference-snapshot.v2\0");
    hash.update((owned.len() as u64).to_le_bytes());
    for path in &owned {
        let name = path.file_name().unwrap_or_default().as_encoded_bytes();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        let bytes = fs::read(path).map_err(|_| "SDK snapshot unavailable")?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(CSharpResolvedReferenceSnapshot {
        directory: scratch,
        references: owned.into_boxed_slice(),
        digest: hash.finalize().into(),
    })
}

/// Private copies of the observed targeting assemblies used by one compile.
/// This does not authenticate the SDK, host, runtime, or observation process.
#[derive(Debug)]
pub struct CSharpTargetingSnapshot {
    directory: tempfile::TempDir,
    directories: Vec<PathBuf>,
    digest: [u8; 32],
    references: Vec<CSharpTargetingReference>,
}

/// One exact reference assembly retained in a targeting snapshot.
#[derive(Debug, Eq, PartialEq)]
pub struct CSharpTargetingReference {
    pub name: String,
    pub length: u64,
    pub sha256: [u8; 32],
}

/// Joins restored package references with the SDK targeting set without
/// inventing a precedence rule. A package may repeat an SDK assembly only
/// when its bytes are exactly the same; otherwise compilation must refuse
/// until an SDK-owned resolution is available.
pub fn resolve_sdk_owned_references(
    sdk: &[CSharpTargetingReference],
    restored: &[CSharpTargetingReference],
) -> Result<Vec<CSharpTargetingReference>, &'static str> {
    use std::collections::BTreeMap;
    let mut merged = BTreeMap::<String, CSharpTargetingReference>::new();
    for reference in sdk.iter().chain(restored) {
        let key = reference.name.to_ascii_lowercase();
        if let Some(existing) = merged.get(&key) {
            if existing.length != reference.length || existing.sha256 != reference.sha256 {
                return Err("SDK/package assembly conflict requires SDK-owned resolution");
            }
            continue;
        }
        merged.insert(
            key,
            CSharpTargetingReference {
                name: reference.name.clone(),
                length: reference.length,
                sha256: reference.sha256,
            },
        );
    }
    Ok(merged.into_values().collect())
}

impl CSharpTargetingSnapshot {
    /// Capture bounded assembly bytes once, so every source invocation consumes
    /// the same selected targeting set even if the installed pack changes.
    pub fn capture(directories: &[PathBuf]) -> Result<Self, &'static str> {
        use sha2::{Digest, Sha256};
        if directories.is_empty() || directories.len() > 16 {
            return Err("invalid targeting directories");
        }
        let directory = tempfile::tempdir().map_err(|_| "targeting snapshot unavailable")?;
        let mut copies = Vec::new();
        let mut hash = Sha256::new();
        hash.update(b"csharp.targeting-snapshot.v1\0");
        let mut total = 0usize;
        let mut count = 0usize;
        let mut references = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for (index, source) in directories.iter().enumerate() {
            let entries = targeting_entries(source)?;
            let destination = directory.path().join(index.to_string());
            fs::create_dir(&destination).map_err(|_| "targeting snapshot unavailable")?;
            hash.update((index as u64).to_le_bytes());
            for name in &entries {
                let stem = name
                    .strip_suffix(".dll")
                    .ok_or("targeting filename unavailable")?;
                if !stem.is_ascii() || stem.is_empty() {
                    return Err("targeting filename unavailable");
                }
                let lower = stem.to_ascii_lowercase();
                if lower.starts_with("api-ms-")
                    || lower.starts_with("clr")
                    || lower == "hostfxr"
                    || lower == "hostpolicy"
                    || lower == "microsoft.diasymreader.native.amd64"
                {
                    continue;
                }
                if !names.insert(lower) {
                    return Err("duplicate targeting assembly");
                }
                count = count.checked_add(1).ok_or("targeting snapshot limit")?;
                if count > 8192 {
                    return Err("targeting snapshot limit");
                }
                let bytes = backend_platform::durable::read_regular_bounded_stable(
                    &source.join(name),
                    32 * 1024 * 1024,
                )
                .map_err(|_| "targeting assembly unavailable")?;
                total = total
                    .checked_add(bytes.len())
                    .ok_or("targeting snapshot limit")?;
                if bytes.is_empty() || total > 256 * 1024 * 1024 {
                    return Err("targeting snapshot limit");
                }
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name.as_bytes());
                hash.update((bytes.len() as u64).to_le_bytes());
                let sha256: [u8; 32] = Sha256::digest(&bytes).into();
                hash.update(sha256);
                references.push(CSharpTargetingReference {
                    name: stem.to_owned(),
                    length: bytes.len() as u64,
                    sha256,
                });
                fs::write(destination.join(name), bytes)
                    .map_err(|_| "targeting snapshot unavailable")?;
            }
            if entries != targeting_entries(source)? {
                return Err("targeting directory changed");
            }
            copies.push(destination);
        }
        if count == 0 {
            return Err("targeting assemblies unavailable");
        }
        Ok(Self {
            directory,
            directories: copies,
            digest: hash.finalize().into(),
            references,
        })
    }

    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn references(&self) -> &[CSharpTargetingReference] {
        &self.references
    }
    /// Keep the private directory alive for the complete compilation.
    pub fn root(&self) -> &Path {
        self.directory.path()
    }
}

fn targeting_entries(directory: &Path) -> Result<Vec<String>, &'static str> {
    let mut entries = Vec::new();
    for (count, entry) in fs::read_dir(directory)
        .map_err(|_| "targeting directory unavailable")?
        .enumerate()
    {
        if count >= 16384 {
            return Err("targeting directory limit");
        }
        let entry = entry.map_err(|_| "targeting directory unavailable")?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "targeting filename unavailable")?;
        // .NET's glob is case-sensitive on Unix and case-insensitive on Windows.
        // Reject mixed-case extensions rather than capture a platform-dependent set.
        if name.to_ascii_lowercase().ends_with(".dll") && !name.ends_with(".dll") {
            return Err("targeting extension differs by platform");
        }
        if name.ends_with(".dll") {
            if entries.len() >= 8192 {
                return Err("targeting snapshot limit");
            }
            entries.push(name);
        }
    }
    entries.sort_unstable();
    Ok(entries)
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
    /// Known shared frameworks required by the restored project-reference graph.
    pub framework_references: Vec<String>,
}

impl CSharpSdkProjectProperties {
    fn xml(&self) -> Result<String, &'static str> {
        let mut xml = String::new();
        for (name, value, allowed) in [
            (
                "OutputType",
                &self.output_type,
                &["Exe", "Library", "WinExe"][..],
            ),
            (
                "Nullable",
                &self.nullable,
                &["enable", "disable", "warnings", "annotations"][..],
            ),
            (
                "AllowUnsafeBlocks",
                &self.allow_unsafe_blocks,
                &["true", "false"][..],
            ),
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
    let mut frameworks = String::new();
    for name in &properties.framework_references {
        if !matches!(
            name.as_str(),
            "Microsoft.NETCore.App" | "Microsoft.AspNetCore.App"
        ) {
            return Err("unsupported restored framework reference");
        }
        if name == "Microsoft.AspNetCore.App" && matches!(kind, CSharpSdkKind::Standard) {
            frameworks =
                "<ItemGroup><FrameworkReference Include=\"Microsoft.AspNetCore.App\"/></ItemGroup>"
                    .to_owned();
        }
    }
    let xml = format!(
        "<Project Sdk=\"{sdk}\"><PropertyGroup><TargetFramework>{target_framework}</TargetFramework><ImplicitUsings>{enabled}</ImplicitUsings>{extra}<ImportDirectoryBuildProps>false</ImportDirectoryBuildProps><ImportDirectoryBuildTargets>false</ImportDirectoryBuildTargets></PropertyGroup>{frameworks}</Project>"
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
    fn targeting_snapshot_retains_bytes_and_changes_identity() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("Core.dll"), b"first").unwrap();
        let first = CSharpTargetingSnapshot::capture(&[source.path().to_path_buf()]).unwrap();
        fs::write(source.path().join("Core.dll"), b"second").unwrap();
        let second = CSharpTargetingSnapshot::capture(&[source.path().to_path_buf()]).unwrap();
        assert_eq!(
            fs::read(first.directories()[0].join("Core.dll")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(second.directories()[0].join("Core.dll")).unwrap(),
            b"second"
        );
        assert_ne!(first.digest(), second.digest());
        fs::write(source.path().join("Other.dll"), b"third").unwrap();
        let third = CSharpTargetingSnapshot::capture(&[source.path().to_path_buf()]).unwrap();
        assert_ne!(second.digest(), third.digest());
    }

    #[test]
    fn targeting_snapshot_refuses_duplicate_reference_names() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::write(first.path().join("Core.dll"), b"first").unwrap();
        fs::write(second.path().join("core.dll"), b"second").unwrap();
        assert!(
            CSharpTargetingSnapshot::capture(&[
                first.path().to_path_buf(),
                second.path().to_path_buf()
            ])
            .is_err()
        );
    }

    #[test]
    fn sdk_owned_reference_join_rejects_different_package_overlap() {
        let sdk = CSharpTargetingReference {
            name: "System.Runtime".into(),
            length: 3,
            sha256: [1; 32],
        };
        let identical = CSharpTargetingReference {
            name: "system.runtime".into(),
            length: 3,
            sha256: [1; 32],
        };
        let different = CSharpTargetingReference {
            name: "System.Runtime".into(),
            length: 4,
            sha256: [2; 32],
        };
        let joined = resolve_sdk_owned_references(&[sdk], &[identical]).unwrap();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].name, "System.Runtime");
        assert!(resolve_sdk_owned_references(&joined, &[different]).is_err());
    }

    #[test]
    fn targeting_snapshot_excludes_native_host_shims() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("Core.dll"), b"reference").unwrap();
        fs::write(source.path().join("hostfxr.dll"), b"native").unwrap();
        let snapshot = CSharpTargetingSnapshot::capture(&[source.path().to_path_buf()]).unwrap();
        assert_eq!(snapshot.references().len(), 1);
        assert_eq!(snapshot.references()[0].name, "Core");
        assert!(!snapshot.directories()[0].join("hostfxr.dll").exists());
    }

    #[cfg(unix)]
    #[test]
    fn targeting_snapshot_refuses_symlinked_assemblies() {
        let source = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("Core.dll"), b"reference").unwrap();
        std::os::unix::fs::symlink(
            other.path().join("Core.dll"),
            source.path().join("Core.dll"),
        )
        .unwrap();
        assert!(CSharpTargetingSnapshot::capture(&[source.path().to_path_buf()]).is_err());
    }

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
