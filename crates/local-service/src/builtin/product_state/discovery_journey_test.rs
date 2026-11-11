// A loopback HTTP registry exercises source metadata and product reads without
// acquiring an archive or contacting a public registry.
#[cfg(unix)]
#[test]
fn fake_npm_and_pypi_discovery_to_pinned_product_reads() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt as _;
    use std::thread;
    use std::time::Duration;

    let mut gaps = Vec::new();
    for (ecosystem, purl, body, request_path) in [
        (
            RegistryEcosystem::Npm,
            "pkg:npm/fixture@1.0.0",
            r#"{"name":"fixture","description":"npm description","readme":"npm readme witness","versions":{"1.0.0":{"license":"MIT","engines":{"node":">=18"},"dependencies":{"dep":"^1"}}}}"#,
            "/fixture",
        ),
        (
            RegistryEcosystem::Pypi,
            "pkg:pypi/fixture@1.0.0",
            r#"{"info":{"name":"fixture","version":"1.0.0","summary":"pypi description","description":"pypi readme witness","requires_python":">=3.10","requires_dist":["dep>=1"]},"vulnerabilities":[],"releases":{"1.0.0":[{"yanked":false}]}}"#,
            "/pypi/fixture/json",
        ),
    ] {
        let root = fixture("fake-http-package-discovery");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("private fixture root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback registry");
        let address = listener.local_addr().expect("registry address");
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("registry request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).expect("HTTP headers");
                request.push(byte[0]);
                assert!(request.len() < 16 * 1024);
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("response");
            String::from_utf8(request).expect("request text")
        });
        let endpoint = RegistryEndpoint::new(ecosystem, format!("http://{address}"))
            .expect("local registry endpoint");
        let mut discovery = crate::discovery::exact_metadata_fixture_gateway(
            endpoint,
            root.join("discovery.journal"),
        );
        let coordinate =
            backend_engine::registry::PackageCoordinate::parse(purl).expect("pinned purl");
        assert!(
            discovery.observe_package(&coordinate).is_none(),
            "source observation admitted"
        );
        let fact = discovery
            .store()
            .facts()
            .find_map(|(_, fact)| (fact.coordinate == coordinate).then_some(fact))
            .expect("stored source fact");
        if ecosystem == RegistryEcosystem::Pypi {
            assert_eq!(
                fact.metadata.advisory_release_scope.as_deref(),
                Some("1.0.0")
            );
        } else {
            assert_eq!(fact.metadata.advisory_release_scope, None);
        }
        let request = worker.join().expect("registry worker");
        assert!(
            request.starts_with(&format!("GET {request_path} ")),
            "{request}"
        );

        let mut state = ProductState::open(root.join("product.json")).expect("product state");
        let catalog = [];
        let catalog_index = CatalogLookupIndex::from_catalog(&catalog);
        let graph = indexed_graph(Vec::new());
        let package = PackageReference::Purl(coordinate.clone());
        let command = |state: &mut ProductState, command| {
            state.execute_with_discovery(
                command,
                &view(),
                &catalog,
                &catalog_index,
                &graph,
                None,
                Some(discovery.store()),
            )
        };
        let search = command(
            &mut state,
            SurfaceCommand::IndexSearch {
                query: ProductText::new("fixture").expect("query"),
                limit: 10,
                cursor: None,
            },
        )
        .expect("source discovery search");
        let search_json = serde_json::to_string(&search).expect("search JSON");
        assert!(search_json.contains(purl), "{search_json}");
        assert!(search_json.contains("discovered"), "{search_json}");
        let explored = command(
            &mut state,
            SurfaceCommand::Explore {
                query: Some(ProductText::new("fixture").expect("query")),
                limit: 10,
            },
        )
        .expect("explore discovered package");
        assert_eq!(explored.id(), backend_library::CommandId::Explore);
        explored
            .admit(backend_library::CommandId::Explore)
            .expect("admitted explore evidence");
        let SurfaceReply::ExploredSearchPage(page) = &explored else {
            panic!("explore must retain discovery authority");
        };
        let SurfaceReply::IndexSearchPage(search_page) = &search else {
            panic!("search page");
        };
        assert_eq!(page.hits, search_page.hits);
        let encoded = serde_json::to_vec(&explored).expect("encode explored reply");
        let decoded: SurfaceReply =
            serde_json::from_slice(&encoded).expect("decode explored reply");
        assert_eq!(decoded.id(), backend_library::CommandId::Explore);

        let reply = command(
            &mut state,
            SurfaceCommand::Package {
                package: package.clone(),
            },
        )
        .expect("pinned source metadata");
        let SurfaceReply::PackageDiscovery {
            observation:
                backend_library::RegistryPackageDiscoveryObservation::Observed { candidates },
            ..
        } = reply
        else {
            panic!("pinned discovery must not become acquired package facts");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].coordinate, coordinate);
        let source_advisories = &candidates[0].metadata.advisories;
        let SurfaceReply::Advisory(authority) =
            read_advisory(&package, None, &catalog, &catalog_index).expect("advisory authority")
        else {
            panic!("advisory reply");
        };
        assert_eq!(
            authority.coverage,
            backend_library::AdvisoryCoverage::Unknown
        );
        if ecosystem == RegistryEcosystem::Pypi {
            assert!(
                matches!(source_advisories, RegistryEvidenceFacet::Known(values) if values.is_empty()),
                "the exact scoped PyPI source list is an empty source observation: {source_advisories:?}"
            );
        } else {
            assert!(matches!(source_advisories, RegistryEvidenceFacet::Absent));
        }
        let candidate_json = serde_json::to_string(&candidates[0]).expect("candidate JSON");
        assert!(
            !candidate_json.contains("readme witness"),
            "README is not retained in discovery"
        );
        assert!(
            !candidate_json.contains(">=18") && !candidate_json.contains(">=3.10"),
            "engine requirements are not retained in discovery"
        );

        let deps = command(
            &mut state,
            SurfaceCommand::Dependencies {
                package: package.clone(),
            },
        )
        .expect("dependency evidence state");
        assert!(
            matches!(
                deps,
                SurfaceReply::Dependencies(DependencyFacts::Unavailable(_))
            ),
            "discovery alone must not imply acquired dependency facts"
        );
        match command(&mut state, SurfaceCommand::PackageVersions { package }) {
            Ok(SurfaceReply::PackageVersions(rows)) if rows.is_empty() => gaps.push(format!(
                "{purl}: unacquired versions returned unqualified empty"
            )),
            Err(reason) if reason.contains("discover") || reason.contains("acquir") => {}
            other => panic!("unexpected versions response: {other:?}"),
        }
        drop(discovery);
        let _ = fs::remove_dir_all(root);
    }
    assert!(gaps.is_empty(), "unqualified discovery evidence: {gaps:?}");
}

#[test]
fn retained_source_advisory_is_a_separate_historical_package_claim() {
    use backend_engine::registry::{
        DiscoveryAdvisory, DiscoveryFacet, DiscoveryMetadata, DiscoverySourceIdentity,
    };

    let root = fixture("historical-source-advisory");
    let source = DiscoverySourceIdentity::from_parts(RegistryEcosystem::Npm, [31; 32]);
    let coordinate = backend_engine::registry::PackageCoordinate::parse("pkg:npm/history@1.0.0")
        .expect("coordinate");
    let make_batch = |sequence, observed_millis, advisories| {
        let observed_at = DiscoveryObservedAt::from_unix_millis(observed_millis);
        DiscoveryBatch {
            source,
            expected_base_sequence: sequence,
            previous_cursor: DiscoveryCursor::default(),
            next_cursor: DiscoveryCursor::default(),
            source_high_watermark: DiscoveryCursor::default(),
            caught_up: false,
            observed_at,
            completeness: DiscoveryCompleteness::Windowed,
            facts: vec![DiscoveryFact {
                source,
                coordinate: coordinate.clone(),
                standing: DiscoveryStanding::Published,
                observed_at,
                source_event: DiscoverySourceEvent::Snapshot,
                source_event_time: None,
                proof: if sequence == 0 { [7; 32] } else { [8; 32] },
                metadata: DiscoveryMetadata {
                    advisories,
                    ..DiscoveryMetadata::default()
                },
            }],
            package_retractions: Vec::new(),
        }
    };
    let advisory = DiscoveryAdvisory {
        id: "GHSA-source-fixture".to_owned(),
        aliases: DiscoveryFacet::Unknown,
        summary: DiscoveryFacet::Unknown,
        severity: DiscoveryFacet::Unknown,
        fixed_in: DiscoveryFacet::Unknown,
    };
    let mut discovery = DiscoveryStore::open(root.join("history.journal")).expect("store");
    discovery
        .commit(make_batch(0, 10, DiscoveryFacet::Known(vec![advisory])))
        .expect("positive");
    discovery
        .commit(make_batch(1, 20, DiscoveryFacet::Unknown))
        .expect("unknown refresh");
    let reply = discovered_package(Some(&discovery), &PackageReference::Purl(coordinate))
        .expect("projection")
        .expect("recorded package");
    let SurfaceReply::PackageDiscovery {
        observation: backend_library::RegistryPackageDiscoveryObservation::Observed { candidates },
        ..
    } = reply
    else {
        panic!("package discovery reply")
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].metadata.advisories,
        RegistryEvidenceFacet::Unknown
    );
    assert_eq!(candidates[0].proof, [8; 32]);
    assert!(matches!(
        candidates[1].freshness,
        RegistryDiscoveryFreshness::Historical {
            observed_at_millis: 10
        }
    ));
    assert_eq!(candidates[1].proof, [7; 32]);
    assert!(
        matches!(&candidates[1].metadata.advisories, RegistryEvidenceFacet::Known(values) if values.len() == 1)
    );
    drop(discovery);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn advisory_search_retains_historical_source_provenance_after_unknown_refresh() {
    use backend_engine::registry::{
        DiscoveryAdvisory, DiscoveryFacet, DiscoveryMetadata, DiscoverySourceIdentity,
    };
    use std::os::unix::fs::PermissionsExt as _;

    let root = fixture("historical-advisory-search");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private root");
    let journal = root.join("discovery.journal");
    let source = DiscoverySourceIdentity::from_parts(RegistryEcosystem::Npm, [31; 32]);
    let coordinate = backend_engine::registry::PackageCoordinate::parse("pkg:npm/history@1.0.0")
        .expect("coordinate");
    let batch = |sequence, observed_millis, advisories| {
        let observed_at = DiscoveryObservedAt::from_unix_millis(observed_millis);
        DiscoveryBatch {
            source,
            expected_base_sequence: sequence,
            previous_cursor: DiscoveryCursor::default(),
            next_cursor: DiscoveryCursor::default(),
            source_high_watermark: DiscoveryCursor::default(),
            caught_up: false,
            observed_at,
            completeness: DiscoveryCompleteness::Windowed,
            facts: vec![DiscoveryFact {
                source,
                coordinate: coordinate.clone(),
                standing: DiscoveryStanding::Published,
                observed_at,
                source_event: DiscoverySourceEvent::Snapshot,
                source_event_time: None,
                proof: if sequence == 0 { [7; 32] } else { [8; 32] },
                metadata: DiscoveryMetadata {
                    advisories,
                    description: DiscoveryFacet::Known(
                        if sequence == 0 { "old" } else { "new metadata" }.to_owned(),
                    ),
                    ..DiscoveryMetadata::default()
                },
            }],
            package_retractions: Vec::new(),
        }
    };
    let advisory = DiscoveryAdvisory {
        id: "GHSA-source-fixture".to_owned(),
        aliases: DiscoveryFacet::Unknown,
        summary: DiscoveryFacet::Unknown,
        severity: DiscoveryFacet::Unknown,
        fixed_in: DiscoveryFacet::Unknown,
    };
    let mut store = DiscoveryStore::open(journal.clone()).expect("store");
    store
        .commit(batch(0, 10, DiscoveryFacet::Known(vec![advisory])))
        .expect("positive");
    let mut product = ProductState::open(root.join("product.json")).expect("product");
    let catalog = [];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    let search = |product: &mut ProductState, store: &DiscoveryStore, query: &str| {
        product
            .execute_with_discovery(
                SurfaceCommand::IndexSearch {
                    query: ProductText::new(query).expect("query"),
                    limit: 10,
                    cursor: None,
                },
                &view(),
                &catalog,
                &index,
                &graph,
                None,
                Some(store),
            )
            .expect("search")
    };
    let first = search(&mut product, &store, "GHSA-source-fixture");
    let SurfaceReply::IndexSearchPage(first) = first else {
        panic!("search page")
    };
    assert_eq!(
        first.hits.len(),
        1,
        "the positive must initially be searchable"
    );
    store
        .commit(batch(1, 20, DiscoveryFacet::Unknown))
        .expect("unknown refresh");
    for stage in 0..2 {
        if stage == 1 {
            drop(store);
            store = DiscoveryStore::open(journal.clone()).expect("reopen");
            product =
                ProductState::open(root.join("product-reopened.json")).expect("reopen product");
        }
        let SurfaceReply::IndexSearchPage(page) =
            search(&mut product, &store, "GHSA-source-fixture")
        else {
            panic!("search page")
        };
        assert_eq!(
            page.hits.len(),
            1,
            "historical advisory must remain searchable at stage {stage}"
        );
        let RegistrySearchHit::PackageGroup(group) = &page.hits[0] else {
            panic!("package group")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("source release")
        };
        assert_eq!(candidate.coordinate, coordinate);
        assert_eq!(candidate.source, source.id());
        assert_eq!(candidate.proof, [7; 32]);
        assert!(matches!(
            candidate.freshness,
            RegistryDiscoveryFreshness::Historical {
                observed_at_millis: 10
            }
        ));
        assert!(
            matches!(&candidate.metadata.advisories, RegistryEvidenceFacet::Known(values) if values.len() == 1 && values[0].id.as_str() == "GHSA-source-fixture")
        );
        let current = store
            .fact(source, coordinate.as_str())
            .expect("current fact");
        assert_eq!(current.metadata.advisories, DiscoveryFacet::Unknown);
        assert_eq!(
            current.metadata.description,
            DiscoveryFacet::Known("new metadata".to_owned())
        );
        assert_eq!(current.proof, [8; 32]);
        assert_eq!(current.observed_at.as_unix_millis(), 20);
        let SurfaceReply::IndexSearchPage(page) = search(&mut product, &store, "new metadata")
        else {
            panic!("description search page")
        };
        let [RegistrySearchHit::PackageGroup(group)] = page.hits.as_ref() else {
            panic!("description should find current release")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("current source release")
        };
        assert_eq!(candidate.proof, [8; 32]);
        assert_eq!(
            candidate.metadata.advisories,
            RegistryEvidenceFacet::Unknown
        );
    }
    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn mixed_facet_advisory_search_preserves_historical_provenance() {
    use backend_engine::registry::{
        DiscoveryAdvisory, DiscoveryFacet, DiscoveryMetadata, DiscoverySourceIdentity,
    };
    use std::os::unix::fs::PermissionsExt as _;

    let root = fixture("mixed-facet-advisory-search");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private root");
    let journal = root.join("discovery.journal");
    let source = DiscoverySourceIdentity::from_parts(RegistryEcosystem::Npm, [31; 32]);
    let coordinate =
        backend_engine::registry::PackageCoordinate::parse("pkg:npm/ghsa-source-fixture@1.0.0")
            .expect("coordinate");
    let batch = |sequence, observed_millis, advisories| {
        let observed_at = DiscoveryObservedAt::from_unix_millis(observed_millis);
        DiscoveryBatch {
            source,
            expected_base_sequence: sequence,
            previous_cursor: DiscoveryCursor::default(),
            next_cursor: DiscoveryCursor::default(),
            source_high_watermark: DiscoveryCursor::default(),
            caught_up: false,
            observed_at,
            completeness: DiscoveryCompleteness::Windowed,
            facts: vec![DiscoveryFact {
                source,
                coordinate: coordinate.clone(),
                standing: DiscoveryStanding::Published,
                observed_at,
                source_event: DiscoverySourceEvent::Snapshot,
                source_event_time: None,
                proof: if sequence == 0 { [7; 32] } else { [8; 32] },
                metadata: DiscoveryMetadata {
                    advisories,
                    description: DiscoveryFacet::Known(
                        if sequence == 0 {
                            "old"
                        } else {
                            "new metadata GHSA-source-fixture"
                        }
                        .to_owned(),
                    ),
                    ..DiscoveryMetadata::default()
                },
            }],
            package_retractions: Vec::new(),
        }
    };
    let advisory = DiscoveryAdvisory {
        id: "GHSA-source-fixture".to_owned(),
        aliases: DiscoveryFacet::Unknown,
        summary: DiscoveryFacet::Unknown,
        severity: DiscoveryFacet::Unknown,
        fixed_in: DiscoveryFacet::Unknown,
    };
    let mut store = DiscoveryStore::open(journal.clone()).expect("store");
    store
        .commit(batch(0, 10, DiscoveryFacet::Known(vec![advisory])))
        .expect("positive");
    let mut product = ProductState::open(root.join("product.json")).expect("product");
    let catalog = [];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    let search = |product: &mut ProductState, store: &DiscoveryStore, query: &str| {
        product
            .execute_with_discovery(
                SurfaceCommand::IndexSearch {
                    query: ProductText::new(query).expect("query"),
                    limit: 10,
                    cursor: None,
                },
                &view(),
                &catalog,
                &index,
                &graph,
                None,
                Some(store),
            )
            .expect("search")
    };
    let first = search(&mut product, &store, "GHSA-source-fixture");
    let SurfaceReply::IndexSearchPage(first) = first else {
        panic!("search page")
    };
    assert_eq!(
        first.hits.len(),
        1,
        "the positive must initially be searchable"
    );
    store
        .commit(batch(1, 20, DiscoveryFacet::Unknown))
        .expect("unknown refresh");
    let mut issues = Vec::new();
    for stage in 0..2 {
        if stage == 1 {
            drop(store);
            store = DiscoveryStore::open(journal.clone()).expect("reopen");
            product =
                ProductState::open(root.join("product-reopened.json")).expect("reopen product");
        }
        let SurfaceReply::IndexSearchPage(page) =
            search(&mut product, &store, "GHSA-source-fixture")
        else {
            panic!("search page")
        };
        assert_eq!(
            page.hits.len(),
            1,
            "historical advisory must remain searchable at stage {stage}"
        );
        let RegistrySearchHit::PackageGroup(group) = &page.hits[0] else {
            panic!("package group")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("source release")
        };
        assert_eq!(candidate.coordinate, coordinate);
        assert_eq!(candidate.source, source.id());
        if candidate.proof != [7; 32]
            || !matches!(
                candidate.freshness,
                RegistryDiscoveryFreshness::Historical {
                    observed_at_millis: 10
                }
            )
            || !matches!(&candidate.metadata.advisories, RegistryEvidenceFacet::Known(values) if values.len() == 1 && values[0].id.as_str() == "GHSA-source-fixture")
        {
            issues.push(format!(
                "stage {stage}: advisory/name collision hydrated {candidate:?}"
            ));
        }
        let current = store
            .fact(source, coordinate.as_str())
            .expect("current fact");
        assert_eq!(current.metadata.advisories, DiscoveryFacet::Unknown);
        assert_eq!(
            current.metadata.description,
            DiscoveryFacet::Known("new metadata GHSA-source-fixture".to_owned())
        );
        assert_eq!(current.proof, [8; 32]);
        assert_eq!(current.observed_at.as_unix_millis(), 20);
        let SurfaceReply::IndexSearchPage(page) = search(&mut product, &store, "new metadata")
        else {
            panic!("description search page")
        };
        let [RegistrySearchHit::PackageGroup(group)] = page.hits.as_ref() else {
            panic!("description should find current release")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("current source release")
        };
        assert_eq!(candidate.proof, [8; 32]);
        assert_eq!(
            candidate.metadata.advisories,
            RegistryEvidenceFacet::Unknown
        );
        assert!(matches!(
            candidate.freshness,
            RegistryDiscoveryFreshness::Expired {
                observed_at_millis: 20,
                ..
            } | RegistryDiscoveryFreshness::Historical {
                observed_at_millis: 20
            }
        ));
        assert_eq!(
            group.release_match_scope,
            RegistryReleaseMatchScope::ReleaseMatches
        );

        let SurfaceReply::IndexSearchPage(page) = search(&mut product, &store, coordinate.as_str())
        else {
            panic!("exact-coordinate search page")
        };
        let [RegistrySearchHit::PackageGroup(group)] = page.hits.as_ref() else {
            panic!("exact-coordinate search must find current release")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("exact current source release")
        };
        assert_eq!(candidate.proof, [8; 32]);
        assert_eq!(
            candidate.metadata.advisories,
            RegistryEvidenceFacet::Unknown
        );
        assert!(matches!(
            candidate.freshness,
            RegistryDiscoveryFreshness::Expired {
                observed_at_millis: 20,
                ..
            } | RegistryDiscoveryFreshness::Historical {
                observed_at_millis: 20
            }
        ));
        assert_eq!(
            group.release_match_scope,
            RegistryReleaseMatchScope::ReleaseMatches
        );

        let SurfaceReply::IndexSearchPage(page) =
            search(&mut product, &store, "new metadata GHSA-source-fixture")
        else {
            panic!("mixed search page")
        };
        let [RegistrySearchHit::PackageGroup(group)] = page.hits.as_ref() else {
            panic!("mixed query must find the source release")
        };
        let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
            panic!("mixed source release")
        };
        if group.release_match_scope != RegistryReleaseMatchScope::LineageMetadataOnly
            || candidate.proof != [7; 32]
            || candidate.source != source.id()
            || !matches!(
                candidate.freshness,
                RegistryDiscoveryFreshness::Historical {
                    observed_at_millis: 10
                }
            )
            || !matches!(&candidate.metadata.advisories, RegistryEvidenceFacet::Known(values) if values.len() == 1 && values[0].id.as_str() == "GHSA-source-fixture")
        {
            issues.push(format!(
                "stage {stage}: mixed query scope {:?}, candidate {candidate:?}",
                group.release_match_scope
            ));
        }
    }
    assert!(issues.is_empty(), "mixed-facet provenance: {issues:?}");
    drop(store);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn pypi_scoped_positive_survives_unknown_http_refresh_with_historical_provenance() {
    use backend_engine::registry::DiscoveryFacet;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt as _;
    use std::thread;
    use std::time::Duration;

    let root = fixture("pypi-historical-http");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private root");
    let journal = root.join("discovery.journal");
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback");
    let address = listener.local_addr().expect("address");
    let worker = thread::spawn(move || {
        for body in [
            r#"{"info":{"name":"history","version":"1.0.0","summary":"old metadata"},"vulnerabilities":[{"id":"GHSA-pypi-history"}],"releases":{"1.0.0":[{"yanked":false}]}}"#,
            r#"{"info":{"name":"history","version":"1.0.0","summary":"new metadata"},"releases":{"1.0.0":[{"yanked":false}]}}"#,
        ] {
            let (mut stream, _) = listener.accept().expect("registry request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).expect("headers");
                request.push(byte[0]);
                assert!(request.len() < 16 * 1024);
            }
            assert!(request.starts_with(b"GET /pypi/history/json "));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("response");
        }
    });
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Pypi, format!("http://{address}"))
        .expect("endpoint");
    let coordinate = backend_engine::registry::PackageCoordinate::parse("pkg:pypi/history@1.0.0")
        .expect("coordinate");
    let mut gateway =
        crate::discovery::exact_metadata_fixture_gateway(endpoint.clone(), journal.clone());
    assert!(
        gateway.observe_package(&coordinate).is_none(),
        "first observation"
    );
    let (source, first) = gateway
        .store()
        .facts()
        .find(|(_, fact)| fact.coordinate == coordinate)
        .map(|(source, fact)| (*source, fact.clone()))
        .expect("first point-source fact");
    assert_eq!(
        first.metadata.advisory_release_scope.as_deref(),
        Some("1.0.0")
    );
    assert!(matches!(&first.metadata.advisories, DiscoveryFacet::Known(items) if items.len() == 1));
    drop(gateway);
    thread::sleep(Duration::from_millis(5));
    let mut gateway =
        crate::discovery::exact_metadata_fixture_gateway(endpoint.clone(), journal.clone());
    assert!(
        gateway.observe_package(&coordinate).is_none(),
        "refresh observation"
    );
    worker.join().expect("HTTP worker");
    let current = gateway
        .store()
        .fact(source, coordinate.as_str())
        .expect("current")
        .clone();
    assert_eq!(current.metadata.advisories, DiscoveryFacet::Unknown);
    assert_eq!(
        current.metadata.description,
        DiscoveryFacet::Known("new metadata".to_owned())
    );
    assert!(current.observed_at.as_unix_millis() > first.observed_at.as_unix_millis());
    assert_ne!(current.proof, first.proof);

    let catalog = [];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    let package = PackageReference::Purl(coordinate.clone());
    let mut product = ProductState::open(root.join("product.json")).expect("product");
    let mut missing = Vec::new();
    for stage in 0..2 {
        if stage == 1 {
            drop(gateway);
            gateway =
                crate::discovery::exact_metadata_fixture_gateway(endpoint.clone(), journal.clone());
            product = ProductState::open(root.join("reopened.json")).expect("reopened product");
        }
        let store = gateway.store();
        let selected = store.fact(source, coordinate.as_str()).expect("selected");
        assert_eq!(selected.metadata.advisories, DiscoveryFacet::Unknown);
        assert_eq!(
            selected.metadata.description,
            DiscoveryFacet::Known("new metadata".to_owned())
        );
        assert_eq!(selected.observed_at, current.observed_at);
        assert_eq!(selected.proof, current.proof);
        let execute = |product: &mut ProductState, command| {
            product
                .execute_with_discovery(
                    command,
                    &view(),
                    &catalog,
                    &index,
                    &graph,
                    None,
                    Some(store),
                )
                .expect("product read")
        };
        let exact = execute(
            &mut product,
            SurfaceCommand::Package {
                package: package.clone(),
            },
        );
        let SurfaceReply::PackageDiscovery {
            observation:
                backend_library::RegistryPackageDiscoveryObservation::Observed { candidates },
            ..
        } = exact
        else {
            panic!("exact discovery")
        };
        let old = candidates
            .iter()
            .find(|candidate| candidate.proof == first.proof);
        if let Some(old) = old {
            assert_eq!(old.source, source.id());
            assert!(
                matches!(old.freshness, RegistryDiscoveryFreshness::Historical { observed_at_millis } if observed_at_millis == first.observed_at.as_unix_millis())
            );
            assert!(
                matches!(&old.metadata.advisories, RegistryEvidenceFacet::Known(items) if items.len() == 1)
            );
        } else {
            missing.push(format!("stage {stage}: exact historical advisory absent"));
        }
        let search = execute(
            &mut product,
            SurfaceCommand::IndexSearch {
                query: ProductText::new("GHSA-pypi-history").expect("query"),
                limit: 10,
                cursor: None,
            },
        );
        let SurfaceReply::IndexSearchPage(page) = search else {
            panic!("search page")
        };
        let old_search = page.hits.iter().any(|hit| matches!(hit,
            RegistrySearchHit::PackageGroup(group) if group.releases.iter().any(|release| matches!(release,
                RegistrySearchRelease::Discovered(candidate) if candidate.proof == first.proof
                && candidate.source == source.id()
                && matches!(candidate.freshness, RegistryDiscoveryFreshness::Historical { observed_at_millis } if observed_at_millis == first.observed_at.as_unix_millis())
            ))));
        if !old_search {
            missing.push(format!("stage {stage}: historical advisory search absent"));
        }
    }
    assert!(missing.is_empty(), "lost scoped PyPI history: {missing:?}");
    drop(gateway);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn retained_observation_description_and_alias_collision_keep_honest_scope() {
    use backend_engine::registry::{
        DiscoveryAdvisory, DiscoveryFacet, DiscoveryMetadata, DiscoverySourceIdentity,
    };
    use std::os::unix::fs::PermissionsExt as _;

    // The first query was fully witnessed by the old description and old
    // advisory. The second combines a new description with an old CVE alias
    // that is also the current package name.
    let mut issues = Vec::new();
    for (name, old_description, new_description, alias, mixed_query, expected_scope) in [
        (
            "renderer-app",
            "renderer",
            "renderer",
            None,
            "renderer GHSA-source-fixture",
            RegistryReleaseMatchScope::ReleaseMatches,
        ),
        (
            "cve-2026-12345",
            "old description",
            "renderer CVE-2026-12345",
            Some("CVE-2026-12345"),
            "renderer CVE-2026-12345",
            RegistryReleaseMatchScope::LineageMetadataOnly,
        ),
    ] {
        let root = fixture("retained-description-alias");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private root");
        let journal = root.join("discovery.journal");
        let source = DiscoverySourceIdentity::from_parts(RegistryEcosystem::Npm, [44; 32]);
        let coordinate =
            backend_engine::registry::PackageCoordinate::parse(format!("pkg:npm/{name}@1.0.0"))
                .expect("coordinate");
        let batch = |sequence, observed_millis, advisories, description: &str| {
            let observed_at = DiscoveryObservedAt::from_unix_millis(observed_millis);
            DiscoveryBatch {
                source,
                expected_base_sequence: sequence,
                previous_cursor: DiscoveryCursor::default(),
                next_cursor: DiscoveryCursor::default(),
                source_high_watermark: DiscoveryCursor::default(),
                caught_up: false,
                observed_at,
                completeness: DiscoveryCompleteness::Windowed,
                facts: vec![DiscoveryFact {
                    source,
                    coordinate: coordinate.clone(),
                    standing: DiscoveryStanding::Published,
                    observed_at,
                    source_event: DiscoverySourceEvent::Snapshot,
                    source_event_time: None,
                    proof: if sequence == 0 { [7; 32] } else { [8; 32] },
                    metadata: DiscoveryMetadata {
                        advisories,
                        description: DiscoveryFacet::Known(description.to_owned()),
                        ..DiscoveryMetadata::default()
                    },
                }],
                package_retractions: Vec::new(),
            }
        };
        let advisory = DiscoveryAdvisory {
            id: "GHSA-source-fixture".to_owned(),
            aliases: alias.map_or(DiscoveryFacet::Unknown, |value| {
                DiscoveryFacet::Known(vec![value.to_owned()])
            }),
            summary: DiscoveryFacet::Unknown,
            severity: DiscoveryFacet::Unknown,
            fixed_in: DiscoveryFacet::Unknown,
        };
        let mut store = DiscoveryStore::open(journal.clone()).expect("store");
        store
            .commit(batch(
                0,
                10,
                DiscoveryFacet::Known(vec![advisory]),
                old_description,
            ))
            .expect("old observation");
        let old_fact = store.fact(source, coordinate.as_str()).expect("old fact");
        let (mentions_identifier, observation_matches) =
            super::super::discovery_search::advisory_query_provenance(
                &old_fact.metadata,
                &old_fact.coordinate,
                mixed_query,
            );
        if !mentions_identifier
            || observation_matches != (expected_scope == RegistryReleaseMatchScope::ReleaseMatches)
        {
            issues.push(format!("{name}: old observation match misclassified: identifier={mentions_identifier}, observation_matches={observation_matches}"));
        }
        let mut product = ProductState::open(root.join("product.json")).expect("product");
        let catalog = [];
        let index = CatalogLookupIndex::from_catalog(&catalog);
        let graph = indexed_graph(Vec::new());
        let search = |product: &mut ProductState, store: &DiscoveryStore, query: &str| {
            product
                .execute_with_discovery(
                    SurfaceCommand::IndexSearch {
                        query: ProductText::new(query).expect("query"),
                        limit: 10,
                        cursor: None,
                    },
                    &view(),
                    &catalog,
                    &index,
                    &graph,
                    None,
                    Some(store),
                )
                .expect("search")
        };
        // Build the initial index so stage zero covers incremental refresh.
        assert!(
            matches!(search(&mut product, &store, "GHSA-source-fixture"), SurfaceReply::IndexSearchPage(page) if page.hits.len() == 1)
        );
        store
            .commit(batch(1, 20, DiscoveryFacet::Unknown, new_description))
            .expect("new unknown observation");
        for stage in 0..2 {
            if stage == 1 {
                drop(store);
                store = DiscoveryStore::open(journal.clone()).expect("reopen");
                product = ProductState::open(root.join("reopened.json")).expect("product reopen");
            }
            let retained = store.retained_advisory(source, coordinate.as_str());
            if retained.is_none_or(|fact| {
                fact.metadata.description != DiscoveryFacet::Known(old_description.to_owned())
                    || fact.proof != [7; 32]
                    || fact.observed_at.as_unix_millis() != 10
            }) {
                issues.push(format!("{name} stage {stage}: retained source observation lost its description or provenance"));
            }
            for (query, historical, scope) in [
                ("renderer", false, RegistryReleaseMatchScope::ReleaseMatches),
                (
                    alias.unwrap_or("GHSA-source-fixture"),
                    true,
                    RegistryReleaseMatchScope::ReleaseMatches,
                ),
                (mixed_query, true, expected_scope),
            ] {
                let SurfaceReply::IndexSearchPage(page) = search(&mut product, &store, query)
                else {
                    panic!("search page")
                };
                let [RegistrySearchHit::PackageGroup(group)] = page.hits.as_ref() else {
                    issues.push(format!(
                        "{name} stage {stage} query {query}: expected group: {:?}",
                        page.hits
                    ));
                    continue;
                };
                let [RegistrySearchRelease::Discovered(candidate)] = group.releases.as_ref() else {
                    issues.push(format!(
                        "{name} stage {stage} query {query}: expected one source release"
                    ));
                    continue;
                };
                let correct_fact = if historical {
                    candidate.proof == [7; 32]
                        && matches!(
                            candidate.freshness,
                            RegistryDiscoveryFreshness::Historical {
                                observed_at_millis: 10
                            }
                        )
                        && matches!(&candidate.metadata.advisories, RegistryEvidenceFacet::Known(items) if items.len() == 1)
                } else {
                    candidate.proof == [8; 32]
                        && matches!(
                            candidate.freshness,
                            RegistryDiscoveryFreshness::Expired {
                                observed_at_millis: 20,
                                ..
                            } | RegistryDiscoveryFreshness::Historical {
                                observed_at_millis: 20
                            }
                        )
                        && candidate.metadata.advisories == RegistryEvidenceFacet::Unknown
                };
                if group.release_match_scope != scope
                    || candidate.source != source.id()
                    || candidate.coordinate != coordinate
                    || !correct_fact
                {
                    issues.push(format!(
                        "{name} stage {stage} query {query}: scope {:?}, candidate {candidate:?}",
                        group.release_match_scope
                    ));
                }
            }
        }
        drop(store);
        let _ = fs::remove_dir_all(root);
    }
    assert!(
        issues.is_empty(),
        "retained observation provenance: {issues:?}"
    );
}

#[cfg(unix)]
#[test]
fn fake_nuget_service_pages_to_pinned_product_reads() {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
        time::Duration,
    };
    let root = fixture("nuget-exact-discovery");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let service = serde_json::json!({"version":"3.0.0", "resources":[{"@id": format!("{base}/v3/registrations/"), "@type":["RegistrationsBaseUrl/3.0.0-beta", "RegistrationsBaseUrl/3.6.0"]}]}).to_string();
    let index = serde_json::json!({"count":2,"items":[{"count":1,"items":[{"catalogEntry":{"id":"Microsoft.EntityFrameworkCore","version":"1.0.0","listed":true}}]},{"@id":format!("{base}/v3/unpredictable-page"),"count":1,"lower":"10.0.0","upper":"10.0.13"}]}).to_string();
    let page = serde_json::json!({"count":1,"items":[{"catalogEntry":format!("{base}/v3/catalog/unpredictable-object")}]}).to_string();
    let catalog = serde_json::json!({"id":"Microsoft.EntityFrameworkCore","version":"10.0.13","listed":true,"description":"Entity Framework metadata from private fixture","licenseExpression":"MIT","published":"2026-11-01T00:00:00Z"}).to_string();
    let worker = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        for (path, body) in [
            ("/v3/index.json", service),
            (
                "/v3/registrations/microsoft.entityframeworkcore/index.json",
                index,
            ),
            ("/v3/unpredictable-page", page),
            ("/v3/catalog/unpredictable-object", catalog),
        ] {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing NuGet request {path}"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 16 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(&format!("GET {path} ")), "{request}");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Nuget,
        format!("{base}/v3/catalog0/index.json"),
    )
    .unwrap();
    let mut gateway =
        crate::discovery::exact_metadata_fixture_gateway(endpoint, root.join("discovery.journal"));
    let purl = "pkg:nuget/Microsoft.EntityFrameworkCore@10.0.13";
    let coordinate = backend_engine::registry::PackageCoordinate::parse(purl).unwrap();
    assert!(
        gateway.observe_package(&coordinate).is_none(),
        "exact NuGet source metadata must be admitted"
    );
    worker.join().unwrap();
    let mut state = ProductState::open(root.join("product.json")).unwrap();
    let catalog = [];
    let index = CatalogLookupIndex::from_catalog(&catalog);
    let graph = indexed_graph(Vec::new());
    let read = |state: &mut ProductState, command| {
        state.execute_with_discovery(
            command,
            &view(),
            &catalog,
            &index,
            &graph,
            None,
            Some(gateway.store()),
        )
    };
    let reply = read(
        &mut state,
        SurfaceCommand::Package {
            package: PackageReference::parse(purl).unwrap(),
        },
    )
    .unwrap();
    reply.admit(backend_library::CommandId::Package).unwrap();
    let json = serde_json::to_string(&reply).unwrap();
    assert!(
        json.contains(purl) && json.contains("Entity Framework metadata from private fixture"),
        "{json}"
    );
    assert!(
        matches!(reply, SurfaceReply::PackageDiscovery { .. }),
        "metadata is not acquisition authority"
    );
    let explored = read(
        &mut state,
        SurfaceCommand::Explore {
            query: Some(ProductText::new("Microsoft.EntityFrameworkCore").unwrap()),
            limit: 10,
        },
    )
    .unwrap();
    explored.admit(backend_library::CommandId::Explore).unwrap();
    assert!(serde_json::to_string(&explored).unwrap().contains(purl));
    drop(gateway);
    fs::remove_dir_all(root).unwrap();
}
