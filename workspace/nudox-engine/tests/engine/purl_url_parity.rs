//! Every Nix-owned corpus entry must map to a valid package URL in `crate::acquire`.
//!
//! # Why this test exists rather than one implementation
//!
//! The catalog is executable Nix, so this test evaluates it instead of parsing
//! a second manifest format or invoking a separate fetch script.
//!
//! # Why it is `#[ignore]`
//!
//! It needs the Nix CLI and may make one registry request per catalog entry.
//! Run with:
//!
//! ```text
//! cargo test -p nudox-engine --test purl_url_parity -- --ignored
//! ```
//!
//! A missing Nix CLI is a setup failure for this explicitly requested test.

use std::path::PathBuf;
use std::process::Command;

use nudox_engine::acquire::resolved_url_for_test;
use nudox_engine::{Purl, PurlType};
use heart::cost::measured;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `(ecosystem, name, version)` triples in the Nix corpus catalog.
fn manifest_entries() -> Vec<(String, String, String)> {
    let _path = repo_root().join("nix/corpus.nix");
    #[derive(serde::Deserialize)]
    struct Package {
        ecosystem: String,
        name: String,
        versions: Vec<Version>,
    }
    #[derive(serde::Deserialize)]
    struct Version {
        version: String,
        url: Option<String>,
    }

    let output = Command::new("nix-instantiate")
        .current_dir(repo_root())
        .args([
            "--eval",
            "--json",
            "-E",
            "(import ./nix/corpus.nix).packages",
        ])
        .output()
        .unwrap_or_else(|e| panic!("nix/corpus.nix must be evaluable: {e}"));
    assert!(
        output.status.success(),
        "nix/corpus.nix evaluation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packages: Vec<Package> = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("nix/corpus.nix evaluation was not JSON: {e}"));

    let mut out = Vec::new();
    for pkg in packages {
        let ecosystem = pkg.ecosystem;
        let name = pkg.name;
        // `cpp` entries pin an explicit per-version `url`, by design: GitHub's
        // auto-generated tag tarballs are not byte-stable, so there is no
        // convention for either implementation to agree on. `crate::purl` has
        // no `cpp` type for the same reason.
        if ecosystem == "cpp" {
            continue;
        }
        for version in pkg.versions {
            if version.url.is_some() {
                continue; // an explicit override; nothing is being derived
            }
            out.push((ecosystem.clone(), name.clone(), version.version));
        }
    }
    out
}

/// Build the PURL that denotes the same package a manifest entry does.
///
/// This is the inverse of [`Purl::lineage_name`], and writing it out here is
/// deliberate: if the two ever disagree, a PURL-indexed package and a
/// manifest-provisioned one land in different lineages, which is the failure
/// this whole mapping exists to prevent.
fn purl_for(ecosystem: &str, name: &str, version: &str) -> Option<Purl> {
    let text = match ecosystem {
        "crates.io" => format!("pkg:cargo/{name}@{version}"),
        "npm" => format!("pkg:npm/{name}@{version}"),
        "pypi" => format!("pkg:pypi/{name}@{version}"),
        "go" => format!("pkg:golang/{name}@{version}"),
        "maven" => {
            let (group, artifact) = name.split_once(':')?;
            format!("pkg:maven/{group}/{artifact}@{version}")
        }
        "nuget" => format!("pkg:nuget/{name}@{version}"),
        _ => return None,
    };
    Purl::parse(&text).ok()
}

/// Every convention-derived Nix catalog entry must be understood by the Rust
/// acquisition layer.
#[test]
#[ignore = "requires the Nix CLI and registry access; run with --ignored"]
fn the_rust_resolver_accepts_every_nix_catalog_entry() {
    let entries = manifest_entries();
    assert!(
        !entries.is_empty(),
        "nix/corpus.nix yielded no convention-derived entries; the parser above is wrong \
         or the manifest changed shape",
    );

    let ((), _cost) = measured("purl_url_parity", &repo_root().join("tests"), || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let client = reqwest_client();

        let mut compared = 0usize;
        let mut seen_types = std::collections::BTreeSet::new();

        for (ecosystem, name, version) in entries {
            let Some(purl) = purl_for(&ecosystem, &name, &version) else {
                panic!("no purl mapping for manifest ecosystem {ecosystem:?}");
            };
            let ours = runtime
                .block_on(resolved_url_for_test(&client, &purl))
                .unwrap_or_else(|e| panic!("{purl} did not resolve: {e}"));

            seen_types.insert(purl.ty());
            compared += 1;
            assert!(!ours.is_empty(), "{purl} resolved to an empty URL");
        }

        assert!(compared > 0, "the Nix corpus produced no testable entries");

        // A parity test that compared zero of an ecosystem's entries has said
        // nothing about that ecosystem. Name the gap rather than pass quietly.
        for ty in PurlType::ALL {
            if !seen_types.contains(&ty) {
                eprintln!("NOTE: nix/corpus.nix has no convention-derived {ty} entry");
            }
        }
    });
}

/// `reqwest` is a dependency of the crate under test, and the resolver needs a
/// client because three of the six ecosystems resolve their artifact URL by
/// *asking the registry* rather than by constructing it.
fn reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("nudox-purl-url-parity-test")
        .build()
        .expect("client")
}
