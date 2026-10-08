//! Exercises selected dependency authority with an installed, actual Go tool.

use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
};

use backend_frontend_go::legacy::oracle::{
    GoDependencyClosureFailure, GoOracleChildEnvironment, GoOracleConfiguration,
};
use backend_frontend_go::legacy::{ConfiguredGoOracle, GoImage, GoOracle, OracleError};

fn installed_go() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let requested = std::env::var_os("COMPILER_GO_COMPILER").unwrap_or_else(|| "go".into());
    let output = Command::new(&requested)
        .args(["env", "GOROOT"])
        .env("GOENV", "off")
        .env("GOTOOLCHAIN", "local")
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("installed Go could not report GOROOT").into());
    }
    let goroot = PathBuf::from(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
    let executable = if PathBuf::from(&requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        goroot
            .join("bin")
            .join(if cfg!(windows) { "go.exe" } else { "go" })
    }
    .canonicalize()?;
    Ok((executable, goroot))
}

#[test]
fn real_selected_loader_refreshes_missing_dependencies_without_owner_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    let dependency = root.path().join("dependency");
    std::fs::create_dir(&project)?;
    let project = project.canonicalize()?;
    let source = project.join("main.go");
    std::fs::write(
        project.join("go.mod"),
        "module example.com/consumer\n\ngo 1.23\n\nrequire example.com/dependency v0.0.0\nreplace example.com/dependency => ../dependency\n",
    )?;
    std::fs::write(
        &source,
        "package consumer\nimport \"example.com/dependency\"\nfunc Value() int { return dependency.Value() }\n",
    )?;
    let module_cache = root.path().join("modules");
    std::fs::create_dir(&module_cache)?;
    let (go, goroot) = installed_go()?;
    let build_cache = root.path().join("build-cache");
    let environment =
        GoOracleChildEnvironment::new(go.clone(), goroot, module_cache, build_cache.clone())?;
    let owner: ConfiguredGoOracle = GoOracle::default()
        .with_configuration(GoOracleConfiguration::go_toolchain(go.clone())?)
        .with_child_environment(environment)?;
    let cancelled = AtomicBool::new(false);
    let missing = owner.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert!(!missing.is_complete());
    assert!(matches!(
        owner.authority_image_for_package_with_authority_witness_cancellable(
            &source,
            &project,
            &missing,
            Some(&cancelled)
        ),
        Err(OracleError::DependencyClosureUnavailable {
            failure: GoDependencyClosureFailure::PackageLoad
        })
    ));

    // Set up an ordinary local replacement. The same configured owner must
    // observe it on a new request, without refreshing executable authority.
    std::fs::create_dir(&dependency)?;
    std::fs::write(
        dependency.join("go.mod"),
        "module example.com/dependency\n\ngo 1.23\n",
    )?;
    let dependency_source = dependency.join("value.go");
    std::fs::write(
        &dependency_source,
        "package dependency\nfunc Value() int { return 1 }\n",
    )?;
    let fresh = owner.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert!(fresh.is_complete());
    assert!(
        fresh.requires_local_execution(),
        "host cache paths do not authorize delegation"
    );
    assert_ne!(fresh.identity(), missing.identity());
    let repeated = owner.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert_eq!(
        fresh.identity(),
        repeated.identity(),
        "unchanged actual Go selection is stable"
    );
    let build_cancelled = Arc::new(AtomicBool::new(false));
    let child_cancelled = Arc::clone(&build_cancelled);
    let child_owner = owner.clone();
    let child_source = source.clone();
    let child_project = project.clone();
    let child_witness = fresh.clone();
    let child = std::thread::spawn(move || {
        child_owner.authority_image_for_package_with_authority_witness_cancellable(
            &child_source,
            &child_project,
            &child_witness,
            Some(&child_cancelled),
        )
    });
    let helper_cache = build_cache.join("nudox-go-oracle-v1");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut observed_build = false;
    while std::time::Instant::now() < deadline {
        let staged = std::fs::read_dir(&helper_cache).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("go-oracle-build-")
            })
        });
        #[cfg(target_os = "linux")]
        {
            observed_build = staged && actual_helper_build_child(&go, &helper_cache);
        }
        #[cfg(not(target_os = "linux"))]
        {
            observed_build = staged;
        }
        if observed_build || child.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    build_cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        child.join().expect("actual Go helper worker"),
        Err(OracleError::Cancelled)
    ));
    assert!(
        observed_build,
        "cancellation must interrupt helper preparation (and an observed real Go build child on Linux)"
    );
    assert!(
        std::fs::read_dir(&helper_cache)?
            .filter_map(Result::ok)
            .all(|entry| !entry.path().join("manifest.txt").exists()),
        "cancelled helper cannot publish an admitted cache entry"
    );
    let image = owner.authority_image_for_package_with_authority_witness_cancellable(
        &source,
        &project,
        &fresh,
        Some(&cancelled),
    )?;
    let image = GoImage::open(&image)?;
    assert!(image.packages().any(|package| {
        package.is_ok_and(|package| package.import_path == b"example.com/consumer")
    }));

    std::fs::write(
        &dependency_source,
        "package dependency\nfunc Value() int { return 2 }\n",
    )?;
    assert!(!fresh.matches_current_cancellable(&project, Some(&cancelled))?);
    assert!(matches!(
        owner.authority_image_for_package_with_authority_witness_cancellable(
            &source,
            &project,
            &fresh,
            Some(&cancelled)
        ),
        Err(OracleError::PackageAuthorityWitnessChanged)
    ));
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        owner.authority_image_for_package_with_authority_witness_cancellable(
            &source,
            &project,
            &fresh,
            Some(&cancelled)
        ),
        Err(OracleError::Cancelled)
    ));
    Ok(())
}

#[cfg(target_os = "linux")]
fn actual_helper_build_child(go: &std::path::Path, helper_cache: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            return false;
        };
        let parent = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<u32>().ok())
        });
        if parent != Some(std::process::id())
            || path.join("exe").canonicalize().ok().as_deref() != Some(go)
        {
            return false;
        }
        let Ok(argv) = std::fs::read(path.join("cmdline")) else {
            return false;
        };
        let arguments = argv.split(|byte| *byte == 0).collect::<Vec<_>>();
        arguments.get(1).copied() == Some(b"build".as_slice())
            && arguments.iter().any(|arg| *arg == b"-mod=vendor")
            && arguments.iter().any(|arg| {
                std::path::Path::new(std::ffi::OsStr::from_bytes(arg)).starts_with(helper_cache)
            })
    })
}
