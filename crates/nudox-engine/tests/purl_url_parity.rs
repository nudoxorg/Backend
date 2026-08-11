//! `crate::acquire` and `corpus/fetch.nu` must resolve the same URLs.
//!
//! # Why this test exists rather than one implementation
//!
//! `src/acquire/mod.rs` argues at length for a Rust fetcher over shelling out
//! to Nushell, and the honest cost of that decision is a second copy of the
//! per-ecosystem URL conventions: crates.io's `static.crates.io` path, the Go
//! proxy's `!`-escape, Maven's `-sources` classifier, NuGet's double-lowercased
//! flat-container path.
//!
//! Two copies of a convention drift. So this test makes the drift *loud*: it
//! reads `corpus/manifest.toml`, asks `fetch.nu`'s own `hash-url` subcommand
//! what URL it would use for each entry, asks this crate the same question, and
//! requires the answers to match. Neither implementation is the reference — a
//! disagreement is a finding either way.
//!
//! # Why it is `#[ignore]`
//!
//! It needs `nu` on `PATH` and it makes one network request per manifest entry
//! (`hash-url` downloads in order to hash). Run with:
//!
//! ```text
//! cargo test -p nudox-engine --test purl_url_parity -- --ignored
//! ```
//!
//! A missing `nu` is reported as a skip rather than a failure: the parity claim
//! is unfalsifiable without it, and a green run on a machine that never ran the
//! comparison would be worse than a loud skip.

use std::path::PathBuf;
use std::process::Command;

use nudox_engine::acquire::resolved_url_for_test;
use nudox_engine::{Purl, PurlType};
use nudox_test_support::measured;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `(ecosystem, name, version)` triples in `corpus/manifest.toml`, as
/// `fetch.nu` spells them.
fn manifest_entries() -> Vec<(String, String, String)> {
    let path = repo_root().join("corpus/manifest.toml");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("corpus/manifest.toml must be readable: {e}"));
    let doc: toml::Value = body.parse().expect("corpus/manifest.toml must parse");

    let mut out = Vec::new();
    let Some(packages) = doc.get("packages").and_then(toml::Value::as_array) else {
        return out;
    };
    for pkg in packages {
        let (Some(ecosystem), Some(name)) = (
            pkg.get("ecosystem").and_then(toml::Value::as_str),
            pkg.get("name").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        // `cpp` entries pin an explicit per-version `url`, by design: GitHub's
        // auto-generated tag tarballs are not byte-stable, so there is no
        // convention for either implementation to agree on. `crate::purl` has
        // no `cpp` type for the same reason.
        if ecosystem == "cpp" {
            continue;
        }
        for version in pkg
            .get("versions")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            if version.get("url").is_some() {
                continue; // an explicit override; nothing is being derived
            }
            let Some(v) = version.get("version").and_then(toml::Value::as_str) else {
                continue;
            };
            out.push((ecosystem.to_owned(), name.to_owned(), v.to_owned()));
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

/// Ask `fetch.nu` what URL it resolves for this entry.
///
/// `hash-url` prints `INFO: resolved URL: <url>` before downloading, so the
/// line can be read without waiting for the hash — but the process is still
/// allowed to finish, because killing it mid-download is how a test starts
/// leaving temp directories behind.
fn fetch_nu_url(ecosystem: &str, name: &str, version: &str) -> Option<String> {
    let output = Command::new("nu")
        .current_dir(repo_root())
        .args(["corpus/fetch.nu", "hash-url", ecosystem, name, version])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("INFO: resolved URL: "))
        .map(|u| u.trim().to_owned())
}

/// The claim `src/acquire/mod.rs` makes in its "why Rust and not Nushell"
/// section: the duplication is bounded to URL resolution, and drift is a test
/// failure rather than a surprise.
#[test]
#[ignore = "requires `nu` on PATH and downloads every manifest entry; run with --ignored"]
fn the_rust_resolver_and_fetch_nu_agree_on_every_manifest_entry() {
    if Command::new("nu").arg("--version").output().is_err() {
        eprintln!(
            "SKIP: `nu` is not on PATH, so the parity claim in src/acquire/mod.rs cannot be \
             checked. This is a skip and not a pass."
        );
        return;
    }

    let entries = manifest_entries();
    assert!(
        !entries.is_empty(),
        "corpus/manifest.toml yielded no convention-derived entries; the parser above is wrong \
         or the manifest changed shape",
    );

    let ((), _cost) = measured("purl_url_parity", &repo_root().join("corpus"), || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let client = reqwest_client();

        let mut compared = 0usize;
        let mut mismatches = Vec::new();
        let mut seen_types = std::collections::BTreeSet::new();

        for (ecosystem, name, version) in entries {
            let Some(purl) = purl_for(&ecosystem, &name, &version) else {
                panic!("no purl mapping for manifest ecosystem {ecosystem:?}");
            };
            let Some(theirs) = fetch_nu_url(&ecosystem, &name, &version) else {
                // `hash-url` failed outright (a yanked package, a network
                // blip). Nothing to compare; skip rather than blame this crate.
                continue;
            };
            let ours = runtime
                .block_on(resolved_url_for_test(&client, &purl))
                .unwrap_or_else(|e| panic!("{purl} did not resolve: {e}"));

            seen_types.insert(purl.ty());
            compared += 1;
            if ours != theirs {
                mismatches.push(format!("{purl}\n    fetch.nu: {theirs}\n    acquire : {ours}"));
            }
        }

        assert!(
            mismatches.is_empty(),
            "{} of {compared} entries resolve to different URLs:\n{}",
            mismatches.len(),
            mismatches.join("\n"),
        );

        // A parity test that compared zero of an ecosystem's entries has said
        // nothing about that ecosystem. Name the gap rather than pass quietly.
        for ty in PurlType::ALL {
            if !seen_types.contains(&ty) {
                eprintln!(
                    "NOTE: corpus/manifest.toml has no convention-derived {ty} entry, so parity \
                     for {ty} was not checked by this run."
                );
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
