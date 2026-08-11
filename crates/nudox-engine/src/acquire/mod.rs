//! Turning a [`Purl`] into a loaded package: resolve → fetch → verify →
//! extract → produce → insert.
//!
//! # The integrity guarantee, stated exactly
//!
//! `corpus/fetch.nu`'s header is the standard this module is held to:
//!
//! > A fetcher that silently accepts a hash mismatch converts a real failure
//! > into an apparent success, which is worse than not fetching at all.
//!
//! That script compares every download against a hash **written in
//! `corpus/manifest.toml` by a human who ran `hash-url` first**. A PURL typed
//! into a search box has no such hash and never can — the whole point is that
//! nobody provisioned it. So the guarantee is different, and it is stated in the
//! type rather than in prose: every acquisition returns an [`Integrity`], and
//! there are exactly two shapes it can take.
//!
//! * [`Integrity::RegistryDigest`] — the registry publishes a digest of these
//!   exact bytes on a *different* endpoint from the one that served them, we
//!   fetched it, recomputed the digest locally, and the two matched. A mismatch
//!   is [`IndexError::IntegrityMismatch`] and the bytes are discarded. This is
//!   what GLOBAL-IR-GRAPH §2 asks for — "verify the digest against the registry
//!   before generating IR" — and it holds for **crates.io, npm, PyPI and Maven
//!   Central**.
//! * [`Integrity::TransportOnly`] — **golang and nuget**. Neither registry
//!   publishes a digest on any endpoint this fetcher reads, so what we have is
//!   the bytes an HTTPS-authenticated host served and nothing else. The variant
//!   carries the SHA-256 we computed and the specific reason no comparison was
//!   possible, so a caller can pin it into `corpus/manifest.toml` and get the
//!   stronger guarantee for every later fetch.
//!
//! What this module does **not** claim, in either case: that the bytes match
//! any particular upstream source repository, that the registry has not been
//! compromised, or that a mutable registry serves the same bytes twice. PURL
//! names a *version*, not bytes (GLOBAL-IR-GRAPH §1), and no amount of fetching
//! changes that.
//!
//! Nothing here follows a caller-supplied URL. Every URL is derived from the
//! parsed PURL against a fixed, per-ecosystem host, and
//! [`crate::purl::PurlParseError::UnsupportedQualifier`] refuses the
//! `?repository_url=` qualifier that would otherwise redirect a fetch while
//! keeping the name.
//!
//! # Why Rust and not `nu corpus/fetch.nu`
//!
//! Recorded here because it is the decision most likely to be revisited.
//!
//! 1. **A runtime shell dependency is not payable.** `lindsey` is a desktop
//!    app and `nudox-mcp` is a server an agent talks to; neither can require
//!    Nushell on the user's machine. "Index this dependency" must not fail with
//!    "install `nu` first".
//! 2. **`fetch.nu` structurally cannot serve this path.** `fetch-one` takes a
//!    `ver_entry: record` whose `hash` field is required and whose mismatch is a
//!    hard failure. A PURL fetch has no manifest hash, so using the script would
//!    mean bypassing exactly the check that makes it trustworthy — leaving a
//!    fetcher with the script's costs and none of its guarantee.
//! 3. **A subprocess flattens the failure taxonomy.** The five distinct,
//!    actionable failures this feature owes a user ([`IndexError`]) do not
//!    survive an exit code and a stderr blob.
//! 4. **The duplication is bounded and tested.** What is genuinely shared is
//!    *URL resolution*, and `tests/purl_url_parity.rs` runs `fetch.nu`'s own
//!    `hash-url` against `corpus/manifest.toml` entries and asserts this module
//!    resolves the same URLs. `fetch.nu` remains the manifest-driven,
//!    hash-pinned corpus provisioner; this is the on-demand path. Drift is a
//!    test failure rather than a surprise.

mod archive;
mod registry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::purl::{Purl, PurlParseError, PurlType};
use crate::wire::{Gen, SharedStr, SymbolKey};
use crate::{PackageSpec, ProducerLanguage};

/// How many versions an "that version does not exist" error lists.
///
/// `serde` has over 300 published versions; printing all of them turns the one
/// actionable line into a wall. Newest first, so the truncation drops the
/// versions least likely to be wanted.
const MAX_LISTED_VERSIONS: usize = 20;

/// Upper bound on a downloaded artifact, in bytes.
///
/// Source artifacts are small — the largest in `corpus/manifest.toml` is a few
/// megabytes. 256 MiB is far above anything legitimate and exists so a
/// misresolved URL (or a hostile `Content-Length`) cannot exhaust memory before
/// the digest check ever runs.
const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Integrity
// ---------------------------------------------------------------------------

/// What is actually known about the bytes a package's IR was produced from.
///
/// Returned on every successful acquisition and carried on
/// [`IndexEvent::Indexed`], because a caller that cannot tell a verified fetch
/// from an unverified one will treat them the same, and then the distinction
/// exists only in this module's head.
///
/// `#[non_exhaustive]` — a third tier (a signed transparency-log proof for Go,
/// say) must not break `lindsey`'s match arms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Integrity {
    /// The registry published a digest of these exact bytes, and it matched.
    RegistryDigest {
        /// `"sha256"`, `"sha512"` or `"sha1"` — whichever the registry
        /// publishes. Recorded rather than normalized because a caller
        /// comparing our claim against the registry needs to know which
        /// endpoint to look at.
        algorithm: String,
        /// The digest, spelled the way the registry spells it (hex for
        /// crates.io/PyPI/Maven, base64 for npm's SRI).
        digest: String,
        /// The endpoint the digest came from, so the claim is auditable.
        published_by: String,
    },

    /// No digest was available to compare against.
    ///
    /// The bytes are what an HTTPS-authenticated registry host served. That is
    /// a real guarantee and a much weaker one than the variant above, and the
    /// two must never be rendered identically.
    TransportOnly {
        /// SHA-256 of the downloaded artifact, lowercase hex.
        ///
        /// Present so this fetch can be *promoted*: paste it into
        /// `corpus/manifest.toml` (`nu corpus/fetch.nu hash-url` prints the
        /// nix-base32 form of the same bytes) and every later fetch of this
        /// version is hash-pinned.
        sha256: String,
        /// Why no comparison was possible, specific to this registry.
        why: String,
    },
}

impl Integrity {
    /// A one-line rendering for a status bar or an agent's log.
    pub fn summary(&self) -> String {
        match self {
            Self::RegistryDigest {
                algorithm,
                published_by,
                ..
            } => format!("verified against the {algorithm} published by {published_by}"),
            Self::TransportOnly { sha256, .. } => format!(
                "not verified against any published digest (transport trust only); sha256 {}",
                &sha256[..sha256.len().min(16)],
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// IndexError
// ---------------------------------------------------------------------------

/// Every distinct way indexing a PURL can fail.
///
/// One variant per *distinct recoverable situation* (doctrine §3), because the
/// user's next action differs in every one: fix the spelling, pick a different
/// version, retry, install a toolchain, or give up on this package. A single
/// "could not index" would collapse all five into the same shrug.
///
/// `Serialize` so the whole structure reaches an MCP client in JSON-RPC
/// `error.data` rather than being re-parsed out of prose, and `Clone` because
/// it rides inside [`IndexEvent`], which several GUI stores observe at once —
/// the same reason `EngineError` is `Clone`.
#[derive(Clone, Debug, thiserror::Error, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum IndexError {
    /// The string is not a package URL this build can act on.
    #[error("{reason}")]
    MalformedPurl {
        /// The rejected input, echoed back so a caller can self-correct.
        input: String,
        /// The parser's own message.
        reason: String,
    },

    /// **Failure 1 of 5.** The registry has never heard of this name.
    #[error("{registry} has no package named {name:?} (looked it up at {url}, which returned 404)")]
    UnknownPackage {
        /// The PURL as canonicalized.
        purl: String,
        /// The name as the registry would have to spell it.
        name: String,
        /// Human name of the registry that was asked.
        registry: &'static str,
        /// The exact endpoint that 404'd, so the claim is checkable.
        url: String,
    },

    /// **Failure 2 of 5.** The package exists; this version of it does not.
    #[error(
        "{registry} publishes {published} version(s) of {purl}, and {requested:?} is not one of them; the newest are {}",
        preview(available),
    )]
    VersionNotFound {
        /// The PURL as canonicalized.
        purl: String,
        /// Human name of the registry that was asked.
        registry: &'static str,
        /// The version that was asked for.
        requested: String,
        /// How many versions the registry publishes in total.
        published: usize,
        /// The newest few, capped at [`MAX_LISTED_VERSIONS`].
        available: Vec<String>,
    },

    /// The PURL carried no `@version` at all.
    ///
    /// Distinct from [`Self::VersionNotFound`]: nothing was misspelled and
    /// nothing was tried. Kept separate so the message can say "add one" rather
    /// than "that one is wrong", while still carrying the same list.
    #[error(
        "{purl} names no version; {registry} publishes {published}, newest first: {}",
        preview(available),
    )]
    VersionMissing {
        /// The PURL as canonicalized.
        purl: String,
        /// Human name of the registry that was asked.
        registry: &'static str,
        /// How many versions the registry publishes in total.
        published: usize,
        /// The newest few, capped at [`MAX_LISTED_VERSIONS`].
        available: Vec<String>,
    },

    /// The version exists but ships no artifact a producer could read.
    ///
    /// Real and common on PyPI (wheel-only releases) and possible on Maven
    /// Central (no `-sources` classifier). Distinct from every other failure
    /// here because nothing is wrong with the request, the network, or this
    /// build — the publisher did not publish sources.
    #[error("{registry} has {purl} but no source artifact for it: {detail}")]
    NoSourceArtifact {
        /// The PURL as canonicalized.
        purl: String,
        /// Human name of the registry that was asked.
        registry: &'static str,
        /// What was missing.
        detail: String,
    },

    /// **Failure 3 of 5.** The network, or the registry, did not answer.
    #[error("could not reach {url}: {detail}")]
    Network {
        /// The endpoint that failed.
        url: String,
        /// The transport error, flattened.
        detail: String,
    },

    /// The bytes did not match the digest the registry published for them.
    ///
    /// The one failure this module refuses to degrade. The download is
    /// discarded, nothing is extracted, and no IR is produced — `fetch.nu`'s
    /// rule, transplanted.
    #[error(
        "{url} does not match the {algorithm} {registry} publishes for {purl}: expected {expected}, got {actual}"
    )]
    IntegrityMismatch {
        /// The PURL as canonicalized.
        purl: String,
        /// Human name of the registry that published the digest.
        registry: &'static str,
        /// The endpoint the bytes came from.
        url: String,
        /// Which digest algorithm disagreed.
        algorithm: String,
        /// What the registry says.
        expected: String,
        /// What the bytes actually hash to.
        actual: String,
    },

    /// The artifact downloaded but could not be unpacked.
    #[error("{purl} downloaded but could not be unpacked: {detail}")]
    Unpack {
        /// The PURL as canonicalized.
        purl: String,
        /// The extraction error, flattened.
        detail: String,
    },

    /// **Failure 4 of 5.** No producer for this language exists in this build.
    #[error(
        "{purl} was fetched, but this build has no {language} producer registered, so it cannot be turned into documentation"
    )]
    NoProducer {
        /// The PURL as canonicalized.
        purl: String,
        /// The language whose producer is absent.
        language: String,
        /// What has to be true for it to be present.
        blocker: String,
    },

    /// **Failure 5 of 5.** The producer ran and could not lower the package.
    #[error("{purl} was fetched and verified, but the {language} producer could not lower it: {detail}")]
    ProducerFailed {
        /// The PURL as canonicalized.
        purl: String,
        /// The language whose producer failed.
        language: String,
        /// The producer's own error, with its `#[source]` chain flattened.
        detail: String,
    },

    /// The caller dropped the [`crate::StreamHandle`] before the work finished.
    #[error("indexing {purl} was cancelled")]
    Cancelled {
        /// The PURL as canonicalized.
        purl: String,
    },

    /// The producer finished but emitted no `Ready` and no `Failed`.
    ///
    /// A broken invariant on our side of the `IrSource` seam, not a failure the
    /// source reported — the same distinction `McpError::TruncatedStream`
    /// draws.
    #[error("the load stream for {purl} ended without producing or failing the package")]
    TruncatedLoad {
        /// The PURL as canonicalized.
        purl: String,
    },
}

fn preview(available: &[String]) -> String {
    if available.is_empty() {
        return "none".to_owned();
    }
    available.join(", ")
}

impl IndexError {
    /// The stable machine tag, independent of the message's wording.
    ///
    /// Mirrors `McpError::kind` so the MCP layer can delegate instead of
    /// re-deriving a parallel taxonomy — the failure vocabulary is defined
    /// where the failures happen.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MalformedPurl { .. } => "malformed_purl",
            Self::UnknownPackage { .. } => "unknown_package",
            Self::VersionNotFound { .. } => "version_not_found",
            Self::VersionMissing { .. } => "version_missing",
            Self::NoSourceArtifact { .. } => "no_source_artifact",
            Self::Network { .. } => "registry_unreachable",
            Self::IntegrityMismatch { .. } => "integrity_mismatch",
            Self::Unpack { .. } => "unpack_failed",
            Self::NoProducer { .. } => "no_producer",
            Self::ProducerFailed { .. } => "producer_failed",
            Self::Cancelled { .. } => "cancelled",
            Self::TruncatedLoad { .. } => "truncated_load",
        }
    }

    /// "What to do next", never repeating what the message already said
    /// happened.
    ///
    /// The five headline failures each get a *different* answer, which is the
    /// entire justification for them being five variants instead of one.
    pub fn help(&self) -> Option<&'static str> {
        match self {
            Self::MalformedPurl { .. } => Some(
                "A package URL is `pkg:<type>/<namespace>/<name>@<version>`. The namespace is the \
                 groupId for maven, the @scope for npm, and the module path prefix for golang; \
                 cargo, pypi and nuget names have no namespace at all.",
            ),
            Self::UnknownPackage { .. } => Some(
                "Check the name against the registry itself. A package URL names the *registry's* \
                 package, not the import path or the repository — `pkg:maven/com.google.guava/guava`, \
                 not `pkg:maven/guava`; `pkg:golang/github.com/pkg/errors`, not `pkg:golang/errors`.",
            ),
            Self::VersionNotFound { .. } => Some(
                "Pick one of data.available. Version strings are the registry's own: Go modules keep \
                 their leading `v` (`v1.9.0`), Maven and NuGet do not, and PyPI pre-releases are \
                 spelled `1.0rc1` rather than `1.0-rc1`.",
            ),
            Self::VersionMissing { .. } => Some(
                "Append `@` and one of data.available. nudox does not pick a version for you: \
                 \"latest\" is a moving target, and a document set that silently changes which \
                 release it describes is worse than one that asks.",
            ),
            Self::NoSourceArtifact { .. } => Some(
                "Nothing is wrong with the request — this release ships no sources. Try an adjacent \
                 version from data.available; publishers often drop and restore source artifacts \
                 between releases.",
            ),
            Self::Network { .. } => Some(
                "This is a transport failure, not a missing package: the same package URL is worth \
                 retrying. nudox reaches only each ecosystem's canonical registry over HTTPS and \
                 follows no caller-supplied URL, so a proxy or an offline machine will fail every \
                 attempt identically.",
            ),
            Self::IntegrityMismatch { .. } => Some(
                "Do not retry expecting success. The registry served bytes that do not match the \
                 digest it publishes for them, which is either corruption in transit or a registry \
                 problem; nothing was extracted and no documentation was produced from them.",
            ),
            Self::Unpack { .. } => None,
            Self::NoProducer { .. } => Some(
                "The package is on disk and the fetch succeeded; only IR production is missing. \
                 This is a property of the build, not of the package — see data.blocker for what \
                 has to be enabled, and expect the same result for every package of this language.",
            ),
            Self::ProducerFailed { .. } => Some(
                "Retrying will not help: the bytes are already verified and cached, so a second \
                 attempt runs the same producer over the same sources. data.detail is the \
                 producer's own error chain and names the actual defect.",
            ),
            Self::Cancelled { .. } => None,
            Self::TruncatedLoad { .. } => None,
        }
    }

    /// True when the same call, issued again unchanged, could plausibly
    /// succeed.
    ///
    /// Exists so a caller does not have to encode that judgement by matching on
    /// variants it will have to revisit when a variant is added — the question
    /// "should I retry" has one answer per variant and it belongs next to the
    /// variants.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network { .. } | Self::Cancelled { .. } | Self::TruncatedLoad { .. } => true,
            Self::MalformedPurl { .. }
            | Self::UnknownPackage { .. }
            | Self::VersionNotFound { .. }
            | Self::VersionMissing { .. }
            | Self::NoSourceArtifact { .. }
            | Self::IntegrityMismatch { .. }
            | Self::Unpack { .. }
            | Self::NoProducer { .. }
            | Self::ProducerFailed { .. } => false,
        }
    }
}

impl From<PurlParseError> for IndexError {
    fn from(e: PurlParseError) -> Self {
        Self::MalformedPurl {
            input: match &e {
                PurlParseError::NotAPurl { input } | PurlParseError::EmptyName { input } => {
                    input.clone()
                }
                _ => String::new(),
            },
            reason: e.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// IndexStage / IndexEvent
// ---------------------------------------------------------------------------

/// Where a running index job has got to.
///
/// Coarse on purpose: these are the boundaries at which the *kind* of work
/// changes, so a stuck job's stage says which subsystem to look at. Finer
/// progress inside a stage would be honest only for `Downloading`, and the byte
/// counts on that variant already carry it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IndexStage {
    /// Asking the registry what exists and where it lives.
    Resolving,
    /// Downloading the source artifact.
    Downloading,
    /// Recomputing the digest and comparing it to the published one.
    Verifying,
    /// Unpacking the artifact into a package root.
    Extracting,
    /// Running the language producer. This is the slow one.
    Producing,
    /// The package root was already in the cache, verified, and is being
    /// re-produced without a download.
    Cached,
}

impl std::fmt::Display for IndexStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Resolving => "resolving",
            Self::Downloading => "downloading",
            Self::Verifying => "verifying",
            Self::Extracting => "extracting",
            Self::Producing => "producing",
            Self::Cached => "cached",
        })
    }
}

/// The stream [`crate::EngineHandle::index_purl`] emits.
///
/// # Why this is its own stream and not just `PackageLoadEvent`
///
/// They answer different questions and have different lifetimes.
/// `PackageLoadEvent` describes *corpus membership* — it is a one-row-per-package
/// stream every subscriber shares, and a package appears on it exactly once,
/// when it becomes searchable. `IndexEvent` describes *one job*, belongs to the
/// caller who started it, and is where a fetch that never becomes a package
/// (a 404, a digest mismatch) has to be reported. Folding the second into the
/// first would put per-caller failures on a broadcast channel and give the
/// package list rows for packages that do not exist.
///
/// A successful index emits on **both**: `IndexEvent::Indexed` to the caller,
/// and `PackageLoadEvent::Loaded` to every `packages()` subscriber, so the GUI's
/// package list updates without knowing an index job ran.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum IndexEvent {
    /// Exactly once, first. Carries the canonicalized PURL, which may differ
    /// from what the caller typed.
    Started {
        /// The canonical PURL this job is for.
        purl: SharedStr,
        /// The generation this stream answers.
        generation: Gen,
    },

    /// The job moved to a new stage.
    Stage {
        /// The stage now running.
        stage: IndexStage,
        /// Bytes received so far, on [`IndexStage::Downloading`] only.
        received: u64,
        /// Total bytes expected, when the registry sent a `Content-Length`.
        total: Option<u64>,
        /// The generation this stream answers.
        generation: Gen,
    },

    /// Terminal, success. The package is in the corpus and searchable.
    Indexed {
        /// The canonical PURL.
        purl: SharedStr,
        /// The package's registry name — the lineage's `name`.
        name: SharedStr,
        /// The ecosystem id (`"cargo"`, `"go"`, …).
        ecosystem: SharedStr,
        /// The version that was indexed.
        version: SharedStr,
        /// Public API symbols in the produced IR.
        symbol_count: u64,
        /// The package's root entry, to navigate to immediately.
        root: Option<SymbolKey>,
        /// What is known about the bytes this IR was produced from.
        integrity: Integrity,
        /// The generation this stream answers.
        generation: Gen,
    },

    /// Terminal, failure.
    Failed {
        /// Why.
        error: IndexError,
        /// The generation this stream answers.
        generation: Gen,
    },
}

// ---------------------------------------------------------------------------
// Acquired
// ---------------------------------------------------------------------------

/// A package that is on disk and ready to be produced.
#[derive(Clone, Debug)]
pub(crate) struct Acquired {
    /// The PURL, with its version resolved.
    pub(crate) purl: Purl,
    /// The unpacked package root — a `PackageSpec::root`.
    pub(crate) root: PathBuf,
    /// What is known about the bytes under `root`.
    pub(crate) integrity: Integrity,
}

impl Acquired {
    /// The existing seam type. Everything downstream of here is the load path
    /// that `Engine::start_with_producer` already uses — a PURL adds a fetch
    /// step in front of it and nothing else.
    pub(crate) fn spec(&self) -> PackageSpec {
        PackageSpec {
            root: self.root.clone(),
            name: self.purl.lineage_name(),
            version: self.purl.version().unwrap_or_default().to_owned(),
            language: self.purl.language(),
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// The thin HTTP layer. Every registry probe goes through here so that "the
/// registry said 404" is decided in exactly one place.
pub(crate) mod http {
    use super::{IndexError, MAX_ARTIFACT_BYTES};
    use crate::purl::Purl;

    /// A shared client. Built once per engine: `reqwest::Client` owns a
    /// connection pool, and building one per request would open a fresh TLS
    /// session for every probe in a six-request resolution.
    pub(crate) fn client() -> reqwest::Client {
        reqwest::Client::builder()
            // Registries redirect (PyPI files, npm tarballs on the CDN); a
            // bounded follow is required for the fetch to work at all. The
            // bound exists so a redirect loop fails as a redirect loop.
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent(concat!("nudox/", env!("CARGO_PKG_VERSION")))
            .build()
            // A `Client` fails to build only if the TLS backend cannot be
            // initialised, which is a process-fatal environment problem rather
            // than a per-request condition — the same judgement
            // `Engine::start` makes about the Tokio runtime.
            .expect("reqwest client construction requires only a working TLS backend")
    }

    fn transport(url: &str, e: reqwest::Error) -> IndexError {
        IndexError::Network {
            url: url.to_owned(),
            detail: std::iter::successors(Some(&e as &dyn std::error::Error), |e| {
                std::error::Error::source(*e)
            })
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(": "),
        }
    }

    /// GET, mapping 404 to [`IndexError::UnknownPackage`] for `purl`.
    async fn get(
        client: &reqwest::Client,
        url: &str,
        purl: &Purl,
    ) -> Result<reqwest::Response, IndexError> {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| transport(url, e))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            return Err(IndexError::UnknownPackage {
                purl: purl.render(),
                name: purl.lineage_name(),
                registry: purl.ty().registry(),
                url: url.to_owned(),
            });
        }
        if !status.is_success() {
            return Err(IndexError::Network {
                url: url.to_owned(),
                detail: format!("registry answered {status}"),
            });
        }
        Ok(response)
    }

    pub(crate) async fn get_text(
        client: &reqwest::Client,
        url: &str,
        purl: &Purl,
    ) -> Result<String, IndexError> {
        get(client, url, purl)
            .await?
            .text()
            .await
            .map_err(|e| transport(url, e))
    }

    pub(crate) async fn get_json(
        client: &reqwest::Client,
        url: &str,
        purl: &Purl,
    ) -> Result<serde_json::Value, IndexError> {
        let body = get_text(client, url, purl).await?;
        serde_json::from_str(&body).map_err(|e| IndexError::Network {
            url: url.to_owned(),
            detail: format!("registry returned a body that is not JSON: {e}"),
        })
    }

    /// GET a digest sidecar, treating *any* failure as "no digest published".
    ///
    /// Deliberately swallowing: a missing `.sha1` degrades the result to
    /// `Integrity::TransportOnly`, which is a weaker claim the caller can see,
    /// not a silent upgrade. Failing the whole fetch because an optional
    /// sidecar 404'd would be the opposite mistake.
    pub(crate) async fn get_text_optional(client: &reqwest::Client, url: &str) -> Option<String> {
        let response = client.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    }

    /// Download an artifact, reporting progress as bytes arrive.
    pub(crate) async fn get_artifact(
        client: &reqwest::Client,
        url: &str,
        purl: &Purl,
        on_progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<Vec<u8>, IndexError> {
        use futures::StreamExt as _;

        let response = get(client, url, purl).await?;
        let total = response.content_length();
        if let Some(total) = total {
            if total > MAX_ARTIFACT_BYTES {
                return Err(IndexError::Network {
                    url: url.to_owned(),
                    detail: format!(
                        "the registry advertises {total} bytes, above the {MAX_ARTIFACT_BYTES}-byte \
                         ceiling for a source artifact"
                    ),
                });
            }
        }

        let mut body = Vec::with_capacity(total.unwrap_or(64 * 1024) as usize);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| transport(url, e))?;
            body.extend_from_slice(&chunk);
            if body.len() as u64 > MAX_ARTIFACT_BYTES {
                return Err(IndexError::Network {
                    url: url.to_owned(),
                    detail: format!(
                        "the response exceeded the {MAX_ARTIFACT_BYTES}-byte ceiling for a source \
                         artifact without advertising its length"
                    ),
                });
            }
            on_progress(body.len() as u64, total);
        }
        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Where fetched packages are unpacked.
///
/// # Why the extracted tree is cached and the archive is not
///
/// The producer reads a directory, so the directory is what a second index of
/// the same PURL can skip. The archive is worth nothing once unpacked — except
/// for its digest, which is why an `Integrity` sidecar is written beside the
/// root. A cache hit re-reads that sidecar and reports *the integrity of the
/// fetch that created the tree*; if the sidecar is missing or unreadable the
/// entry is treated as absent and re-fetched, because reporting a verified
/// fetch we cannot substantiate is exactly the "apparent success" this module
/// exists to avoid.
pub(crate) fn default_cache_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os("NUDOX_PACKAGE_CACHE") {
        return PathBuf::from(explicit);
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("nudox").join("packages");
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(local).join("nudox").join("packages");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home)
            .join(".cache")
            .join("nudox")
            .join("packages");
    }
    std::env::temp_dir().join("nudox-packages")
}

fn cache_paths(cache: &Path, purl: &Purl) -> (PathBuf, PathBuf) {
    let dir = cache.join(purl.ecosystem());
    let root = dir.join(purl.cache_dir_name());
    let sidecar = dir.join(format!("{}.integrity.json", purl.cache_dir_name()));
    (root, sidecar)
}

// ---------------------------------------------------------------------------
// acquire
// ---------------------------------------------------------------------------

/// Resolve, fetch, verify and unpack one PURL into a package root.
///
/// Everything before the producer. `report` is called once per stage change.
pub(crate) async fn acquire(
    client: &reqwest::Client,
    purl: &Purl,
    cache: &Path,
    report: &(dyn Fn(IndexStage, u64, Option<u64>) + Send + Sync),
) -> Result<Acquired, IndexError> {
    report(IndexStage::Resolving, 0, None);

    // A versionless PURL is answered with the list, not with a guess. See
    // `IndexError::VersionMissing`'s help for why nudox does not pick.
    if purl.version().is_none() {
        let available = registry::versions(client, purl).await?;
        return Err(IndexError::VersionMissing {
            purl: purl.render(),
            registry: purl.ty().registry(),
            published: available.len(),
            available: available.into_iter().take(MAX_LISTED_VERSIONS).collect(),
        });
    }

    let (root, sidecar) = cache_paths(cache, purl);
    if root.is_dir() {
        // The sidecar is the only thing that makes a cache hit reportable. A
        // tree with no recorded integrity is indistinguishable from one an
        // unrelated process left behind.
        if let Some(integrity) = std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|body| serde_json::from_str::<Integrity>(&body).ok())
        {
            report(IndexStage::Cached, 0, None);
            return Ok(Acquired {
                purl: purl.clone(),
                root,
                integrity,
            });
        }
        // Unattributable tree: remove it and fetch again rather than produce IR
        // from bytes whose provenance we cannot state.
        let _ = std::fs::remove_dir_all(&root);
    }

    let artifact = registry::artifact(client, purl).await?;

    report(IndexStage::Downloading, 0, None);
    let bytes = http::get_artifact(client, &artifact.url, purl, &|received, total| {
        report(IndexStage::Downloading, received, total);
    })
    .await?;

    report(IndexStage::Verifying, bytes.len() as u64, None);
    let integrity = match &artifact.expected {
        Some(expected) => {
            let actual = expected.compute_over(&bytes);
            if !actual.eq_ignore_ascii_case(expected.value()) {
                return Err(IndexError::IntegrityMismatch {
                    purl: purl.render(),
                    registry: purl.ty().registry(),
                    url: artifact.url.clone(),
                    algorithm: expected.algorithm().to_owned(),
                    expected: expected.value().to_owned(),
                    actual,
                });
            }
            Integrity::RegistryDigest {
                algorithm: expected.algorithm().to_owned(),
                digest: expected.value().to_owned(),
                published_by: digest_endpoint(purl.ty()).to_owned(),
            }
        }
        None => {
            use sha2::Digest as _;
            Integrity::TransportOnly {
                sha256: registry::hex(&sha2::Sha256::digest(&bytes)),
                why: registry::no_digest_reason(purl.ty())
                    .expect(
                        "an ecosystem with no published digest must declare why; \
                         `registry::tests::exactly_the_two_registries_without_a_reachable_digest_say_why` \
                         pins that the two lists agree",
                    )
                    .to_owned(),
            }
        }
    };

    report(IndexStage::Extracting, bytes.len() as u64, None);
    let scratch = root.with_extension("partial");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| IndexError::Unpack {
        purl: purl.render(),
        detail: format!("creating {}: {e}", scratch.display()),
    })?;

    let unpacked = archive::unpack(&bytes, purl.ty(), &scratch, &root);
    let _ = std::fs::remove_dir_all(&scratch);
    unpacked.map_err(|e| IndexError::Unpack {
        purl: purl.render(),
        detail: e.to_string(),
    })?;

    // A Go module zip is not guaranteed to contain its own `go.mod` — see
    // `registry::go_mod`, and `github.com/pkg/errors@v0.9.1`, which does not.
    // The producer needs one, so fetch the manifest the proxy serves beside
    // the zip, exactly as `go mod download` populates the module cache.
    if purl.ty() == PurlType::Golang {
        let manifest = root.join("go.mod");
        if !manifest.is_file() {
            let body = registry::go_mod(client, purl).await?;
            std::fs::write(&manifest, body).map_err(|e| IndexError::Unpack {
                purl: purl.render(),
                detail: format!(
                    "the module zip carries no go.mod and the one the proxy serves could not be \
                     written to {}: {e}",
                    manifest.display()
                ),
            })?;
        }
    }

    // Mirrors `append-cargo-workspace` in `corpus/fetch.nu`: a fetched crate
    // that lands inside some ancestor cargo workspace would otherwise be
    // auto-promoted into it, and rust-analyzer would then load the *host*
    // workspace instead of this package.
    append_cargo_workspace(&root);

    if let Ok(json) = serde_json::to_string(&integrity) {
        let _ = std::fs::write(&sidecar, json);
    }

    Ok(Acquired {
        purl: purl.clone(),
        root,
        integrity,
    })
}

/// The endpoint a published digest came from, for [`Integrity::RegistryDigest`].
fn digest_endpoint(ty: PurlType) -> &'static str {
    match ty {
        PurlType::Cargo => "the crates.io sparse index (`cksum`)",
        PurlType::Npm => "the npm packument (`dist.integrity`)",
        PurlType::PyPi => "the PyPI JSON API (`digests.sha256`)",
        PurlType::Maven => "the Maven Central `.sha1` sidecar",
        // Unreachable: these two produce `expected: None` and therefore
        // `TransportOnly`. Named rather than `_` so adding an ecosystem lands
        // here as a compile error.
        PurlType::Golang => "the Go module proxy",
        PurlType::NuGet => "nuget.org",
    }
}

fn append_cargo_workspace(root: &Path) {
    use std::io::Write as _;
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return;
    }
    if let Ok(existing) = std::fs::read_to_string(&manifest) {
        if existing.contains("[workspace]") {
            return;
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&manifest) {
        // Best-effort: a package whose manifest we could not append to still
        // loads, it just risks being read as a workspace member. Logged rather
        // than failed, matching `fetch.nu`'s `try { … } catch { print WARN }`.
        if writeln!(file, "\n[workspace]").is_err() {
            tracing::warn!(manifest = %manifest.display(), "could not append [workspace]");
        }
    }
}

// ---------------------------------------------------------------------------
// EngineHandle::index_purl
// ---------------------------------------------------------------------------

impl crate::EngineHandle {
    /// Fetch, produce and insert a package named by a PURL, into the *running*
    /// corpus.
    ///
    /// Returns immediately with a stream of [`IndexEvent`]. Dropping the
    /// [`crate::StreamHandle`] cancels the job — including a download in
    /// flight, because every await point between here and the producer is
    /// inside the same `select!`.
    ///
    /// # Why this closes `EngineCapability::ProjectResolution`'s blocker
    ///
    /// That capability's `blocked_on` says project resolution needs "an
    /// `EngineHandle` method that can add a discovered package to a running
    /// corpus; today packages can only be supplied to
    /// `Engine::start_with_producer`, so a resolved list could not be acted
    /// on". This is that method. It is deliberately built on the same
    /// `ProducerSource` → `VersionRegistry::record` → `Corpus::insert` path the
    /// seeding task uses, extracted into
    /// [`crate::runtime::drive_load`](crate::runtime) rather than duplicated, so
    /// a package that arrives at second 300 is indistinguishable from one that
    /// arrived at second 0 — same lineage rules, same version ordering, same
    /// `packages()` broadcast.
    ///
    /// # Cost
    ///
    /// Seconds to minutes. Nothing about this call blocks the caller's thread:
    /// the whole pipeline runs on the engine's Tokio runtime and the producer
    /// itself on its blocking pool, exactly as a start-up load does.
    pub fn index_purl(
        &self,
        purl: Purl,
        generation: Gen,
    ) -> (crate::StreamHandle, flume::Receiver<IndexEvent>) {
        // Capacity 32, matching Appendix C's "Package" row: an index job is a
        // package load with a fetch in front of it, and its consumer is the
        // same GUI render loop.
        let (tx, rx) = flume::bounded::<IndexEvent>(32);
        let (cancel, cancel_fn) = Self::make_cancel();
        let handle = crate::StreamHandle::new(generation, cancel_fn);

        let engine = self.clone();
        self.spawn(async move {
            let rendered: SharedStr = purl.render().into();
            if tx
                .send_async(IndexEvent::Started {
                    purl: rendered.clone(),
                    generation,
                })
                .await
                .is_err()
            {
                return;
            }

            let job = engine.run_index_job(&purl, generation, &tx);
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => IndexEvent::Failed {
                    error: IndexError::Cancelled { purl: purl.render() },
                    generation,
                },
                outcome = job => match outcome {
                    Ok(event) => event,
                    Err(error) => IndexEvent::Failed { error, generation },
                },
            };
            let _ = tx.send_async(event).await;
        });

        (handle, rx)
    }

    /// The body of one index job: acquire, then produce, then insert.
    async fn run_index_job(
        &self,
        purl: &Purl,
        generation: Gen,
        tx: &flume::Sender<IndexEvent>,
    ) -> Result<IndexEvent, IndexError> {
        let report = {
            let tx = tx.clone();
            move |stage: IndexStage, received: u64, total: Option<u64>| {
                // `try_send` rather than an await: a progress update that
                // cannot be delivered because the consumer is behind must not
                // slow the download down. The terminal events above are sent
                // with backpressure, so nothing that matters is dropped.
                let _ = tx.try_send(IndexEvent::Stage {
                    stage,
                    received,
                    total,
                    generation,
                });
            }
        };

        let acquired = acquire(
            self.http_client(),
            purl,
            self.package_cache(),
            &report,
        )
        .await?;

        report(IndexStage::Producing, 0, None);
        let loaded = self.load_one(acquired.spec()).await;

        match loaded {
            Ok(package) => Ok(IndexEvent::Indexed {
                purl: acquired.purl.render().into(),
                name: package.name,
                ecosystem: package.ecosystem,
                version: package.version,
                symbol_count: package.symbol_count,
                root: package.root,
                integrity: acquired.integrity,
                generation,
            }),
            Err(failure) => Err(classify_load_failure(purl, failure)),
        }
    }
}

/// Turn the store's flattened failure text back into a typed distinction
/// between "no producer exists" and "the producer failed".
///
/// # Why this is string matching, and why that is the honest option here
///
/// `SourceError` is a typed enum with exactly the variants needed
/// (`ToolchainMissing` vs `OracleFailed`/`LoweringFailed`), but it does not
/// reach this crate: `LoadEvent::Failed` is flattened to a `SharedStr` at the
/// `nudox-engine` boundary, and `PackageLoadEvent::LoadFailed` — the type
/// `lindsey` sees — carries only that string. Widening the seam to carry the
/// typed variant is the right fix and it is a `nudox-store` **and** wire-protocol
/// change; the discrimination is recorded here, in one place, with the prefix it
/// depends on named, so that when the seam is widened this function deletes
/// rather than moves.
///
/// The prefix is `SourceError::ToolchainMissing`'s own `#[error]` format string,
/// `"toolchain missing for {language} (package {package})"`, and
/// `nudox_store::source::SourceError` is `#[non_exhaustive]` so a new variant
/// lands in the `else` branch — reported as a producer failure with its own
/// message intact, never as a silent success.
fn classify_load_failure(purl: &Purl, failure: SharedStr) -> IndexError {
    let language = format!("{:?}", purl.language());
    if failure.starts_with("toolchain missing") {
        IndexError::NoProducer {
            purl: purl.render(),
            language,
            blocker: blocker_for(purl.language()).to_owned(),
        }
    } else {
        IndexError::ProducerFailed {
            purl: purl.render(),
            language,
            detail: failure.to_string(),
        }
    }
}

/// What has to be true for a language's producer to be registered.
///
/// Derived from `ProducerRegistry::with_all_available`, which registers six
/// languages unconditionally and Python only behind `nudox-store`'s `pyrefly`
/// feature. Six of these are therefore "the toolchain is missing on this
/// machine" and one is "this build was compiled without it" — a distinction the
/// user can act on and `SourceError::ToolchainMissing` alone cannot make.
fn blocker_for(language: ProducerLanguage) -> &'static str {
    match language {
        ProducerLanguage::Python => {
            "nudox-store's `pyrefly` feature, which this build does not enable. Without it the \
             Python producer is deliberately not registered, because it would otherwise return an \
             empty oracle — making \"we cannot document this\" indistinguishable from \"this \
             package has no public API\" (LIMITATIONS.md L2)."
        }
        ProducerLanguage::Rust => "the Rust producer, which is registered unconditionally — its \
             absence means the registry was built with `with_rust_pilot` or a custom set",
        ProducerLanguage::Go => "a Go toolchain reachable by the vendored Go oracle subprocess",
        ProducerLanguage::Java => "a JDK reachable by the Java oracle subprocess (javac/javadoc)",
        ProducerLanguage::CSharp => "a .NET SDK reachable as `dotnet`, for the Roslyn oracle",
        ProducerLanguage::TypeScript => "the OXC-based TypeScript producer, which is in-process \
             and registered unconditionally",
        ProducerLanguage::Cpp => "libclang, loaded at runtime via dlopen",
    }
}

/// One produced package, as far as this module needs it.
pub(crate) struct LoadedPackage {
    pub(crate) name: SharedStr,
    pub(crate) ecosystem: SharedStr,
    pub(crate) version: SharedStr,
    pub(crate) symbol_count: u64,
    pub(crate) root: Option<SymbolKey>,
}

/// The URL this crate would download for `purl`, without downloading it.
///
/// Exists solely for `tests/purl_url_parity.rs`, which compares it against what
/// `corpus/fetch.nu`'s `resolve-url` produces for the same package. It is `pub`
/// and named `_for_test` rather than being reached through `#[cfg(test)]`
/// because an *integration* test lives in a separate crate and cannot see this
/// module's internals — and because the alternative, re-deriving the URLs in
/// the test, would have compared the test author's understanding of the
/// conventions against `fetch.nu` instead of comparing the shipping code.
pub async fn resolved_url_for_test(
    client: &reqwest::Client,
    purl: &Purl,
) -> Result<String, IndexError> {
    registry::artifact(client, purl).await.map(|a| a.url)
}

/// Shared `Arc` so `EngineInner` can hold one client and one cache path.
pub(crate) struct AcquireContext {
    pub(crate) client: reqwest::Client,
    pub(crate) cache: PathBuf,
}

impl AcquireContext {
    pub(crate) fn new(cache: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            client: http::client(),
            cache: cache.unwrap_or_else(default_cache_dir),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The five headline failures must each say something *different* about
    /// what to do next. A shared help string would be doctrine §3's `"failed"`
    /// message wearing five hats.
    #[test]
    fn the_five_failures_give_five_different_next_actions() {
        let cases = [
            IndexError::UnknownPackage {
                purl: "pkg:cargo/serd@1.0.0".to_owned(),
                name: "serd".to_owned(),
                registry: "crates.io",
                url: "https://index.crates.io/se/rd/serd".to_owned(),
            },
            IndexError::VersionNotFound {
                purl: "pkg:cargo/serde@9.9.9".to_owned(),
                registry: "crates.io",
                requested: "9.9.9".to_owned(),
                published: 300,
                available: vec!["1.0.219".to_owned()],
            },
            IndexError::Network {
                url: "https://static.crates.io/x".to_owned(),
                detail: "connection timed out".to_owned(),
            },
            IndexError::ProducerFailed {
                purl: "pkg:cargo/serde@1.0.196".to_owned(),
                language: "Rust".to_owned(),
                detail: "lowering failed".to_owned(),
            },
            IndexError::NoProducer {
                purl: "pkg:pypi/attrs@23.2.0".to_owned(),
                language: "Python".to_owned(),
                blocker: "the pyrefly feature".to_owned(),
            },
        ];

        let mut kinds = Vec::new();
        let mut helps = Vec::new();
        for case in &cases {
            let kind = case.kind();
            assert!(!kinds.contains(&kind), "{kind} is reused");
            kinds.push(kind);
            let help = case.help().unwrap_or_else(|| {
                panic!("{kind} is one of the five headline failures and must say what to do next")
            });
            assert!(!helps.contains(&help), "{kind} reuses another failure's help");
            helps.push(help);
            // And the message must not merely restate the help.
            assert_ne!(case.to_string(), help);
        }
    }

    #[test]
    fn a_version_that_does_not_exist_names_the_ones_that_do() {
        let err = IndexError::VersionNotFound {
            purl: "pkg:cargo/serde@9.9.9".to_owned(),
            registry: "crates.io",
            requested: "9.9.9".to_owned(),
            published: 312,
            available: vec!["1.0.219".to_owned(), "1.0.218".to_owned()],
        };
        let text = err.to_string();
        assert!(text.contains("312"), "{text}");
        assert!(text.contains("1.0.219"), "{text}");
        assert!(text.contains("\"9.9.9\""), "{text}");
    }

    #[test]
    fn a_transport_failure_is_the_only_headline_failure_worth_retrying() {
        assert!(
            IndexError::Network {
                url: String::new(),
                detail: String::new()
            }
            .is_transient()
        );
        for permanent in [
            IndexError::UnknownPackage {
                purl: String::new(),
                name: String::new(),
                registry: "crates.io",
                url: String::new(),
            },
            IndexError::VersionNotFound {
                purl: String::new(),
                registry: "crates.io",
                requested: String::new(),
                published: 0,
                available: Vec::new(),
            },
            IndexError::ProducerFailed {
                purl: String::new(),
                language: String::new(),
                detail: String::new(),
            },
            IndexError::NoProducer {
                purl: String::new(),
                language: String::new(),
                blocker: String::new(),
            },
            IndexError::IntegrityMismatch {
                purl: String::new(),
                registry: "crates.io",
                url: String::new(),
                algorithm: String::new(),
                expected: String::new(),
                actual: String::new(),
            },
        ] {
            assert!(
                !permanent.is_transient(),
                "{} must not advertise itself as retryable",
                permanent.kind()
            );
        }
    }

    /// `Integrity`'s two variants must not render the same way — a caller that
    /// prints them identically has silently upgraded an unverified fetch.
    #[test]
    fn a_verified_and_an_unverified_fetch_do_not_read_alike() {
        let verified = Integrity::RegistryDigest {
            algorithm: "sha256".to_owned(),
            digest: "ab".repeat(32),
            published_by: "the crates.io sparse index (`cksum`)".to_owned(),
        };
        let unverified = Integrity::TransportOnly {
            sha256: "cd".repeat(32),
            why: "the Go module proxy publishes no digest".to_owned(),
        };
        assert!(verified.summary().contains("verified against"));
        assert!(unverified.summary().contains("not verified"));
        assert_ne!(verified.summary(), unverified.summary());
    }

    /// The sidecar has to survive a round trip or every cache hit re-fetches.
    #[test]
    fn an_integrity_record_round_trips_through_its_cache_sidecar() {
        for original in [
            Integrity::RegistryDigest {
                algorithm: "sha512".to_owned(),
                digest: "Zm9v".to_owned(),
                published_by: "the npm packument (`dist.integrity`)".to_owned(),
            },
            Integrity::TransportOnly {
                sha256: "00".repeat(32),
                why: "nuget.org's flat container serves no digest sibling".to_owned(),
            },
        ] {
            let json = serde_json::to_string(&original).expect("Integrity serializes");
            let back: Integrity = serde_json::from_str(&json).expect("Integrity deserializes");
            assert_eq!(original, back);
        }
    }

    #[test]
    fn a_toolchain_missing_load_failure_is_reported_as_a_build_gap_not_a_lowering_bug() {
        // The distinction a user acts on: "install something" vs "this package
        // does not lower". Both arrive as one flattened string today.
        let purl = Purl::parse("pkg:pypi/attrs@23.2.0").expect("valid purl");
        let err = classify_load_failure(
            &purl,
            "toolchain missing for Python (package pypi:attrs)".into(),
        );
        assert_eq!(err.kind(), "no_producer");
        assert!(err.to_string().contains("no Python producer"), "{err}");

        let err = classify_load_failure(
            &purl,
            "lowering failed for pypi:attrs: duplicate declaration id".into(),
        );
        assert_eq!(err.kind(), "producer_failed");
        assert!(err.to_string().contains("duplicate declaration id"), "{err}");
    }

    /// Doctrine §2 applied to the `_` arm: an unrecognised `SourceError`
    /// rendering must surface as a producer failure carrying its own text, not
    /// be swallowed.
    #[test]
    fn an_unrecognised_load_failure_keeps_its_message() {
        let purl = Purl::parse("pkg:cargo/serde@1.0.196").expect("valid purl");
        let err = classify_load_failure(&purl, "internal source error: something new".into());
        assert_eq!(err.kind(), "producer_failed");
        assert!(err.to_string().contains("something new"), "{err}");
    }

    #[test]
    fn every_purl_type_has_a_named_digest_endpoint_or_a_named_reason_but_never_both() {
        for ty in PurlType::ALL {
            let has_reason = registry::no_digest_reason(ty).is_some();
            let endpoint = digest_endpoint(ty);
            assert!(!endpoint.is_empty(), "{ty}");
            if has_reason {
                // The endpoint string exists only to keep the match total; it
                // is never reached for these two.
                continue;
            }
            assert!(
                endpoint.contains('`') || endpoint.contains("sidecar"),
                "{ty}'s digest endpoint must name the field or file it comes from, got {endpoint:?}"
            );
        }
    }

    #[test]
    fn the_cache_root_is_overridable_for_tests_and_sandboxes() {
        // `NUDOX_PACKAGE_CACHE` wins over everything, which is what lets an
        // integration test fetch into a scratch directory without touching the
        // developer's real cache.
        // SAFETY-style note: this test does not run concurrently with anything
        // that reads the variable, because `default_cache_dir` is called once
        // per engine and no engine is started here.
        unsafe { std::env::set_var("NUDOX_PACKAGE_CACHE", "/tmp/nudox-cache-probe") };
        assert_eq!(default_cache_dir(), PathBuf::from("/tmp/nudox-cache-probe"));
        unsafe { std::env::remove_var("NUDOX_PACKAGE_CACHE") };
    }
}
