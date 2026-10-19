//! Cold saved-store discovery uses the real startup/configuration and owner
//! recovery paths. A bootstrap cwd is not an instruction to index that folder.
#![cfg(unix)]

use backend_client::Session;
use backend_library::{
    CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
    IndexOperationState, PackageReference, RowId,
};
use backend_local_service::{EmbeddedLocalService, ProcessConfig};
use backend_runtime::{
    AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV, SpawnedOwnerBootstrap,
    WorkspacePaths,
};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_ROOT: &str = "NUDOX_SAVED_WORKSPACE_TEST_ROOT";
const CHILD_OUTPUT_CAP: u64 = 4 * 1024 * 1024;

#[test]
fn cold_saved_owner_recovers_original_publication_without_indexing_its_bootstrap_cwd()
-> Result<(), Box<dyn Error>> {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return cold_owner(&PathBuf::from(root));
    }
    // Isolate HOME/state/CWD in another test process, never mutate process-wide
    // configuration underneath other native tests or an existing owner.
    let fixture = tempfile::Builder::new().prefix("sv-").tempdir()?;
    let root = fixture.path().canonicalize()?;
    for name in ["home", "state", "data"] {
        backend_platform::durable::ensure_private_directory(&root.join(name))?;
    }
    let directory = backend_platform::DirectoryCapability::open(&root)?;
    let stdout_path = root.join("child.stdout.raw");
    let stderr_path = root.join("child.stderr.raw");
    let stdout = directory.create_file_exclusive("child.stdout.raw")?;
    let stderr = directory.create_file_exclusive("child.stderr.raw")?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "cold_saved_owner_recovers_original_publication_without_indexing_its_bootstrap_cwd",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_DATA_HOME", root.join("data"))
        // Keep data/raw under the caller's managed TMPDIR; the normal short
        // per-user endpoint path ignores TMPDIR on Unix.
        .env_remove("XDG_RUNTIME_DIR")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .current_dir(&root);
    for name in [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV] {
        command.env_remove(name);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        for path in [&stdout_path, &stderr_path] {
            if fs::metadata(path)?.len() > CHILD_OUTPUT_CAP {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("isolated child output exceeded its bound").into());
            }
        }
        if let Some(status) = child.try_wait()? {
            for (kind, path) in [("stdout", &stdout_path), ("stderr", &stderr_path)] {
                let size = fs::metadata(path)?.len();
                assert!(
                    size <= CHILD_OUTPUT_CAP,
                    "bounded isolated child {kind}: {size} bytes"
                );
                if !status.success() {
                    // Escape line breaks: a nested summary is not another
                    // selected outer libtest result.
                    eprintln!(
                        "isolated child {kind}: {:?}",
                        String::from_utf8_lossy(&fs::read(path)?)
                    );
                }
            }
            assert!(status.success(), "isolated real cold owner test: {status}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill(); // Only this exact test-owned child.
            let _ = child.wait();
            return Err(io::Error::other("isolated cold owner regression exceeded 120s").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn owner_args(paths: &WorkspacePaths) -> Vec<String> {
    vec![
        "--endpoint".into(),
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".into(),
        paths.data().to_string_lossy().into_owned(),
        "--authority-secret-file".into(),
        paths.authority_secret().to_string_lossy().into_owned(),
        "--profile".into(),
        "builtin".into(),
        "--registry-offline".into(),
        "--registry-discovery-offline".into(),
        "--advisory-offline".into(),
        "--forge-offline".into(),
    ]
}

fn native_marker(session: &mut Session) -> Result<Vec<(RowId, String)>, Box<dyn Error>> {
    let CommandReply::Names(names) = session.names("saved_native_marker", 20)?.reply else {
        return Err(io::Error::other("native names reply").into());
    };
    let rows = names
        .root
        .row_refs()
        .filter(|row| {
            matches!(row.id, RowId::Symbol(_)) && row.label.ends_with("::saved_native_marker")
        })
        .map(|row| (row.id, row.label.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "one actual selected native declaration");
    Ok(rows)
}

fn cold_owner(root: &Path) -> Result<(), Box<dyn Error>> {
    let owner_project = root.join("original-owner");
    let project = owner_project.join("crates/selected-package");
    backend_platform::durable::ensure_private_directory(&project)?;
    backend_platform::durable::ensure_private_directory(&owner_project.join(".git"))?;
    assert_eq!(
        backend_runtime::discover_project_from_directory(&project)?,
        owner_project,
        "default CLI/MCP owner is the Git parent of the selected indexed package"
    );
    fs::write(
        project.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true},"files":["entry.ts"]}"#,
    )?;
    let source = "export function saved_native_marker(): number { return 41; }\n";
    fs::write(project.join("entry.ts"), source)?;
    let paths = WorkspacePaths::discover(Some(owner_project.clone()), None, None)?;
    paths.initialize()?;
    let credential = fs::read(paths.authority_secret())?;
    let first = EmbeddedLocalService::start(ProcessConfig::parse(owner_args(&paths))?)?;
    let published = (|| -> Result<_, Box<dyn Error>> {
        let mut session = Session::connect(first.endpoint())?;
        let package = PackageReference::parse(project.to_string_lossy().into_owned())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let key = IndexOperationKey::from_bytes([0x72; 32])
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut status =
            session.start_index_operation(key, package, CompileExecutionIntent::Interactive)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let IndexOperationObservation::Known(known) = status else {
                return Err(io::Error::other("native operation unknown").into());
            };
            match known.state {
                IndexOperationState::Published(_) => break,
                IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
                state => {
                    return Err(io::Error::other(format!(
                        "native TSZ publication refused: {state:?}"
                    ))
                    .into());
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("tiny native TSZ publication deadline").into());
            }
            thread::sleep(Duration::from_millis(20));
            status = session.index_operation_status(key)?;
        }
        Ok((session.revision()?.root, native_marker(&mut session)?))
    })();
    assert_eq!(first.close()?.failures, 0);
    let (published_root, native_rows) = published?;
    assert!(
        !paths.data().join("client-index-operations-v1").exists(),
        "no caller claim catalog"
    );

    // A real source file inside the bootstrap cwd makes accidental eager
    // indexing observable. It must not become a package or change the root.
    fs::write(
        paths.data().join("trap.ts"),
        "export function never_index_this_data_folder() {}\n",
    )?;
    fs::write(
        paths.data().join("tsconfig.json"),
        r#"{"files":["trap.ts"]}"#,
    )?;
    std::env::set_current_dir(paths.data())?; // This isolated child runs one exact test.
    let mut args = vec![
        "--runtime-startup-v1".into(),
        "explicit".into(),
        String::new(),
        paths.data().to_string_lossy().into_owned(),
        paths.data().to_string_lossy().into_owned(),
        paths.endpoint().to_string_lossy().into_owned(),
        paths.authority_secret().to_string_lossy().into_owned(),
    ];
    args.extend(owner_args(&paths));
    let (bootstrap, args) = SpawnedOwnerBootstrap::extract(args)?;
    let config = ProcessConfig::parse(args)?;
    assert_eq!(config.workspace, paths.data());
    bootstrap
        .ok_or_else(|| io::Error::other("missing versioned automatic-start prefix"))?
        .initialize(
            &config.workspace,
            config.endpoint.as_path(),
            config.authority_secret.as_deref(),
        )?;
    let cold = EmbeddedLocalService::start(config)?;
    let checked = (|| -> Result<(), Box<dyn Error>> {
        let mut session = Session::connect(cold.endpoint())?;
        assert_eq!(
            session.revision()?.root,
            published_root,
            "no mutation at cold startup"
        );
        assert_eq!(
            native_marker(&mut session)?,
            native_rows,
            "exact retained native identities"
        );
        let CommandReply::Packages(shelf) = session.packages()?.reply else {
            return Err(io::Error::other("authenticated cold package shelf").into());
        };
        let packages = shelf
            .root
            .rows()
            .iter()
            .filter(|row| matches!(row.id, RowId::Package(_)))
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        let project_label = project
            .to_str()
            .ok_or_else(|| io::Error::other("selected fixture path is not UTF-8"))?;
        let bootstrap_label = paths
            .data()
            .to_str()
            .ok_or_else(|| io::Error::other("bootstrap fixture path is not UTF-8"))?;
        assert!(packages.contains(&project_label));
        assert!(!packages.contains(&bootstrap_label));
        let saved = backend_client::find_existing_indexed_project(&project)?;
        let selected = saved.open()?;
        saved.revalidate(&selected)?;
        assert_eq!(selected.project(), owner_project);
        assert_ne!(
            selected.project(),
            project,
            "owner and indexed package stay distinct"
        );
        assert_eq!(selected.data(), paths.data());
        assert_eq!(selected.endpoint(), paths.endpoint());
        assert_eq!(selected.authority_secret(), paths.authority_secret());
        assert_eq!(
            session.revision()?.root,
            published_root,
            "read-only discovery/open"
        );
        assert_eq!(fs::read(paths.authority_secret())?, credential);
        assert_eq!(fs::read_to_string(project.join("entry.ts"))?, source);
        Ok(())
    })();
    assert_eq!(cold.close()?.failures, 0);
    checked
}
