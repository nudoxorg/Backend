//! Per-ecosystem registry resolution: which versions exist, where the source
//! artifact lives, and what digest (if any) the registry publishes for it.
//!
//! # Relationship to `nix build .#checks.corpus`
//!
//! [`artifact`] is a port of that script's `resolve-url`, including the two
//! rules that are not guessable: the Go module proxy's uppercase `!`-escape
//! (`golang.org/x/mod/module.EscapePath`) and the fact that maven and nuget
//! resolve to a **sources** jar and a nupkg respectively rather than to a
//! compiled artifact. `tests/purl_url_parity.rs` re-derives the same URLs by
//! running `fetch.nu` itself over `nix/corpus.nix`, so the two cannot
//! drift silently.
//!
//! [`versions`] has no counterpart in `fetch.nu` — the script never needs to
//! ask what exists, because the manifest already says. It exists here for the
//! one reason a version listing is worth a round trip: it is what turns "that
//! version is not there" into a message naming the versions that are.
//!
//! # Digest publication is not uniform, and this module does not pretend it is
//!
//! Four of the six registries publish a digest of the exact artifact bytes on
//! an endpoint reachable without extra protocol machinery. Two do not. Every
//! resolution therefore returns `Option<PublishedDigest>`, and a `None` becomes
//! [`super::Integrity::TransportOnly`] carrying the reason — never a silent
//! "verified".

use serde_json::Value;

use crate::packages::purl::{Purl, PurlType};

use super::{Error, http};

// ---------------------------------------------------------------------------
// Artifact
// ---------------------------------------------------------------------------

/// A resolved download: where the source artifact is, and what the registry
/// says it should hash to.
pub(crate) struct Artifact {
    /// The absolute HTTPS URL of the source artifact.
    pub(crate) url: String,
    /// The digest the registry publishes for these exact bytes, if it publishes
    /// one at all.
    pub(crate) expected: Option<PublishedDigest>,
}

/// A digest a registry publishes for an artifact, in whatever algorithm that
/// registry chose.
///
/// An enum rather than a normalized `(alg, hex)` pair because the comparison is
/// algorithm-specific in more than the hash function: npm publishes SRI
/// (`sha512-<base64>`), Maven publishes lowercase hex SHA-1 in a sidecar file
/// that sometimes has a trailing filename, and crates.io publishes hex SHA-256
/// inside a JSON line. Flattening them to strings early is how a comparison
/// ends up passing on two values that were never the same shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PublishedDigest {
    /// Lowercase hex SHA-256 — crates.io `cksum`, PyPI `digests.sha256`.
    Sha256Hex(String),
    /// Standard-base64 SHA-512 — npm `dist.integrity`, minus the `sha512-`
    /// prefix.
    Sha512B64(String),
    /// Lowercase hex SHA-1 — the Maven Central `.sha1` sidecar.
    Sha1Hex(String),
}

impl PublishedDigest {
    /// The name of the algorithm, for [`super::Integrity::RegistryDigest`].
    pub(crate) fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha256Hex(_) => "sha256",
            Self::Sha512B64(_) => "sha512",
            Self::Sha1Hex(_) => "sha1",
        }
    }

    /// The published value, as the registry spells it.
    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Sha256Hex(v) | Self::Sha512B64(v) | Self::Sha1Hex(v) => v,
        }
    }

    /// Recompute this digest over `bytes` and render it the same way the
    /// registry does, so the comparison is a plain string equality on two
    /// values produced by the same code path.
    pub(crate) fn compute_over(&self, bytes: &[u8]) -> String {
        use sha2::Digest as _;
        match self {
            Self::Sha256Hex(_) => hex(&sha2::Sha256::digest(bytes)),
            Self::Sha512B64(_) => {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(bytes))
            }
            Self::Sha1Hex(_) => hex(&sha1::Sha1::digest(bytes)),
        }
    }
}

/// Lowercase hex, the spelling every hex-publishing registry here uses.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing to a `String` is infallible; the `Result` exists only because
        // `fmt::Write` is also implemented for fallible sinks.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Why a given ecosystem has no digest to check, stated per ecosystem rather
/// than as one generic sentence — a reader deciding whether to trust a
/// `TransportOnly` result needs to know *what would have to change*.
pub(crate) fn no_digest_reason(ty: PurlType) -> Option<&'static str> {
    match ty {
        PurlType::Golang => Some(
            "the Go module proxy publishes no digest alongside a module zip. The authoritative \
             one is the `h1:` dirhash in sum.golang.org's signed transparency log, which is a \
             signed-note lookup plus a reimplementation of golang.org/x/mod/sumdb/dirhash — \
             neither of which this fetcher performs.",
        ),
        PurlType::NuGet => Some(
            "nuget.org's flat container serves the .nupkg with no digest sibling. The \
             authoritative `packageHash` lives in the registration catalog, two further \
             indirections away, which this fetcher does not read.",
        ),
        PurlType::Cargo | PurlType::Npm | PurlType::PyPi | PurlType::Maven => None,
    }
}

// ---------------------------------------------------------------------------
// Go module proxy escaping
// ---------------------------------------------------------------------------

/// `golang.org/x/mod/module.EscapePath`: every uppercase letter becomes `!`
/// plus its lowercase form.
///
/// This exists because module paths are case-sensitive but some filesystems the
/// proxy serves from are not. It is also why [`crate::packages::purl`] refuses to apply
/// purl-spec's "lowercase the golang type" rule: the escape would have nothing
/// to escape, and `github.com/BurntSushi/toml` would 404.
fn go_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_uppercase() {
            out.push('!');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The crates.io sparse-index path for a crate name.
///
/// The 1/2/3/nn-nn layout is the index's own convention (`cargo`'s
/// `registry/index.rs`), and the name is lowercased because the index is
/// case-folded even though crate names are not.
fn crates_index_path(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.len() {
        0 => lower,
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", &lower[..1]),
        _ => format!("{}/{}/{lower}", &lower[..2], &lower[2..4]),
    }
}

/// `groupId` → `com/google/guava`, the Maven repository layout.
fn maven_group_path(group: &str) -> String {
    group.replace('.', "/")
}

// ---------------------------------------------------------------------------
// versions
// ---------------------------------------------------------------------------

/// Every version the registry publishes for this package, newest first where
/// the registry's own ordering permits.
///
/// A 404 on the *name* endpoint becomes [`Error::UnknownPackage`]: that is
/// the one HTTP status that distinguishes a typo from everything else, and it
/// is why this probe exists at all.
pub(crate) async fn versions(client: &reqwest::Client, purl: &Purl) -> Result<Vec<String>, Error> {
    match purl.ty() {
        PurlType::Cargo => {
            let url = format!(
                "https://index.crates.io/{}",
                crates_index_path(purl.name())
            );
            let body = http::get_text(client, &url, purl).await?;
            // Newline-delimited JSON, one object per published version, in
            // publish order. Yanked versions stay listed — a yanked version is
            // still fetchable and still documentable, and hiding it would make
            // "you asked for a yanked version" indistinguishable from "that
            // version never existed".
            let mut out: Vec<String> = body
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter_map(|v| v.get("vers")?.as_str().map(str::to_owned))
                .collect();
            out.reverse();
            Ok(out)
        }
        PurlType::PyPi => {
            let url = format!("https://pypi.org/pypi/{}/json", purl.name());
            let body = http::get_json(client, &url, purl).await?;
            let mut out: Vec<String> = body
                .get("releases")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            out.reverse();
            Ok(out)
        }
        PurlType::Npm => {
            let url = format!("https://registry.npmjs.org/{}", purl.lineage_name());
            let body = http::get_json(client, &url, purl).await?;
            let mut out: Vec<String> = body
                .get("versions")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            out.reverse();
            Ok(out)
        }
        PurlType::Golang => {
            let url = format!(
                "https://proxy.golang.org/{}/@v/list",
                go_escape(&purl.lineage_name())
            );
            let body = http::get_text(client, &url, purl).await?;
            let mut out: Vec<String> = body
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            out.reverse();
            Ok(out)
        }
        PurlType::Maven => {
            let group = purl.namespace().unwrap_or_default();
            let url = format!(
                "https://repo1.maven.org/maven2/{}/{}/maven-metadata.xml",
                maven_group_path(group),
                purl.name()
            );
            let body = http::get_text(client, &url, purl).await?;
            let mut out = parse_maven_versions(&body);
            out.reverse();
            Ok(out)
        }
        PurlType::NuGet => {
            let url = format!(
                "https://api.nuget.org/v3-flatcontainer/{}/index.json",
                purl.name().to_ascii_lowercase()
            );
            let body = http::get_json(client, &url, purl).await?;
            let mut out: Vec<String> = body
                .get("versions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            out.reverse();
            Ok(out)
        }
    }
}

/// Pull `<version>…</version>` out of a `maven-metadata.xml`.
///
/// A hand scan rather than an XML parser: this crate has no XML dependency, the
/// document is machine-generated by one implementation, and the only thing we
/// want from it is a flat list of leaf text nodes with a fixed tag name. A
/// parser would buy correctness on documents Maven Central does not serve.
fn parse_maven_versions(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<version>") {
        rest = &rest[start + "<version>".len()..];
        let Some(end) = rest.find("</version>") else {
            break;
        };
        let value = rest[..end].trim();
        if !value.is_empty() {
            out.push(value.to_owned());
        }
        rest = &rest[end + "</version>".len()..];
    }
    out
}

// ---------------------------------------------------------------------------
// artifact
// ---------------------------------------------------------------------------

/// Resolve the source artifact for a fully-versioned PURL.
///
/// # Panics
///
/// Never — but it requires `purl.version()` to be `Some`. The caller
/// ([`super::acquire`]) has already turned `None` into
/// [`Error::VersionMissing`] with the version list attached, which is a
/// far more useful failure than anything this function could produce.
pub(crate) async fn artifact(
    client: &reqwest::Client,
    purl: &Purl,
) -> Result<Artifact, Error> {
    let version = purl.version().unwrap_or_default();

    match purl.ty() {
        PurlType::Cargo => {
            // One request serves both jobs: the index line carries the `cksum`
            // that GLOBAL-IR-GRAPH §2 asks us to verify *before* generating IR
            // ("so IR is provably generated from the exact bytes the registry
            // serves"), and its presence confirms the version exists.
            let index_url = format!(
                "https://index.crates.io/{}",
                crates_index_path(purl.name())
            );
            let body = http::get_text(client, &index_url, purl).await?;
            let line = body
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|v| v.get("vers").and_then(Value::as_str) == Some(version))
                .ok_or_else(|| version_not_found(purl, versions_from_index(&body)))?;
            let cksum = line
                .get("cksum")
                .and_then(Value::as_str)
                .map(|s| PublishedDigest::Sha256Hex(s.to_ascii_lowercase()));
            Ok(Artifact {
                url: format!(
                    "https://static.crates.io/crates/{name}/{name}-{version}.crate",
                    name = purl.name(),
                ),
                expected: cksum,
            })
        }

        PurlType::PyPi => {
            // Ported from `pypi-sdist-url`: the *sdist*, never a wheel — a
            // wheel has no sources for a producer to read.
            let url = format!("https://pypi.org/pypi/{}/{version}/json", purl.name());
            let body = match http::get_json(client, &url, purl).await {
                Ok(body) => body,
                Err(Error::UnknownPackage { .. }) => {
                    // PyPI answers 404 for both "no such project" and "no such
                    // release of a project that exists". Re-probe the project
                    // endpoint to tell them apart rather than blaming the name.
                    let available = versions(client, purl).await?;
                    return Err(version_not_found(purl, available));
                }
                Err(other) => return Err(other),
            };
            let sdist = body
                .get("urls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|f| f.get("packagetype").and_then(Value::as_str) == Some("sdist"))
                .ok_or_else(|| Error::NoSourceArtifact {
                    purl: purl.render(),
                    registry: purl.ty().registry(),
                    detail: "PyPI has published only built distributions (wheels) for this \
                             release, and a wheel contains no sources to document"
                        .to_owned(),
                })?;
            let expected = sdist
                .get("digests")
                .and_then(|d| d.get("sha256"))
                .and_then(Value::as_str)
                .map(|s| PublishedDigest::Sha256Hex(s.to_ascii_lowercase()));
            let url = sdist
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::NoSourceArtifact {
                    purl: purl.render(),
                    registry: purl.ty().registry(),
                    detail: "the sdist entry in PyPI's JSON carries no download url".to_owned(),
                })?
                .to_owned();
            Ok(Artifact { url, expected })
        }

        PurlType::Npm => {
            // The packument gives the tarball URL *and* the SRI digest. Taking
            // the URL from the registry rather than constructing it is the
            // difference between a resolution that works for every package and
            // one that works until a package's tarball is not where the naming
            // convention says.
            let url = format!("https://registry.npmjs.org/{}", purl.lineage_name());
            let body = http::get_json(client, &url, purl).await?;
            let entry = body
                .get("versions")
                .and_then(|v| v.get(version))
                .ok_or_else(|| {
                    let available = body
                        .get("versions")
                        .and_then(Value::as_object)
                        .map(|m| m.keys().rev().cloned().collect())
                        .unwrap_or_default();
                    version_not_found(purl, available)
                })?;
            let dist = entry.get("dist");
            let tarball = dist
                .and_then(|d| d.get("tarball"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::NoSourceArtifact {
                    purl: purl.render(),
                    registry: purl.ty().registry(),
                    detail: "the packument entry for this version carries no dist.tarball"
                        .to_owned(),
                })?
                .to_owned();
            let expected = dist
                .and_then(|d| d.get("integrity"))
                .and_then(Value::as_str)
                .and_then(|sri| sri.strip_prefix("sha512-"))
                .map(|b64| PublishedDigest::Sha512B64(b64.to_owned()))
                .or_else(|| {
                    dist.and_then(|d| d.get("shasum"))
                        .and_then(Value::as_str)
                        .map(|hex| PublishedDigest::Sha1Hex(hex.to_ascii_lowercase()))
                });
            Ok(Artifact {
                url: tarball,
                expected,
            })
        }

        PurlType::Golang => {
            let path = go_escape(&purl.lineage_name());
            let escaped_version = go_escape(version);
            Ok(Artifact {
                url: format!("https://proxy.golang.org/{path}/@v/{escaped_version}.zip"),
                expected: None,
            })
        }

        PurlType::Maven => {
            let group = purl.namespace().unwrap_or_default();
            let artifact_id = purl.name();
            let url = format!(
                "https://repo1.maven.org/maven2/{group_path}/{artifact_id}/{version}/{artifact_id}-{version}-sources.jar",
                group_path = maven_group_path(group),
            );
            // Maven Central publishes `<artifact>.sha1` next to every artifact.
            // A missing sidecar is not fatal: it degrades to TransportOnly
            // rather than failing a fetch that would otherwise succeed.
            let expected = http::get_text_optional(client, &format!("{url}.sha1"))
                .await
                .and_then(|body| {
                    // The sidecar is usually bare hex, but some uploads append
                    // "  filename" in `sha1sum` style.
                    body.split_whitespace()
                        .next()
                        .map(|h| PublishedDigest::Sha1Hex(h.to_ascii_lowercase()))
                });
            Ok(Artifact { url, expected })
        }

        PurlType::NuGet => {
            let id = purl.name().to_ascii_lowercase();
            let ver = version.to_ascii_lowercase();
            Ok(Artifact {
                url: format!("https://api.nuget.org/v3-flatcontainer/{id}/{ver}/{id}.{ver}.nupkg"),
                expected: None,
            })
        }
    }
}

/// Fetch the `go.mod` the module proxy serves *beside* the zip.
///
/// # Why this exists at all
///
/// A Go module zip does not necessarily contain a `go.mod`.
/// `github.com/pkg/errors@v0.9.1` — one of the most-depended-on modules in the
/// ecosystem — unpacks to eighteen files, none of them a manifest, because the
/// tag predates the repository adopting modules. The proxy's `/@v/<ver>.mod`
/// endpoint is where the manifest lives, and for pre-modules tags the proxy
/// *synthesizes* one; `go mod download` writes it into the module cache
/// alongside the extracted zip for exactly this reason.
///
/// Without it, `GoProducer` finds no `go.mod`, and the failure surfaces as a
/// producer error about a package that was fetched and verified perfectly.
/// `nix build .#checks.corpus` never hit this because every Go entry in
/// `nix/corpus.nix` happens to be a modules-era tag.
///
/// This was found by fetching a real module. No fixture would have contained
/// the *absence* of a file.
pub(crate) async fn go_mod(
    client: &reqwest::Client,
    purl: &Purl,
) -> Result<String, Error> {
    let url = format!(
        "https://proxy.golang.org/{}/@v/{}.mod",
        go_escape(&purl.lineage_name()),
        go_escape(purl.version().unwrap_or_default()),
    );
    http::get_text(client, &url, purl).await
}

fn versions_from_index(body: &str) -> Vec<String> {
    let mut out: Vec<String> = body
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("vers")?.as_str().map(str::to_owned))
        .collect();
    out.reverse();
    out
}

fn version_not_found(purl: &Purl, available: Vec<String>) -> Error {
    Error::VersionNotFound {
        purl: purl.render(),
        registry: purl.ty().registry(),
        requested: purl.version().unwrap_or_default().to_owned(),
        published: available.len(),
        available: available.into_iter().take(super::MAX_LISTED_VERSIONS).collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_paths_escape_uppercase_the_way_the_module_proxy_requires() {
        // `golang.org/x/mod/module.EscapePath`. Without this,
        // `github.com/BurntSushi/toml` resolves to a 404 that would be reported
        // as "no such package" about a package with 6k dependents.
        assert_eq!(
            go_escape("github.com/BurntSushi/toml"),
            "github.com/!burnt!sushi/toml"
        );
        assert_eq!(go_escape("github.com/pkg/errors"), "github.com/pkg/errors");
        // Versions are escaped too — pre-release identifiers can be uppercase.
        assert_eq!(go_escape("v1.0.0-RC1"), "v1.0.0-!r!c1");
    }

    #[test]
    fn the_crates_index_path_follows_the_sparse_index_layout() {
        assert_eq!(crates_index_path("a"), "1/a");
        assert_eq!(crates_index_path("ab"), "2/ab");
        assert_eq!(crates_index_path("abc"), "3/a/abc");
        assert_eq!(crates_index_path("serde"), "se/rd/serde");
        // Case-folded: the index is lowercase even though crate names are not.
        assert_eq!(crates_index_path("Inflector"), "in/fl/inflector");
    }

    #[test]
    fn maven_group_ids_become_repository_paths() {
        assert_eq!(maven_group_path("com.google.guava"), "com/google/guava");
    }

    #[test]
    fn maven_metadata_yields_every_published_version_in_document_order() {
        let xml = "<metadata><versioning><versions>\
            <version>1.0</version><version>1.1</version><version>2.0-rc1</version>\
            </versions></versioning></metadata>";
        assert_eq!(parse_maven_versions(xml), vec!["1.0", "1.1", "2.0-rc1"]);
    }

    #[test]
    fn an_empty_or_truncated_metadata_document_yields_no_versions_rather_than_panicking() {
        assert!(parse_maven_versions("").is_empty());
        assert!(parse_maven_versions("<metadata><version>1.0").is_empty());
    }

    #[test]
    fn a_published_digest_recomputes_in_the_registrys_own_spelling() {
        // The comparison must be between two strings produced the same way,
        // not between a hex string and a base64 one that happen to be equal
        // never.
        let bytes = b"nudox";
        let sha256 = PublishedDigest::Sha256Hex(String::new());
        assert_eq!(sha256.compute_over(bytes).len(), 64);
        assert!(sha256.compute_over(bytes).chars().all(|c| c.is_ascii_hexdigit()));

        let sha512 = PublishedDigest::Sha512B64(String::new());
        let encoded = sha512.compute_over(bytes);
        assert!(encoded.ends_with('='), "SRI sha512 is padded base64: {encoded}");

        let sha1 = PublishedDigest::Sha1Hex(String::new());
        assert_eq!(sha1.compute_over(bytes).len(), 40);
    }

    #[test]
    fn exactly_the_two_registries_without_a_reachable_digest_say_why() {
        // Not a style assertion: a `None` here means the fetcher will report
        // `Integrity::TransportOnly`, and that variant is required to carry a
        // reason. A new ecosystem that forgets one fails this test.
        assert!(no_digest_reason(PurlType::Golang).is_some());
        assert!(no_digest_reason(PurlType::NuGet).is_some());
        for ty in [
            PurlType::Cargo,
            PurlType::Npm,
            PurlType::PyPi,
            PurlType::Maven,
        ] {
            assert!(
                no_digest_reason(ty).is_none(),
                "{ty} publishes a digest, so it must not carry a no-digest excuse"
            );
        }
    }
}
