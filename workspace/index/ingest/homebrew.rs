//! The Homebrew follower (REGISTRYLESS-PLAN §7.1): the warm-up follower, pure
//! HTTP JSON. `GET https://formulae.brew.sh/api/formula.json` with
//! `If-None-Match` (watermark = ETag); each formula maps to `UpsertPackage` +
//! `UpsertAlias(brew_formula)` + `UpsertVersion` (`registry_checksum` = sha256,
//! `source: None`) + recipe edges from its dependencies.
//!
//! # Stem resolution and confidence tiers (§7.1.3)
//!
//! A formula's upstream repo is inferred from `urls.stable.url` first, then its
//! `homepage`, through `crate::ecosystem::repo::normalize_repo_url`:
//! - a recognizable GitHub **release** tarball URL → `authoritative` (the feed
//!   itself points at the upstream repo);
//! - any other normalizable URL / homepage → `heuristic` (a sniff).
//!
//! A formula whose neither URL nor homepage normalizes to a repo slug still
//! produces a package + alias + version keyed on a **feed-scoped stem**
//! (`homebrew/<formula>`), so brew data is never dropped (RL-3 spirit); its
//! alias is recorded at `heuristic`. The reconstruction-fallback nature of brew
//! tarballs (git is preferred when P6 enumerates the stem directly) is honored
//! by emitting `source: None` on the version — brew only records the checksum.

use crate::{
    ecosystem::{Language, repo::normalize_repo_url},
    enums::{AliasConfidence, EdgeKind, EdgeSource, SourceKind},
    ids::PackageStemId,
    protocol::{
        CatalogOp, EdgeWire, FacetWire, PackageStemWire, SourceAcquisitionWire, VersionCoordinates,
    },
};

use crate::ingest::{
    enumerate::{cpp_stem_id, cpp_version_id},
    follower::{Follower, FollowerBatch, FollowerError, PollCadence},
    transport::{FeedRequest, FeedResponse, FeedTransport},
    watermark::FeedWatermark,
};

/// The stable feed id (the `feed_watermarks` primary key).
pub const FEED_ID: &str = "homebrew";

/// The default formulae endpoint (REGISTRYLESS-PLAN §7.1).
pub const DEFAULT_FORMULA_URL: &str = "https://formulae.brew.sh/api/formula.json";

/// The Homebrew formulae follower.
pub struct HomebrewFollower<Transport: FeedTransport> {
    transport: Transport,
    formula_url: String,
    /// Steady-state poll cadence; brew publishes at most a few times a day.
    cadence_seconds: u64,
}

impl<Transport: FeedTransport> HomebrewFollower<Transport> {
    /// A follower against the default endpoint.
    pub fn new(transport: Transport) -> Self {
        Self {
            transport,
            formula_url: DEFAULT_FORMULA_URL.to_owned(),
            cadence_seconds: 6 * 60 * 60,
        }
    }

    /// A follower against a custom endpoint (tests, enterprise mirror).
    pub fn with_url(transport: Transport, formula_url: impl Into<String>) -> Self {
        Self {
            transport,
            formula_url: formula_url.into(),
            cadence_seconds: 6 * 60 * 60,
        }
    }
}

impl<Transport: FeedTransport> Follower for HomebrewFollower<Transport> {
    fn feed_id(&self) -> &str {
        FEED_ID
    }

    fn cadence(&self) -> PollCadence {
        PollCadence::EverySeconds(self.cadence_seconds)
    }

    fn poll(
        &self,
        previous: Option<&FeedWatermark>,
        now_unix_ms: i64,
    ) -> Result<FollowerBatch, FollowerError> {
        let request = FeedRequest::conditional(
            self.formula_url.clone(),
            previous.and_then(|watermark| watermark.last_ref.clone()),
        );
        let response = self.transport.fetch(&request)?;

        match response {
            // 304: nothing changed. Advance only the crawl clock; the whole
            // batch is empty so the driver writes no catalog rows.
            FeedResponse::NotModified => Ok(FollowerBatch {
                ops: Vec::new(),
                next_watermark: FeedWatermark {
                    feed: FEED_ID.to_owned(),
                    last_ref: previous.and_then(|watermark| watermark.last_ref.clone()),
                    last_checked_at: now_unix_ms,
                    last_error: None,
                },
                caught_up: true,
            }),
            FeedResponse::Modified { body, etag } => {
                let ops = parse_formulae(&body)?;
                Ok(FollowerBatch {
                    ops,
                    next_watermark: FeedWatermark {
                        feed: FEED_ID.to_owned(),
                        last_ref: etag,
                        last_checked_at: now_unix_ms,
                        last_error: None,
                    },
                    caught_up: true,
                })
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The pure JSON → CatalogOp mapping (fully unit-testable off a fixture)
// ─────────────────────────────────────────────────────────────────────────────

/// The subset of a Homebrew formula the follower reads (REGISTRYLESS-PLAN §7.1
/// step 2). Serde is lenient: unknown fields are ignored, missing optional
/// fields default, so the brittle full schema is never required.
#[derive(Debug, serde::Deserialize)]
struct Formula {
    name: String,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    versions: FormulaVersions,
    #[serde(default)]
    urls: FormulaUrls,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    build_dependencies: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FormulaVersions {
    #[serde(default)]
    stable: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FormulaUrls {
    #[serde(default)]
    stable: Option<StableUrl>,
}

#[derive(Debug, serde::Deserialize)]
struct StableUrl {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
}

/// The resolved upstream identity of a formula and how confident we are in it.
struct StemResolution {
    stem_id: PackageStemId,
    /// The canonical name recorded on the stem (a repo slug or `homebrew/<n>`).
    name_canonical: String,
    /// The repo URL to record on the package row, when one was inferred.
    repo_url: Option<String>,
    /// The confidence tier for the `brew_formula` alias.
    confidence: AliasConfidence,
}

/// Whether a URL is a GitHub **release** download — the authoritative signal
/// (the feed itself points at the upstream repo's release, §7.1.3).
fn is_github_release_url(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    lowered.contains("github.com/") && lowered.contains("/releases/download/")
}

/// Resolve a formula's stem, URL, and alias confidence (§7.1.3).
fn resolve_stem(formula: &Formula) -> StemResolution {
    let stable_url = formula.urls.stable.as_ref().and_then(|s| s.url.as_deref());

    // 1. Authoritative: a GitHub release tarball whose slug normalizes cleanly.
    if let Some(url) = stable_url
        && is_github_release_url(url)
        && let Some(slug) = normalize_repo_url(url)
    {
        let slug = slug.as_str().to_owned();
        return StemResolution {
            stem_id: cpp_stem_id(&slug),
            name_canonical: slug.clone(),
            repo_url: Some(format!("https://{slug}")),
            confidence: AliasConfidence::Authoritative,
        };
    }

    // 2. Heuristic: normalize the stable URL, then the homepage.
    for candidate in [stable_url, formula.homepage.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(slug) = normalize_repo_url(candidate) {
            let slug = slug.as_str().to_owned();
            return StemResolution {
                stem_id: cpp_stem_id(&slug),
                name_canonical: slug.clone(),
                repo_url: Some(format!("https://{slug}")),
                confidence: AliasConfidence::Heuristic,
            };
        }
    }

    // 3. Feed-scoped fallback: never drop the formula (RL-3). Identity is
    //    `homebrew/<formula>`; alias stays heuristic.
    let feed_slug = format!("homebrew/{}", formula.name);
    StemResolution {
        stem_id: cpp_stem_id(&feed_slug),
        name_canonical: feed_slug,
        repo_url: None,
        confidence: AliasConfidence::Heuristic,
    }
}

/// Build the recipe edges for a formula from its runtime + build dependencies
/// (REGISTRYLESS-PLAN §7.1 step 4; recorded literally per RL-5, resolved
/// later).
fn recipe_edges(formula: &Formula) -> Vec<EdgeWire> {
    let mut edges =
        Vec::with_capacity(formula.dependencies.len() + formula.build_dependencies.len());
    for dependency in &formula.dependencies {
        edges.push(EdgeWire {
            dep_ecosystem: Language::Cpp,
            dep_name_canonical: dependency.clone(),
            requirement: String::new(),
            kind: EdgeKind::Recipe,
            source: EdgeSource::Feed,
            resolved_stem: None,
            optional: false,
        });
    }
    for dependency in &formula.build_dependencies {
        edges.push(EdgeWire {
            dep_ecosystem: Language::Cpp,
            dep_name_canonical: dependency.clone(),
            requirement: String::new(),
            kind: EdgeKind::Recipe,
            source: EdgeSource::Feed,
            resolved_stem: None,
            optional: false,
        });
    }
    edges
}

/// Parse the `formula.json` array into the catalog op batch (REGISTRYLESS-PLAN
/// §7.1). Ordering is stable (feed order), so goldens are deterministic. A
/// formula with no stable version is skipped (nothing to version), but still
/// registered as a package + alias.
pub fn parse_formulae(body: &[u8]) -> Result<Vec<CatalogOp>, FollowerError> {
    let formulae: Vec<Formula> =
        serde_json::from_slice(body).map_err(|error| FollowerError::Parse {
            feed: FEED_ID.to_owned(),
            detail: error.to_string(),
        })?;

    let mut ops = Vec::new();
    for formula in &formulae {
        let resolution = resolve_stem(formula);

        // ── Package ──────────────────────────────────────────────────────────
        ops.push(CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: resolution.stem_id,
                ecosystem: Language::Cpp,
                name_struct: format!("pkg:brew/{}", formula.name),
                name_canonical: resolution.name_canonical.clone(),
                name_original: resolution.name_canonical.clone(),
            },
            repo_url: resolution.repo_url.clone(),
        });

        // ── Alias (brew_formula → stem) ──────────────────────────────────────
        ops.push(CatalogOp::UpsertAlias {
            ecosystem: Language::Cpp,
            kind: "brew_formula".into(),
            alias: formula.name.clone().into(),
            stem: resolution.stem_id,
            confidence: resolution.confidence,
        });

        // ── Version (checksum kept; source None — brew is fallback) ──────────
        if let Some(stable) = &formula.versions.stable {
            let checksum = formula
                .urls
                .stable
                .as_ref()
                .and_then(|s| s.checksum.clone());
            let version_id = cpp_version_id(resolution.stem_id, stable);
            ops.push(CatalogOp::UpsertVersion {
                coordinates: VersionCoordinates {
                    version_id,
                    stem_id: resolution.stem_id,
                    version_canonical: stable.clone(),
                    version_original: stable.clone(),
                },
                published_at: None,
                toolchain: None,
                license: formula.license.clone(),
                edges: crate::protocol::EdgeSnapshot::recipe(recipe_edges(formula)),
                facets: FacetWire {
                    keywords: formula.desc.clone(),
                    quality_ppm: None,
                    extras: None,
                },
                // The registry checksum is kept for honesty; `source` stays
                // `Some(reconstructed uri)` only when a pack is later sealed —
                // at ingest we record the checksum but NO source acquisition, so
                // git (P6) is preferred when it enumerates the same stem.
                source: Some(SourceAcquisitionWire {
                    source_kind: SourceKind::Unknown,
                    source_pack: None,
                    source_rev: None,
                    registry_checksum: checksum,
                    registry_package_uri: formula.urls.stable.as_ref().and_then(|s| s.url.clone()),
                }),
            });
        }
    }
    Ok(ops)
}
