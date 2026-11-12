//! Owner-wire regression for the separately provisioned React fixture.
//! The release owner's index route may also invoke its legacy structural
//! frontends; this test does not claim a native-only indexing pipeline.
#![cfg(any(unix, windows))]

use backend_client::Session;
use backend_library::{
    CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
    IndexOperationState, PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};
use backend_local_service::{EmbeddedLocalService, ProcessConfig};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs, io,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires isolated lock-verified React project fixture"]
fn native_react_children_index_names_references_and_source() -> Result<(), Box<dyn Error>> {
    let project =
        PathBuf::from(std::env::var_os("TSZ_REACT_REPRO_ROOT").ok_or("TSZ_REACT_REPRO_ROOT")?)
            .canonicalize()?;
    for (path, digest) in [
        (
            "node_modules/@types/react/index.d.ts",
            "045c1b395851aa71ef76393398466330fe1eeebc113c8085eefc6d84abea3a88",
        ),
        (
            "node_modules/@types/react/global.d.ts",
            "7e29f41b158de217f94cb9676bf9cbd0cd9b5a46e1985141ed36e075c52bf6ad",
        ),
        (
            "node_modules/csstype/index.d.ts",
            "ac51dd7d31333793807a6abaa5ae168512b6131bd41d9c5b98477fc3b7800f9f",
        ),
        (
            "components/AuthCard.tsx",
            "a0203de6af1d7cddbab62fc17437689951593d37580bf0a64e8614dd5f864753",
        ),
        (
            "components/AppShell.tsx",
            "eebea631a0c49a170ad214f179f249fe72998d87f153c6eeac7a822a9bc335e1",
        ),
        (
            "components/Button.tsx",
            "cf2a3606660c6274962efb99244ef13271fa4364846d73b0d33084fd36b539a5",
        ),
        (
            "lib/api.ts",
            "1ffb26f9f15dd9a3b3f88f39f1d81b2b46792c98ff3b0e7702ca070b93dc712b",
        ),
        (
            "components/ReactCycleEvidence.tsx",
            "b47ca15e57305282812c36224861c9d85fb8b083f4cba5acb9601ebe24125086",
        ),
        (
            "node_modules/@types/react/jsx-runtime.d.ts",
            "b838d4c72740eb0afd284bf7575b74c624b105eff2e8c7b4aeead57e7ac320ff",
        ),
        (
            "node_modules/next/link.d.ts",
            "edca4e02f095466637fa5adcae39d38e70e325be10a5ba013cef677c773c7e36",
        ),
        (
            "node_modules/next/navigation.d.ts",
            "611e9b1df79fca98148afc0bd07e520e3f28ca049650086b785a1757120dec92",
        ),
        (
            "tsconfig.json",
            "be18523b23b78b6e1a876ddd107d330f0168bddf09d751b57e0b5edce69c66f3",
        ),
    ] {
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(project.join(path))?)),
            digest,
            "fixture identity: {path}"
        );
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-owner-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().to_path_buf()),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let mut config = ProcessConfig::parse([
        "--endpoint".to_owned(),
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        paths.data().to_string_lossy().into_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.profile = "builtin".to_owned();
    config.worker_endpoint = None;
    config.authority_secret = Some(paths.authority_secret().to_path_buf());
    let owner = EmbeddedLocalService::start(config)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let mut session = Session::connect(owner.endpoint())?;
        let package = PackageReference::parse(project.to_string_lossy().into_owned())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let key = IndexOperationKey::from_bytes([0x49; 32])
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut status =
            session.start_index_operation(key, package, CompileExecutionIntent::Interactive)?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let IndexOperationObservation::Known(known) = status else {
                return Err("unknown operation".into());
            };
            match known.state {
                IndexOperationState::Published(_) => break,
                IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
                state => return Err(io::Error::other(format!("index refused: {state:?}")).into()),
            }
            if Instant::now() >= deadline {
                return Err("index deadline".into());
            }
            thread::sleep(Duration::from_millis(30));
            status = session.index_operation_status(key)?;
        }
        session.reconnect()?;
        for (name, expected_source) in [
            ("AuthCard", "children: ReactNode"),
            ("AppShell", "children: (user: User) => ReactNode"),
        ] {
            let CommandReply::Names(names) = session.names(name, 100)?.reply else {
                return Err("names reply".into());
            };
            let suffix = format!("::{name}");
            let source_path = format!("::components/{name}.tsx:");
            let labels = names
                .root
                .rows()
                .iter()
                .filter(|row| row.label.contains(&source_path) && row.label.ends_with(&suffix))
                .map(|row| row.label.clone())
                .collect::<Vec<_>>();
            let [coordinate] = labels.as_slice() else {
                return Err(io::Error::other(format!("{name} labels: {labels:?}")).into());
            };
            let CommandReply::Document(document) = session.source(coordinate)?.reply else {
                return Err("source reply".into());
            };
            assert!(
                document
                    .excerpt
                    .text()
                    .is_some_and(|text| text.contains(expected_source)),
                "source for {name}"
            );
            let target = ProductText::new(coordinate.clone())
                .map_err(|e| io::Error::other(e.to_string()))?;
            let reply = session.surface(SurfaceCommand::References { target })?;
            let SurfaceReply::References { references, .. } = reply else {
                return Err("references reply".into());
            };
            let consumer = fs::read_to_string(project.join("components/ReactCycleEvidence.tsx"))?;
            assert!(
                references
                    .iter()
                    .any(|item| item.evidence.source.as_ref().is_some_and(|span| span
                        .file
                        .as_str()
                        .ends_with("components/ReactCycleEvidence.tsx")
                        && consumer.get(span.start as usize..span.end as usize) == Some(name))),
                "{name} missing exact fixture consumer references: {references:?}"
            );
            eprintln!(
                "owner-wire {name}: coordinate={coordinate} exact-reference-count={}",
                references.len()
            );
        }
        Ok(())
    })();
    let closed = owner.close()?;
    assert_eq!(closed.failures, 0);
    result
}
