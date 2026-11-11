//! Real Roslyn producer checks over a private captured source set.
use backend_frontend_csharp::legacy::{
    CSharpAuthorityConfiguration, CSharpAuthorityControl, CSharpAuthorityError,
    CSharpAuthorityRequest, CSharpCapturedSource, CSharpOracle, probe_dotnet,
};
use backend_semantic::vocabulary::{CSharpVersion, NativeTool};
use std::{
    error::Error,
    fs,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[path = "support/oracle.rs"]
mod oracle;

fn request<'a>(
    root: &'a Path,
    binding: &'a Path,
    dotnet: &'a Path,
    source: &'a str,
    members: &'a [CSharpCapturedSource<'a>],
    cancelled: &'a AtomicBool,
) -> CSharpAuthorityRequest<'a, 'static, 'a> {
    CSharpAuthorityRequest {
        package_root: root,
        source_path: binding,
        project_file: None,
        source: source.as_bytes(),
        captured_sources: Some(members),
        captured_config: None,
        profile: CSharpVersion::CSharp14,
        native_tool: NativeTool::CSharpCompiler,
        toolchain: dotnet,
        control: CSharpAuthorityControl {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled,
        },
        configuration: CSharpAuthorityConfiguration::DEFAULT,
    }
}

#[test]
fn captured_members_bind_cross_file_symbols_and_retain_incomplete_diagnostics()
-> Result<(), Box<dyn Error>> {
    let dotnet = probe_dotnet().map_err(|error| format!("dotnet unavailable: {error:?}"))?;
    let helper = oracle::for_test(&dotnet, "captured-source")?;
    let producer = CSharpOracle::new(helper.path_for_use()?.to_path_buf());
    let root = tempfile::tempdir()?;
    let binding = root.path().join("A.cs");
    let a = "Target.Go();\npublic partial class Program;\n";
    fs::write(&binding, a)?;
    let members = [
        CSharpCapturedSource {
            path: "A.cs",
            text: a,
        },
        CSharpCapturedSource {
            path: "B.cs",
            text: "public static class Target { public static void Go() {} }\n",
        },
    ];
    let cancelled = AtomicBool::new(false);
    let bytes = producer.authority_image(request(
        root.path(),
        &binding,
        &dotnet,
        a,
        &members,
        &cancelled,
    ))?;
    let image = backend_frontend_csharp::legacy::CSharpImage::open(&bytes)?;
    assert_eq!(image.compilation_errors(), 0);
    assert!(
        image
            .references()
            .any(|r| r.is_ok_and(|r| r.owner == u32::MAX))
    );
    let foreign = image
        .references()
        .filter_map(Result::ok)
        .find_map(|row| row.foreign_source)
        .ok_or("cross-file source coordinate missing")?;
    assert_eq!(foreign.path().as_deref(), Some("B.cs"));
    assert_eq!(foreign.documentation_id().as_deref(), Some("M:Target.Go"));
    let target_bytes = members[1].text.as_bytes();
    assert_eq!(
        &target_bytes[foreign.name_start as usize..foreign.name_end as usize],
        b"Go"
    );
    use sha2::{Digest, Sha256};
    assert_eq!(
        foreign.source_sha256,
        <[u8; 32]>::from(Sha256::digest(target_bytes))
    );
    // Exercise the Rust-side same-image check for persisted occurrence claims.
    let config = [
        backend_frontend_csharp::legacy::CSharpCapturedConfig { path: "App.csproj", text: Some("<Project/>") },
        backend_frontend_csharp::legacy::CSharpCapturedConfig { path: "Directory.Build.props", text: None },
        backend_frontend_csharp::legacy::CSharpCapturedConfig { path: "Directory.Build.targets", text: None },
    ];
    let project = root.path().join("App.csproj");
    fs::write(&project, "<Project/>")?;
    let mut captured = request(root.path(), &binding, &dotnet, a, &members, &cancelled);
    captured.project_file = Some(&project);
    captured.captured_config = Some(&config);
    let admitted = producer.produce(captured)?;
    let evidence: serde_json::Value = serde_json::from_slice(admitted.captured_evidence().ok_or("missing occurrence evidence")?)?;
    assert_eq!(evidence["format"], 2);
    assert_eq!(evidence["occurrences"].as_array().ok_or("missing occurrences")?.len(), 1);
    assert_eq!(evidence["occurrences"][0]["owner"], u32::MAX);
    let invalid = "MissingName();\npublic partial class Program;\n";
    fs::write(&binding, invalid)?;
    let bad = [CSharpCapturedSource {
        path: "A.cs",
        text: invalid,
    }];
    let error = producer
        .authority_image(request(
            root.path(),
            &binding,
            &dotnet,
            invalid,
            &bad,
            &cancelled,
        ))
        .expect_err("unbound source must not be published as complete");
    match error {
        CSharpAuthorityError::CompilationIncomplete { count, diagnostic } => {
            assert!(count > 0);
            assert!(diagnostic.contains("CS0103"), "{diagnostic}");
        }
        other => panic!("unexpected failure: {other}"),
    }
    Ok(())
}

#[test]
fn owner_captured_config_overrides_changed_live_project() -> Result<(), Box<dyn Error>> {
    use backend_frontend_csharp::legacy::CSharpCapturedConfig;
    let dotnet = probe_dotnet().map_err(|error| format!("dotnet unavailable: {error:?}"))?;
    let helper = oracle::for_test(&dotnet, "captured-config")?;
    let producer = CSharpOracle::new(helper.path_for_use()?.to_path_buf());
    let root = tempfile::tempdir()?;
    let binding = root.path().join("A.cs");
    let project = root.path().join("App.csproj");
    let source = "public class Sample { StringBuilder value = new(); }";
    fs::write(&binding, source)?;
    fs::write(&project, "<Project />")?;
    fs::write(
        root.path().join("Directory.Build.props"),
        "<Project><ItemGroup><Using Remove=\"System.Text\" /></ItemGroup></Project>",
    )?;
    let members = [CSharpCapturedSource {
        path: "A.cs",
        text: source,
    }];
    let config = [
        CSharpCapturedConfig {
            path: "App.csproj",
            text: Some(
                "<Project><ItemGroup><Using Include=\"System.Text\" /></ItemGroup></Project>",
            ),
        },
        CSharpCapturedConfig {
            path: "Directory.Build.props",
            text: None,
        },
        CSharpCapturedConfig {
            path: "Directory.Build.targets",
            text: None,
        },
    ];
    let cancelled = AtomicBool::new(false);
    let mut request = request(root.path(), &binding, &dotnet, source, &members, &cancelled);
    request.project_file = Some(&project);
    request.captured_config = Some(&config);
    let image = producer.authority_image(request)?;
    assert_eq!(
        backend_frontend_csharp::legacy::CSharpImage::open(&image)?.compilation_errors(),
        0
    );
    let retained = producer.produce(request)?;
    let evidence: serde_json::Value = serde_json::from_slice(
        retained
            .captured_evidence()
            .ok_or("captured evidence missing")?,
    )?;
    assert_eq!(evidence["inputClosure"], "unknown");
    assert_eq!(evidence["generators"], "unknown");
    let bad = [config[0], config[1]];
    request.captured_config = Some(&bad);
    assert!(producer.authority_image(request).is_err());
    Ok(())
}

#[test]
fn private_calls_and_full_source_spans_survive_real_roslyn_capture() -> Result<(), Box<dyn Error>> {
    let dotnet = probe_dotnet().map_err(|error| format!("dotnet unavailable: {error:?}"))?;
    let helper = oracle::for_test(&dotnet, "private-source")?;
    let producer = CSharpOracle::new(helper.path_for_use()?.to_path_buf());
    let root = tempfile::tempdir()?;
    let binding = root.path().join("Accounts.cs");
    let source = "// λ before byte coordinates\npublic static class Accounts {\n    public static void MapAccounts() { MissingFields(); MissingFields(); }\n    private static bool MissingFields() { return false; }\n}\n";
    fs::write(&binding, source)?;
    let members = [CSharpCapturedSource { path: "Accounts.cs", text: source }];
    let cancelled = AtomicBool::new(false);
    let bytes = producer.authority_image(request(root.path(), &binding, &dotnet, source, &members, &cancelled))?;
    let image = backend_frontend_csharp::legacy::CSharpImage::open(&bytes)?;
    assert_eq!(image.compilation_errors(), 0);
    let declarations = image.declarations().collect::<Result<Vec<_>, _>>()?;
    let (target, declaration) = declarations.iter().enumerate()
        .find(|(_, row)| row.name.bytes == b"MissingFields")
        .ok_or("private method missing")?;
    assert_eq!(&source.as_bytes()[declaration.decl_start as usize..declaration.decl_end as usize],
        b"private static bool MissingFields() { return false; }");
    let calls = image.references().collect::<Result<Vec<_>, _>>()?.into_iter()
        .filter(|row| row.target == Some(target as u32)).collect::<Vec<_>>();
    assert_eq!(calls.len(), 2, "both same-file private calls must remain queryable");
    Ok(())
}
