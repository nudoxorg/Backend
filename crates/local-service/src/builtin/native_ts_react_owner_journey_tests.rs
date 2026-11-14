//! Owner-wire regression for the separately provisioned React fixture.
//! Draft for the frozen NSE native-only route. Requires unit-test-only frontend
//! construction failpoint and selected native-program/inventory assertions.
#![cfg(any(unix, windows))]

use backend_client::Session;
use backend_library::{
    CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
    IndexOperationState, PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn verify_react_fixture(project: &Path) -> Result<(), Box<dyn Error>> {
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
    Ok(())
}

struct CaptureGuard;
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        super::ingest::TEST_CAPTURE_NATIVE_PROGRAMS.store(false, Ordering::Release);
    }
}

fn native_react_children_journey(
    reject_frontends: bool,
    require_references: bool,
    require_qualified_references: bool,
) -> Result<(), Box<dyn Error>> {
    let _guard = super::native_ts_owner_journey_tests::FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(reject_frontends, Ordering::Release);
    let _capture_guard = CaptureGuard;
    super::ingest::TEST_NATIVE_PROGRAMS
        .lock()
        .map_err(|_| "native Program observation lock")?
        .clear();
    super::ingest::TEST_CAPTURE_NATIVE_PROGRAMS.store(true, Ordering::Release);
    let project =
        PathBuf::from(std::env::var_os("TSZ_REACT_REPRO_ROOT").ok_or("TSZ_REACT_REPRO_ROOT")?)
            .canonicalize()?;
    verify_react_fixture(&project)?;
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
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)?;
    let mut selected_memberships = Vec::<([u8; 32], [u8; 32])>::new();
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
            status = super::native_ts_owner_journey_tests::poll_index_status(&mut session, key)?;
        }
        session.reconnect()?;
        for (name, expected_source) in [
            ("AuthCard", "children: ReactNode"),
            ("AppShell", "children: (user: User) => ReactNode"),
        ] {
            let expected_path = format!("components/{name}.tsx");
            let (coordinate, document) =
                super::native_ts_owner_journey_tests::select_native_document(
                    &mut session,
                    &authority,
                    name,
                    &expected_path,
                )?;
            let policy = serde_json::to_value(&document.source_policy)?;
            assert_eq!(policy["state"], "captured_observed");
            assert_eq!(policy["data"]["statement"]["policy_stability"], "unknown");
            selected_memberships.push((
                serde_json::from_value(policy["data"]["statement"]["program_membership"].clone())?,
                serde_json::from_value(policy["data"]["statement"]["publication_binding"].clone())?,
            ));
            assert!(
                document
                    .excerpt
                    .text()
                    .is_some_and(|text| text.contains(expected_source)),
                "source for {name}"
            );
            if require_references {
                let target = ProductText::new(coordinate.clone())
                    .map_err(|e| io::Error::other(e.to_string()))?;
                let reply = session.surface(SurfaceCommand::References { target })?;
                let SurfaceReply::References { references, coverage, .. } = reply else {
                    return Err("references reply".into());
                };
                let consumer =
                    fs::read_to_string(project.join("components/ReactCycleEvidence.tsx"))?;
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
                if require_qualified_references {
                    assert!(matches!(coverage, backend_library::ReferenceCoverage::Partial { .. }),
                        "native observed references must retain Partial coverage: {coverage:?}");
                    assert!(references.iter().all(|item| !item.source_policy.is_unknown()),
                        "every returned native reference must carry an observed source claim");
                    for item in references.iter().filter(|item| item.evidence.source.as_ref().is_some_and(|span|
                        span.file.as_str().ends_with("components/ReactCycleEvidence.tsx"))) {
                        let span = item.evidence.source.as_ref().expect("filtered exact source");
                        assert_eq!(span.file.as_str(), "components/ReactCycleEvidence.tsx");
                        assert_eq!(consumer.get(span.start as usize..span.end as usize), Some(name));
                        let CommandReply::Document(caller) = session.source(item.site.as_str())?.reply else {
                            return Err("reference caller source reply".into());
                        };
                        let selection = caller.selection.as_ref().ok_or("reference caller selection")?;
                        assert_eq!(selection.coordinate(), item.site.as_str());
                        assert_eq!(item.site_symbol, Some(*selection.symbol().as_bytes()));
                        item.source_policy.verify_owner(&authority, selection)
                            .map_err(|error| io::Error::other(format!("reference owner seal: {error:?}")))?;
                        let policy = serde_json::to_value(&item.source_policy)?;
                        assert_eq!(policy["data"]["statement"]["source_path"], "components/ReactCycleEvidence.tsx");
                        assert_eq!(policy["data"]["statement"]["selection_coordinate"], item.site.as_str());
                        assert_eq!(policy["data"]["statement"]["symbol"], serde_json::to_value(item.site_symbol.expect("site symbol"))?);
                        assert_eq!(policy["state"], "captured_observed");
                        assert_eq!(policy["data"]["statement"]["policy_stability"], "unknown");
                    }
                }
                eprintln!(
                    "owner-wire {name}: coordinate={coordinate} exact-reference-count={}",
                    references.len()
                );
            }
        }
        Ok(())
    })();
    let closed = owner.close()?;
    assert_eq!(closed.failures, 0);
    verify_react_fixture(&project)?;
    result?;
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)?;
    let mut cold = Session::connect(owner.endpoint())?;
    for name in ["AuthCard", "AppShell"] {
        let expected_path = format!("components/{name}.tsx");
        let (_, document) = super::native_ts_owner_journey_tests::select_native_document(
            &mut cold,
            &authority,
            name,
            &expected_path,
        )?;
        let policy = serde_json::to_value(&document.source_policy)?;
        assert_eq!(policy["state"], "captured_observed");
        assert_eq!(policy["data"]["statement"]["policy_stability"], "unknown");
        selected_memberships.push((
            serde_json::from_value(policy["data"]["statement"]["program_membership"].clone())?,
            serde_json::from_value(policy["data"]["statement"]["publication_binding"].clone())?,
        ));
    }
    drop(cold);
    assert_eq!(owner.close()?.failures, 0);
    super::native_ts_owner_journey_tests::selected_evidence_root(
        paths.data(),
        &project,
        &[
            (
                "components/AuthCard.tsx",
                backend_semantic::vocabulary::TypeScriptSource::Tsx,
            ),
            (
                "components/AppShell.tsx",
                backend_semantic::vocabulary::TypeScriptSource::Tsx,
            ),
        ],
    )?;
    // The observation was taken only after the native Program/image join.
    // Match it to each authenticated selected claim's membership identity,
    // then check exact external source bytes. A staged observation alone is
    // not selection authority.
    let observations = super::ingest::TEST_NATIVE_PROGRAMS
        .lock()
        .map_err(|_| "native Program observation lock")?;
    assert_eq!(selected_memberships.len(), 4);
    for (membership, binding) in selected_memberships {
        let observed = observations
            .iter()
            .find(|entry| entry.membership == membership && entry.binding == binding)
            .ok_or("selected native Program was not observed at its admitted image join")?;
        for path in [
            "node_modules/@types/react/index.d.ts",
            "node_modules/@types/react/global.d.ts",
            "node_modules/@types/react/jsx-runtime.d.ts",
            "node_modules/csstype/index.d.ts",
        ] {
            let bytes = fs::read(project.join(path))?;
            let source_id = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(&bytes);
            let matches = observed
                .sources
                .iter()
                .filter(|(actual, _, identity, size)| {
                    actual == path
                        && identity == source_id.as_ref()
                        && *size as usize == bytes.len()
                })
                .count();
            assert_eq!(matches, 1, "exact selected native external source: {path}");
        }
    }
    Ok(())
}

#[test]
#[ignore = "isolated lock-verified React fixture with test-only frontend failpoint"]
fn native_react_owner_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(true, false, false)
}

#[test]
#[ignore = "isolated lock-verified React fixture, uninstrumented control"]
fn native_react_owner_uninstrumented_control() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(false, false, false)
}

#[test]
#[ignore = "isolated lock-verified React reference journey with frontend failpoint"]
fn native_react_owner_references_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(true, true, false)
}

#[test]
#[ignore = "isolated lock-verified React reference journey uninstrumented control"]
fn native_react_owner_references_uninstrumented_control() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(false, true, false)
}

#[test]
#[ignore = "isolated React reference claim and owner MAC journey with frontend failpoint"]
fn native_react_owner_qualified_references_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(true, true, true)
}

#[test]
#[ignore = "isolated React reference claim and owner MAC uninstrumented control"]
fn native_react_owner_qualified_references_uninstrumented_control() -> Result<(), Box<dyn Error>> {
    native_react_children_journey(false, true, true)
}
