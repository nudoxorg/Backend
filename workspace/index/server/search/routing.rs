//! Quality-mode routing for the dense Stage-1 (09-vector §20.5).
//!
//! The client states *scope*, *quality mode*, and its claimed hot-set
//! membership; the server maps that — through the shared
//! [`vector::routing::plan_route`] vocabulary — onto which dense
//! collection (if any) answers Stage-1 here, whether the deep rerank stage
//! runs, and how the results are labeled (`source=` telemetry, §8.4).
//!
//! This module only *decides*; dispatch stays where it lives today: the
//! [`SearchPlanner`] still owns the semantic budget and every
//! [`SemanticGate`] issuance — routing chooses a collection, it never mints a
//! gate or bypasses one.
//!
//! # The server-side routing rows (§20.5)
//!
//! | quality  | dense Stage-1 here            | rerank | label          |
//! |----------|-------------------------------|--------|----------------|
//! | local    | none (client-local only)      | no     | `precise-only` |
//! | parity   | parity collection (Jina)      | no     | `index-jina`   |
//! | premium  | premium collection (Voyage)   | no     | `index-voyage` |
//! | deep     | premium, parity fallback      | yes    | per collection |
//!
//! Scope narrows, it does not redirect: `Project` and claimed-hot dep
//! packages are served by the client's local shards, so the server excludes
//! the claimed packages from its Stage-1 rather than double-answering them.
//!
//! [`SearchPlanner`]: crate::server::search::SearchPlanner
//! [`SemanticGate`]: registry::vector::SemanticGate

#[allow(unused_imports)]
use crate::server::{registry, vector};
use std::collections::HashSet;

use heart::PackageId;
pub use vector::routing::{QualityMode, QueryScope};

/// Everything the router needs about one request.
#[derive(Debug, Clone)]
pub struct RouteInputs {
    /// What slice of the world the query addresses.
    pub scope: QueryScope,
    /// The client's requested quality mode. Wire default: `Parity` (the
    /// server is by definition online).
    pub quality: QualityMode,
    /// Whether this node considers itself online. Always `true` for a serving
    /// request; carried so the §20.5 offline rows are testable through the
    /// same function the handler calls.
    pub online: bool,
    /// Dep packages the **client claims** it holds as local baked shards.
    /// Claimed — not verified — so the only thing the claim may do is *narrow*
    /// the server's answer (excluding a package the client then does not
    /// actually cover loses recall for that client only, never correctness
    /// for anyone else).
    pub claimed_hot: Vec<PackageId>,
}

impl RouteInputs {
    /// The serving-side default: parity, org-wide, online, no claims.
    pub fn serving_default() -> Self {
        Self {
            scope: QueryScope::Org,
            quality: QualityMode::Parity,
            online: true,
            claimed_hot: Vec::new(),
        }
    }

    /// Lower the wire routing knobs from the one query algebra
    /// ([`heart::query::Routing`]) into the server-side routing inputs. A serving
    /// request is online by definition; the client's claimed hot-set narrows the
    /// server's dense stage (§20.5).
    ///
    /// The `heart::query` enums are the wire vocabulary; they are mapped onto the
    /// re-exported [`vector::routing`] enums by hand here (the two are
    /// foreign to this crate, so no blanket `From` impl is possible without
    /// touching `vector_core`).
    pub fn from_wire(routing: &heart::query::Routing) -> Self {
        let quality = match routing.quality {
            heart::query::QualityMode::Local => QualityMode::Local,
            heart::query::QualityMode::Parity => QualityMode::Parity,
            heart::query::QualityMode::Premium => QualityMode::Premium,
            heart::query::QualityMode::Deep => QualityMode::Deep,
        };
        let scope = match routing.reach {
            heart::query::QueryReach::Project => QueryScope::Project,
            heart::query::QueryReach::Deps => QueryScope::Deps,
            heart::query::QueryReach::Org => QueryScope::Org,
        };
        Self {
            scope,
            quality,
            online: true,
            claimed_hot: routing.hot_packages.clone(),
        }
    }
}

/// Which dense collection answers Stage-1 on this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenseCollection {
    /// The parity collection — the compiled-in embedding model (Jina code),
    /// i.e. the existing `Semantic` path.
    Parity,
    /// The premium collection (Voyage). Not yet wired to a second branded
    /// store; the dispatcher answers from parity and labels the fallback
    /// truthfully (never a silent relabel).
    Premium,
}

/// The server's routing decision for one request.
#[derive(Debug, Clone)]
pub struct ServerRoute {
    /// Which collection answers dense Stage-1; `None` = no dense stage here
    /// (precise-only response, e.g. `local` quality reaching the server).
    pub collection: Option<DenseCollection>,
    /// Whether the deep rerank stage runs over the Stage-1 top-K.
    pub rerank: bool,
    /// The §8.4 `source` telemetry / response label for hits answered here.
    pub source_label: &'static str,
    /// Packages excluded from the server's Stage-1 because the client claims
    /// local shards for them.
    pub excluded: HashSet<PackageId>,
}

/// How many Stage-1 candidates feed the deep rerank stage (§20.5: "stage-1
/// top-100 → server rerank").
pub const DEEP_STAGE_ONE_LIMIT: u32 = 100;

/// Decide the server-side route for one request.
///
/// This is the server's half of the §20.5 table ([`vector::routing`] is
/// the client's full evaluation — the client decides *whether* to ask the
/// server; this decides what the server does when asked). The dispatch below
/// is total on `(quality, online)`; consistency with the shared table is
/// pinned by the row tests, not by re-evaluating client inputs the server
/// cannot know (project readiness, premium subscription).
pub fn route_stage_one(inputs: &RouteInputs) -> ServerRoute {
    let excluded: HashSet<PackageId> = inputs.claimed_hot.iter().copied().collect();
    let (collection, rerank) = match inputs.quality {
        // `local` never routes dense work to the server; a request that still
        // arrives here gets the precise surface only.
        QualityMode::Local => (None, false),
        QualityMode::Parity => (Some(DenseCollection::Parity), false),
        QualityMode::Premium => (Some(DenseCollection::Premium), false),
        // Deep = premium Stage-1 when available, parity otherwise, then rerank.
        QualityMode::Deep => (Some(DenseCollection::Premium), true),
    };
    // Offline drops the dense Stage-1 collection *and* the rerank stage: there is
    // no dense candidate set to rerank, and the reranker is itself a network
    // service. The offline row is therefore precise-only (§20.5 table stays fully
    // representable).
    let (collection, rerank) = if inputs.online {
        (collection, rerank)
    } else {
        (None, false)
    };

    let source_label = match collection {
        None => "precise-only",
        Some(DenseCollection::Parity) => "index-jina",
        Some(DenseCollection::Premium) => "index-voyage",
    };
    ServerRoute {
        collection,
        rerank,
        source_label,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(scope: QueryScope, quality: QualityMode) -> RouteInputs {
        RouteInputs {
            scope,
            quality,
            online: true,
            claimed_hot: Vec::new(),
        }
    }

    /// §20.5 row: parity quality → the parity (Jina) collection, no rerank,
    /// labeled `index-jina`.
    #[test]
    fn parity_routes_to_parity_collection() {
        let route = route_stage_one(&inputs(QueryScope::Deps, QualityMode::Parity));
        assert_eq!(route.collection, Some(DenseCollection::Parity));
        assert!(!route.rerank);
        assert_eq!(route.source_label, "index-jina");
    }

    /// §20.5 row: premium quality → the premium (Voyage) collection.
    #[test]
    fn premium_routes_to_premium_collection() {
        let route = route_stage_one(&inputs(QueryScope::Org, QualityMode::Premium));
        assert_eq!(route.collection, Some(DenseCollection::Premium));
        assert!(!route.rerank);
        assert_eq!(route.source_label, "index-voyage");
    }

    /// §20.5 row: deep = premium/parity Stage-1 then rerank.
    #[test]
    fn deep_adds_rerank_stage() {
        let route = route_stage_one(&inputs(QueryScope::Org, QualityMode::Deep));
        assert_eq!(route.collection, Some(DenseCollection::Premium));
        assert!(route.rerank);
    }

    /// §20.5 row: local quality never runs a dense stage on the server.
    #[test]
    fn local_is_precise_only() {
        let route = route_stage_one(&inputs(QueryScope::Project, QualityMode::Local));
        assert_eq!(route.collection, None);
        assert!(!route.rerank);
        assert_eq!(route.source_label, "precise-only");
    }

    /// §20.5 row: offline → the dense stage is omitted, explicitly labeled
    /// (never a silent empty answer presented as complete).
    #[test]
    fn offline_omits_dense_stage() {
        let mut request = inputs(QueryScope::Deps, QualityMode::Parity);
        request.online = false;
        let route = route_stage_one(&request);
        assert_eq!(route.collection, None);
        assert_eq!(route.source_label, "precise-only");
    }

    /// §20.5 row: dep packages in the client's claimed hot set are excluded
    /// from the server's Stage-1 (the client answers those locally).
    #[test]
    fn claimed_hot_packages_are_excluded() {
        let hot = PackageId::from_uuid(uuid::Uuid::from_u128(11));
        let mut request = inputs(QueryScope::Deps, QualityMode::Parity);
        request.claimed_hot = vec![hot];
        let route = route_stage_one(&request);
        assert!(route.excluded.contains(&hot));
        assert_eq!(route.collection, Some(DenseCollection::Parity));
    }
}
