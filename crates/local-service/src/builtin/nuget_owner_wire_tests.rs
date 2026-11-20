//! Loopback NuGet metadata through the selected local owner and client wire.
//! Run in a network namespace containing loopback only; no archive is served.
use crate::{EmbeddedLocalService, ProcessConfig};
use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use backend_client::Session;
use backend_engine::registry::{
    RegistryEcosystem, RegistryEndpoint, RegistrySource, RegistrySourceSet,
};
use backend_library::{
    PackageReference, ProductText, RegistryDiscoveryCandidate, RegistryEvidenceFacet,
    RegistryPackageDiscoveryObservation, RegistrySearchHit, RegistrySearchRelease, SurfaceCommand,
    SurfaceReply,
};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const NAME: &str = "Microsoft.EntityFrameworkCore";
const PURL: &str = "pkg:nuget/Microsoft.EntityFrameworkCore@10.0.13";
const DESCRIPTION: &str = "Entity Framework metadata from private fixture";

type Documents = Arc<BTreeMap<String, (StatusCode, Vec<u8>)>>;
#[derive(Clone)]
struct FeedState {
    documents: Documents,
    requests: Arc<Mutex<Vec<String>>>,
}
async fn route(State(state): State<FeedState>, uri: Uri) -> Response {
    let path = uri.path().to_owned();
    state.requests.lock().unwrap().push(path.clone());
    let (status, bytes) = state
        .documents
        .get(&path)
        .cloned()
        .unwrap_or((StatusCode::NOT_FOUND, Vec::new()));
    (
        status,
        [
            ("content-type", "application/json"),
            ("connection", "close"),
        ],
        bytes,
    )
        .into_response()
}
struct Feed {
    base: String,
    docs: Documents,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Feed {
    fn start(
        page_status: StatusCode,
        wrong_leaf: Option<(&str, &str)>,
    ) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let base = format!("http://{}", listener.local_addr()?);
        let page = format!("/v3/page-{}", listener.local_addr()?.port());
        let leaf = format!("/v3/catalog/leaf-{}", listener.local_addr()?.port());
        let mut docs = BTreeMap::new();
        let mut put = |path: &str, value: serde_json::Value| {
            docs.insert(
                path.to_owned(),
                (StatusCode::OK, serde_json::to_vec(&value).unwrap()),
            );
        };
        put(
            "/v3/catalog0/index.json",
            serde_json::json!({"commitId":"fixture-tip","commitTimeStamp":"2026-11-01T00:00:00Z","count":0,"items":[]}),
        );
        put(
            "/v3/index.json",
            serde_json::json!({"version":"3.0.0","resources":[{"@id":format!("{base}/v3/registrations/"),"@type":["RegistrationsBaseUrl/3.0.0-beta","RegistrationsBaseUrl/3.6.0"]}]}),
        );
        put(
            "/v3/registrations/microsoft.entityframeworkcore/index.json",
            serde_json::json!({"count":2,"items":[{"count":1,"items":[{"catalogEntry":{"id":NAME,"version":"1.0.0","listed":true}}]},{"@id":format!("{base}{page}"),"count":1,"lower":"10.0.0","upper":"10.0.13"}]}),
        );
        put(
            &page,
            serde_json::json!({"count":1,"items":[{"catalogEntry":format!("{base}{leaf}")}]}),
        );
        put(
            &leaf,
            serde_json::json!({"id":wrong_leaf.map_or(NAME, |v| v.0),"version":wrong_leaf.map_or("10.0.13", |v| v.1),"listed":true,"description":DESCRIPTION,"licenseExpression":"MIT","published":"2026-11-01T00:00:00Z"}),
        );
        if page_status != StatusCode::OK {
            docs.insert(page.clone(), (page_status, Vec::new()));
        }
        let docs = Arc::new(docs);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = FeedState {
            documents: docs.clone(),
            requests: requests.clone(),
        };
        let (stop, rx) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(
                    listener,
                    Router::new().fallback(any(route)).with_state(state),
                )
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
            });
            runtime.shutdown_timeout(Duration::from_secs(2));
        });
        Ok(Self {
            base,
            docs,
            requests,
            stop: Some(stop),
            thread: Some(thread),
        })
    }
    fn endpoint(&self) -> String {
        format!("{}/v3/catalog0/index.json", self.base)
    }
    fn proof(&self) -> [u8; 32] {
        let page = format!("/v3/page-{}", self.base.rsplit(':').next().unwrap());
        let leaf = format!("/v3/catalog/leaf-{}", self.base.rsplit(':').next().unwrap());
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.nuget-exact-service-registration.v1\0");
        for path in [
            "/v3/index.json",
            "/v3/registrations/microsoft.entityframeworkcore/index.json",
            &page,
            &leaf,
        ] {
            let url = format!("{}{path}", self.base);
            let bytes = &self.docs[path].1;
            hash.update(&(url.len() as u64).to_be_bytes());
            hash.update(url.as_bytes());
            hash.update(&(bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        *hash.finalize().as_bytes()
    }
}
impl Drop for Feed {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn start_owner(
    paths: &backend_runtime::WorkspacePaths,
    feed: &Feed,
    offline: bool,
) -> Result<EmbeddedLocalService, Box<dyn Error>> {
    let mut args = vec![
        "--endpoint".to_owned(),
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        paths.data().to_string_lossy().into_owned(),
        "--registry-source".to_owned(),
        format!("nuget={}", feed.endpoint()),
        "--registry-discovery-source".to_owned(),
        format!("nuget={}", feed.endpoint()),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ];
    if offline {
        args.push("--registry-discovery-offline".to_owned());
    }
    let mut config = ProcessConfig::parse(args)?;
    // Parsed process configuration includes official default registry sources
    // and can include environment overrides. Close both source sets to this
    // private feed before the owner starts, with no registry credentials.
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Nuget, feed.endpoint())?;
    config.registry.sources =
        RegistrySourceSet::empty().with_source(RegistrySource::new(endpoint.clone()))?;
    config.registry.endpoint = None;
    config.registry.authentication = None;
    config.discovery.sources = vec![endpoint];
    config.profile = "builtin".to_owned();
    config.worker_endpoint = None;
    config.authority_secret = Some(paths.authority_secret().to_path_buf());
    Ok(EmbeddedLocalService::start(config)?)
}
fn package(session: &mut Session) -> Result<SurfaceReply, Box<dyn Error>> {
    Ok(session.surface(SurfaceCommand::Package {
        package: PackageReference::parse(PURL).map_err(|e| std::io::Error::other(e.to_string()))?,
    })?)
}
fn candidate(reply: &SurfaceReply) -> Option<&RegistryDiscoveryCandidate> {
    match reply {
        SurfaceReply::PackageDiscovery {
            observation: RegistryPackageDiscoveryObservation::Observed { candidates },
            ..
        } => candidates.iter().find(|c| c.coordinate.as_str() == PURL),
        _ => None,
    }
}
fn explore(session: &mut Session) -> Result<Option<RegistryDiscoveryCandidate>, Box<dyn Error>> {
    let reply = session.surface(SurfaceCommand::Explore {
        query: Some(ProductText::new(NAME).map_err(|e| std::io::Error::other(e.to_string()))?),
        limit: 10,
    })?;
    let SurfaceReply::ExploredSearchPage(page) = reply else {
        return Err(format!("unexpected explore reply: {reply:?}").into());
    };
    for hit in page.hits.iter() {
        match hit {
            RegistrySearchHit::Discovered(c) if c.coordinate.as_str() == PURL => {
                return Ok(Some(c.clone()));
            }
            RegistrySearchHit::PackageGroup(group) => {
                for release in group.releases.iter() {
                    if let RegistrySearchRelease::Discovered(c) = release {
                        if c.coordinate.as_str() == PURL {
                            return Ok(Some(c.clone()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(None)
}
fn paths() -> Result<(tempfile::TempDir, backend_runtime::WorkspacePaths), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(root.path().join("owner")),
        Some(root.path().join("state")),
        Some(root.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    Ok((root, paths))
}
fn poll(session: &mut Session) -> Result<RegistryDiscoveryCandidate, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let reply = package(session)?;
        if let Some(c) = candidate(&reply) {
            return Ok(c.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!("NuGet exact metadata deadline: {reply:?}").into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn nuget_owner_wire_exact_explore_and_cold_reopen() -> Result<(), Box<dyn Error>> {
    let feed = Feed::start(StatusCode::OK, None)?;
    let (_root, paths) = paths()?;
    let owner = start_owner(&paths, &feed, false)?;
    let mut session = Session::connect(owner.endpoint())?;
    let exact = poll(&mut session)?;
    let source = RegistryEndpoint::new(RegistryEcosystem::Nuget, feed.endpoint())?
        .id()
        .as_bytes();
    assert_eq!(exact.source, source);
    assert_eq!(exact.proof, feed.proof());
    assert!(
        matches!(&exact.metadata.description, RegistryEvidenceFacet::Known(value) if value.as_str() == DESCRIPTION)
    );
    assert!(
        matches!(&exact.metadata.license, RegistryEvidenceFacet::Known(value) if value.as_str() == "MIT")
    );
    assert!(
        matches!(&exact.metadata.published_at, RegistryEvidenceFacet::Known(value) if value.as_str() == "2026-11-01T00:00:00Z")
    );
    assert!(matches!(
        &exact.metadata.yanked,
        RegistryEvidenceFacet::Known(false)
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let search = loop {
        if let Some(hit) = explore(&mut session)? {
            break hit;
        }
        if Instant::now() >= deadline {
            return Err("explore discovery deadline".into());
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(search.coordinate, exact.coordinate);
    assert_eq!(search.source, exact.source);
    assert_eq!(search.proof, exact.proof);
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    let before = feed.requests.lock().unwrap().len();
    let owner = start_owner(&paths, &feed, true)?;
    let mut session = Session::connect(owner.endpoint())?;
    let reopened = poll(&mut session)?;
    assert_eq!(reopened.coordinate, exact.coordinate);
    assert_eq!(reopened.source, exact.source);
    assert_eq!(reopened.proof, exact.proof);
    let search = explore(&mut session)?.ok_or("offline explore missing exact discovery")?;
    assert_eq!(search.proof, exact.proof);
    assert_eq!(search.source, exact.source);
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    assert_eq!(
        feed.requests.lock().unwrap().len(),
        before,
        "offline reopen contacted feed"
    );
    let requests = feed.requests.lock().unwrap();
    for path in [
        "/v3/index.json",
        "/v3/registrations/microsoft.entityframeworkcore/index.json",
    ] {
        assert!(
            requests.iter().any(|p| p == path),
            "missing {path}: {requests:?}"
        );
    }
    assert!(requests.iter().any(|p| p.starts_with("/v3/page-")));
    assert!(requests.iter().any(|p| p.starts_with("/v3/catalog/leaf-")));
    assert!(
        requests.iter().all(|p| feed.docs.contains_key(p)),
        "unexpected request (including archive): {requests:?}"
    );
    // Deliberate acceptance gate: currently the discovery DTO omits this source fact.
    assert!(
        serde_json::to_string(&exact)?.contains(DESCRIPTION),
        "source description not delivered by typed candidate"
    );
    Ok(())
}
#[test]
fn nuget_owner_wire_partial_errors_and_complete_absence_are_distinct() -> Result<(), Box<dyn Error>>
{
    for (status, wrong_leaf) in [
        (StatusCode::NOT_FOUND, None),
        (StatusCode::INTERNAL_SERVER_ERROR, None),
        (StatusCode::OK, Some(("Other.Package", "10.0.13"))),
        (StatusCode::OK, Some((NAME, "10.0.12"))),
    ] {
        let feed = Feed::start(status, wrong_leaf)?;
        let (_root, paths) = paths()?;
        let owner = start_owner(&paths, &feed, false)?;
        let mut session = Session::connect(owner.endpoint())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let reply = package(&mut session)?;
            let observed = match &reply {
                SurfaceReply::PackageDiscovery {
                    observation:
                        RegistryPackageDiscoveryObservation::Unavailable {
                            source: Some(_),
                            reason,
                        },
                    ..
                } => reason.as_str().contains("metadata request failed"),
                SurfaceReply::PackageDiscovery {
                    observation: RegistryPackageDiscoveryObservation::Missing { .. },
                    ..
                } => wrong_leaf == Some((NAME, "10.0.12")),
                SurfaceReply::PackageDiscovery {
                    observation: RegistryPackageDiscoveryObservation::Observed { .. },
                    ..
                } => return Err(format!("mismatched release was promoted: {reply:?}").into()),
                _ => false,
            };
            if observed {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("page request deadline: {reply:?}").into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        drop(session);
        assert_eq!(owner.close()?.failures, 0);
    }
    Ok(())
}
