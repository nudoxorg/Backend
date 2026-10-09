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

#[derive(Clone, Default)]
struct CaptureEvents(Arc<std::sync::atomic::AtomicUsize>);

impl tracing::Subscriber for CaptureEvents {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().target() == "compiler::go_authority_capture" {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

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
fn real_selected_loader_binds_inactive_platform_source_without_active_calls()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    std::fs::create_dir(&project)?;
    let project = project.canonicalize()?;
    let (go, goroot) = installed_go()?;
    let output = Command::new(&go)
        .args(["env", "GOOS"])
        .env_remove("GOOS")
        .env("GOENV", "off")
        .env("GOTOOLCHAIN", "local")
        .output()?;
    assert!(
        output.status.success(),
        "genuine Go target discovery failed"
    );
    let native_os = String::from_utf8(output.stdout)?.trim().to_owned();
    assert!(!native_os.is_empty());
    // These are fixture operands; only the actual Go compiler selects which
    // source is active. Rust does not implement Go's filename matching law.
    let other_os = if native_os == "windows" {
        "darwin"
    } else {
        "windows"
    };
    let active = project.join(format!("listener_{native_os}.go"));
    let inactive = project.join(format!("listener_{other_os}.go"));
    let caller = project.join("caller.go");
    std::fs::write(
        project.join("go.mod"),
        "module example.com/native-selection\n\ngo 1.23\n",
    )?;
    std::fs::write(
        &active,
        "package selection\n// Platform is active.\nfunc Platform() int { return 1 }\n",
    )?;
    let inactive_source =
        b"package selection\n// Platform is dormant.\nfunc Platform() int { return 2 }\n";
    std::fs::write(&inactive, inactive_source)?;
    std::fs::write(
        &caller,
        "package selection\nfunc Caller() int { return Platform() }\nvar Inferred = Platform()\n",
    )?;
    let module_cache = root.path().join("modules");
    std::fs::create_dir(&module_cache)?;
    let environment = GoOracleChildEnvironment::new(
        go.clone(),
        goroot,
        module_cache,
        root.path().join("build-cache"),
    )?;
    let configured = || -> Result<ConfiguredGoOracle, Box<dyn std::error::Error>> {
        Ok(GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go.clone())?)
            .with_child_environment(environment.clone())?)
    };
    let owner = configured()?;
    let cancelled = AtomicBool::new(false);
    let witness = owner.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert!(witness.is_complete());
    let capture_events = CaptureEvents::default();
    let capture_scope = tracing::subscriber::set_default(capture_events.clone());
    let active_bytes = owner.authority_image_for_package_with_authority_witness_cancellable(
        &caller,
        &project,
        &witness,
        Some(&cancelled),
    )?;
    assert_eq!(
        capture_events.0.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "real cold helper preparation retains one full pre-spawn and one full post-output capture"
    );
    drop(capture_scope);
    let active_image = GoImage::open(&active_bytes)?;
    assert!(active_image.declarations().any(|row| row.is_ok_and(
        |row| row.name == b"Platform" && row.file == active.to_string_lossy().as_bytes()
    )));
    assert!(active_image.references().any(|row| row.is_ok_and(
        |row| row.target == b"Platform" && row.file == caller.to_string_lossy().as_bytes()
    )));
    let inferred = active_image
        .declarations()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|row| row.name == b"Inferred" && row.bound)
        .ok_or("missing selected inferred variable")?;
    let inferred_type =
        active_image.type_row(inferred.type_root.ok_or("missing inferred type")? as usize)?;
    assert_eq!(
        inferred_type.kind,
        backend_frontend_go::legacy::TypeRowKind::Basic
    );
    assert_eq!(inferred_type.name, b"int");
    assert!(active_image.doc_count() > 0);
    let inactive_bytes = owner.authority_image_for_package_with_authority_witness_cancellable(
        &inactive,
        &project,
        &witness,
        Some(&cancelled),
    )?;
    assert_inactive_image(&inactive_bytes, inactive_source, &inactive, &native_os)?;
    drop(owner);
    let reopened = configured()?;
    let fresh = reopened.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert_eq!(witness.identity(), fresh.identity());
    let cold_bytes = reopened.authority_image_for_package_with_authority_witness_cancellable(
        &inactive,
        &project,
        &fresh,
        Some(&cancelled),
    )?;
    assert_eq!(
        inactive_bytes, cold_bytes,
        "cold offline source selection changed"
    );
    assert_inactive_image(&cold_bytes, inactive_source, &inactive, &native_os)?;
    assert_eq!(std::fs::read(&inactive)?, inactive_source);
    Ok(())
}

fn assert_inactive_image(
    bytes: &[u8],
    source: &[u8],
    path: &std::path::Path,
    native_os: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use backend_frontend_go::legacy::parse_constraint_blob;
    use sha2::{Digest as _, Sha256};
    let image = GoImage::open(bytes)?;
    assert_eq!(
        image.source_digest(),
        <[u8; 32]>::from(Sha256::digest(source))
    );
    assert_eq!(image.declaration_count(), 0);
    assert_eq!(image.reference_count(), 0);
    assert_eq!(image.doc_count(), 0);
    assert_eq!(
        image.constraint_count(),
        1,
        "inactive source must retain positive availability evidence"
    );
    let constraint = image.constraint(0)?;
    assert_eq!(constraint.file, path.to_string_lossy().as_bytes());
    let spelling = std::str::from_utf8(constraint.constraint)?;
    assert!(spelling.contains(&format!("GOOS={native_os} ")));
    assert!(spelling.contains("CGO_ENABLED=0"));
    assert!(spelling.contains(&format!(
            "example.com/native-selection/{}",
            path.file_name()
                .ok_or("missing filename")?
                .to_string_lossy()
        )));
    let declarations = parse_constraint_blob(constraint.exported, constraint.exported_count)
        .ok_or("invalid excluded declaration plane")?;
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].name, b"Platform");
    Ok(())
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
        "package consumer\nimport \"example.com/dependency\"\nfunc Value() int { return dependency.Value() }\nvar Selected = dependency.Value()\n",
    )?;
    let module_cache = root.path().join("modules");
    std::fs::create_dir(&module_cache)?;
    let (go, goroot) = installed_go()?;
    let build_cache = root.path().join("build-cache");
    let environment =
        GoOracleChildEnvironment::new(go.clone(), goroot, module_cache, build_cache.clone())?;
    let owner: ConfiguredGoOracle = GoOracle::default()
        .with_configuration(GoOracleConfiguration::go_toolchain(go.clone())?)
        .with_child_environment(environment.clone())?;
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
    let retry_started = std::time::Instant::now();
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

    assert_selected_call_and_type(
        image,
        &std::fs::read(&source)?,
        "same-owner-after-setup",
        retry_started.elapsed(),
    )?;
    // A fresh owner must reopen the same offline selected closure and retain
    // the exact foreign call and result type, rather than only cached rows.
    let reopen_started = std::time::Instant::now();
    let reopened: ConfiguredGoOracle = GoOracle::default()
        .with_configuration(GoOracleConfiguration::go_toolchain(go.clone())?)
        .with_child_environment(environment)?;
    let reopened_witness =
        reopened.package_authority_witness_cancellable(&project, Some(&cancelled))?;
    assert!(reopened_witness.is_complete());
    assert_eq!(reopened_witness.identity(), fresh.identity());
    let reopened_image = reopened.authority_image_for_package_with_authority_witness_cancellable(
        &source,
        &project,
        &reopened_witness,
        Some(&cancelled),
    )?;
    assert_selected_call_and_type(
        GoImage::open(&reopened_image)?,
        &std::fs::read(&source)?,
        "cold-owner-offline",
        reopen_started.elapsed(),
    )?;

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

fn assert_selected_call_and_type(
    image: GoImage<'_>,
    source: &[u8],
    phase: &str,
    elapsed: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    use backend_frontend_go::legacy::{
        DeclarationKind, ReferenceTargetClass, ReferenceUseKind, TypeRowKind,
    };
    let declarations = image.declarations().collect::<Result<Vec<_>, _>>()?;
    let selected = declarations
        .iter()
        .enumerate()
        .filter(|(_, declaration)| {
            declaration.bound
                && declaration.kind == DeclarationKind::Function
                && declaration.name == b"Value"
                && declaration.package == b"example.com/consumer"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected.len(),
        1,
        "one exact bound consumer function is required"
    );
    let (owner, declaration) = selected[0];
    let signature =
        image.type_row(declaration.type_root.expect("checked Value signature") as usize)?;
    assert_eq!(signature.kind, TypeRowKind::Func);
    assert_eq!(signature.param_count, 0);
    assert_eq!(
        signature.children.1, 1,
        "Value returns one compiler-resolved result"
    );
    let (result, _) = image.type_child(signature.children.0 as usize)?;
    let result = image.type_row(result as usize)?;
    assert_eq!(result.kind, TypeRowKind::Basic);
    assert_eq!(result.name, b"int");
    let inferred = declarations
        .iter()
        .filter(|row| {
            row.bound
                && row.kind == DeclarationKind::Static
                && row.name == b"Selected"
                && row.package == b"example.com/consumer"
                && row.file == declaration.file
        })
        .collect::<Vec<_>>();
    assert_eq!(
        inferred.len(),
        1,
        "one bound inferred Selected variable is required"
    );
    let inferred = image.type_row(
        inferred[0]
            .type_root
            .expect("inferred dependency call result") as usize,
    )?;
    assert_eq!(inferred.kind, TypeRowKind::Basic);
    assert_eq!(inferred.name, b"int");
    assert!(
        source
            .windows(b"var Selected = dependency.Value()".len())
            .any(|bytes| bytes == b"var Selected = dependency.Value()")
    );
    let references = image.references().collect::<Result<Vec<_>, _>>()?;
    let selected = references
        .iter()
        .filter(|reference| {
            reference.target == b"Value"
                && reference.target_package == b"example.com/dependency"
                && reference.file == declaration.file
                && reference.use_kind == ReferenceUseKind::Call
                && reference.target_class == ReferenceTargetClass::Func
                && reference.owner_is_declaration
                && reference.owner_row as usize == owner
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected.len(),
        1,
        "one exact foreign dependency function call is required"
    );
    let call = selected[0];
    assert!(call.owner_is_declaration);
    assert_eq!(call.owner_row as usize, owner);
    assert_eq!(
        &source[call.span.0 as usize..call.span.1 as usize],
        b"Value"
    );
    let expected = source
        .windows(b"dependency.Value()".len())
        .position(|bytes| bytes == b"dependency.Value()")
        .expect("known foreign call")
        + b"dependency.".len();
    assert_eq!(call.span, (expected as u32, expected as u32 + 5));
    println!(
        "{}",
        serde_json::json!({"phase":phase,"elapsed_seconds":elapsed.as_secs_f64(),"declaration_count":declarations.len(),"reference_count":references.len(),"selected_owner":"example.com/consumer::Value","selected_target":"example.com/dependency::Value","known_call_span":call.span,"consumer_signature_result_type":"int","inferred_variable":"Selected","inferred_dependency_result_type":"int","exact_owner_foreign_call_count":1})
    );
    Ok(())
}
