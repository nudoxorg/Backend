//! The central egress guard (REGISTRYLESS-PLAN §10, gate G-10).
//!
//! The one hard rule of internal / enterprise mode: **a stem whose authority
//! matches an internal host must never trigger public-feed lookups, OSV calls,
//! Software-Heritage requests, or any outbound request beyond the matching host
//! itself.** This module is the *single* predicate that decides, before any
//! outbound request leaves the process, whether a fetch is allowed under the
//! configured policy. It is deliberately transport-agnostic and pure so it can
//! be dropped into the narrowest HTTP chokepoint (see the wiring note below) and
//! unit-tested with no network.
//!
//! # Wiring
//! The predicate belongs at the one shared transport chokepoint — currently
//! [`registry::upstream::UpstreamClient::get`] (`workspace/registry/upstream.rs`,
//! immediately before `self.inner.get(url).send()`). The `registry`/`server`
//! owners insert one call:
//! ```ignore
//! policy.permit(&EgressRequest { stem_authority: Some(stem_authority), url })?;
//! ```
//! translating an [`EgressDenied`] into their transport error. Placing it here
//! (in `heart`) keeps it a reusable, dependency-light predicate rather than
//! coupling it to whichever crate owns the client.
//!
//! # Adversarial parsing
//! Host matching is performed on the **parsed** URL authority via
//! [`url::Url`], never a substring scan. This defeats the classic tricks:
//! `https://public.com@internal.host/…` has host `internal.host` (the
//! `public.com` is userinfo), a port never changes the host, and
//! `https://evilinternal.example/` does **not** match `*.internal.example`
//! because matching is label-boundary aware.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A host-glob pattern (the `GOPRIVATE` analog). Two shapes are supported:
/// - an exact host (`git.corp.example`) — matches that host only;
/// - a leading-wildcard host (`*.internal.example`) — matches the base domain
///   `internal.example` *and* any subdomain `<label>.internal.example`, on a
///   label boundary (so `evilinternal.example` never matches).
///
/// Matching is ASCII-case-insensitive (hosts are folded to lowercase). A
/// trailing dot on either side is ignored (`internal.example.` ≡
/// `internal.example`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostGlob(String);

impl HostGlob {
    /// Build a host glob from a configured pattern string.
    pub fn new(pattern: impl Into<String>) -> Self {
        HostGlob(normalize_host(&pattern.into()))
    }

    /// Whether `host` (already a parsed URL authority host) matches this glob.
    ///
    /// `host` is folded to lowercase and its trailing dot stripped before the
    /// comparison, so the caller passes the raw `url::Url::host_str` result.
    pub fn matches(&self, host: &str) -> bool {
        let host = normalize_host(host);
        match self.0.strip_prefix("*.") {
            // Wildcard: the base domain itself, or any `.`-boundary subdomain.
            Some(base) => host == base || host.ends_with(&format!(".{base}")),
            // Exact host.
            None => host == self.0,
        }
    }
}

/// Lowercase a host and strip a single trailing dot (`EXAMPLE.COM.` →
/// `example.com`). Pure; no allocation beyond the owned result.
fn normalize_host(host: &str) -> String {
    let trimmed = host.strip_suffix('.').unwrap_or(host);
    trimmed.to_ascii_lowercase()
}

/// The internal-mode egress policy (REGISTRYLESS-PLAN §10.1). Constructed from
/// deployment config; consulted by [`EgressPolicy::permit`] at the transport
/// chokepoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPolicy {
    /// Hosts (globs) that name the internal / enterprise world. A stem whose
    /// authority matches any of these is *internal*, and outbound requests on
    /// its behalf are confined to internal hosts (the G-10 rule).
    pub internal_hosts: Vec<HostGlob>,
    /// Optional files listing internal repository URLs, one per line (the
    /// `repo_lists` config). Held for the discovery reconcile; not consulted by
    /// the permit predicate itself, but carried on the policy so the whole
    /// internal-mode config is one value.
    pub repo_lists: Vec<PathBuf>,
}

/// One outbound request presented to the guard: the URL, plus the authority of
/// the stem on whose behalf the request is being made (when known).
///
/// `stem_authority` is the host component of the stem's repository slug (e.g.
/// `git.corp.example` for `git.corp.example/team/lib`). `None` means the request
/// is not scoped to a specific stem (a generic public feed crawl); such requests
/// are always permitted — the internal rule only constrains *internal* stems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressRequest<'a> {
    /// The authority (host) of the stem this request serves, when scoped.
    pub stem_authority: Option<&'a str>,
    /// The full target URL of the outbound request.
    pub url: &'a str,
}

/// Why an outbound request was denied by the egress guard.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressDenied {
    /// The request served an internal stem but targeted a host outside the
    /// internal set — a forbidden public leak (G-10).
    #[error(
        "egress denied: internal stem on {stem_authority:?} may not reach public host {target_host:?}"
    )]
    InternalStemPublicTarget {
        /// The internal stem's authority.
        stem_authority: String,
        /// The public host the request tried to reach.
        target_host: String,
    },
    /// The target URL could not be parsed, or carried no host — never permitted,
    /// because a hostless URL cannot be checked against the policy.
    #[error("egress denied: target URL is unparseable or has no host: {url:?}")]
    UnparseableTarget {
        /// The offending URL.
        url: String,
    },
}

impl EgressPolicy {
    /// A policy with no internal hosts — public mode; every request is permitted.
    pub fn public() -> Self {
        EgressPolicy::default()
    }

    /// Whether `host` matches any configured internal host glob (pure predicate).
    pub fn is_internal_host(&self, host: &str) -> bool {
        self.internal_hosts.iter().any(|glob| glob.matches(host))
    }

    /// The one central egress decision (G-10).
    ///
    /// Rules, applied to the **parsed** target host:
    /// 1. A hostless/unparseable target is denied (it cannot be policy-checked).
    /// 2. When the request serves a stem whose authority is internal, the target
    ///    host must also be internal — otherwise it is a public leak and denied.
    /// 3. All other requests (public stem, or unscoped) are permitted.
    ///
    /// Note that an *internal target* is always allowed regardless of the stem
    /// context (talking to the internal host itself is the whole point); only a
    /// *public* target under an *internal* stem is forbidden.
    pub fn permit(&self, request: &EgressRequest<'_>) -> Result<(), EgressDenied> {
        let target_host =
            parse_host(request.url).ok_or_else(|| EgressDenied::UnparseableTarget {
                url: request.url.to_owned(),
            })?;

        // Only internal stems are constrained.
        let stem_is_internal = request
            .stem_authority
            .is_some_and(|authority| self.is_internal_host(authority));
        if !stem_is_internal {
            return Ok(());
        }

        // An internal stem may only reach internal hosts.
        if self.is_internal_host(&target_host) {
            Ok(())
        } else {
            Err(EgressDenied::InternalStemPublicTarget {
                stem_authority: request.stem_authority.unwrap_or_default().to_owned(),
                target_host,
            })
        }
    }
}

/// Parse the host authority out of a URL, guarding on the parsed host and never
/// a substring. Returns the lowercased host, or `None` if the URL is
/// unparseable or hostless. Userinfo (`user@`), port (`:443`), path, and query
/// are all correctly excluded by [`url::Url::host_str`].
pub fn parse_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed.host_str().map(normalize_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with(hosts: &[&str]) -> EgressPolicy {
        EgressPolicy {
            internal_hosts: hosts.iter().map(|h| HostGlob::new(*h)).collect(),
            repo_lists: Vec::new(),
        }
    }

    #[test]
    fn wildcard_matches_subdomain_and_base_but_not_lookalike() {
        let glob = HostGlob::new("*.internal.example");
        assert!(glob.matches("sub.internal.example"), "subdomain matches");
        assert!(
            glob.matches("deep.sub.internal.example"),
            "deep subdomain matches"
        );
        assert!(glob.matches("internal.example"), "base domain matches");
        // The adversarial look-alike must NOT match — matching is label-aware.
        assert!(
            !glob.matches("evilinternal.example"),
            "look-alike must not match"
        );
        assert!(
            !glob.matches("internal.example.attacker.com"),
            "suffix trick must not match"
        );
    }

    #[test]
    fn exact_host_matches_only_itself() {
        let glob = HostGlob::new("git.corp.example");
        assert!(glob.matches("git.corp.example"));
        assert!(!glob.matches("evil.git.corp.example"));
        assert!(!glob.matches("git.corp.example.evil.com"));
    }

    #[test]
    fn matching_is_case_insensitive_and_dot_tolerant() {
        let glob = HostGlob::new("Internal.Example");
        assert!(glob.matches("INTERNAL.EXAMPLE"));
        assert!(glob.matches("internal.example."));
    }

    #[test]
    fn parse_host_ignores_userinfo_port_and_path() {
        // The classic userinfo trick: host is internal.host, NOT public.com.
        assert_eq!(
            parse_host("https://public.com@internal.host/osv/query").as_deref(),
            Some("internal.host")
        );
        // A port never changes the host.
        assert_eq!(
            parse_host("https://internal.host:8443/x").as_deref(),
            Some("internal.host")
        );
        // Case folded.
        assert_eq!(
            parse_host("https://INTERNAL.HOST/x").as_deref(),
            Some("internal.host")
        );
    }

    #[test]
    fn internal_stem_may_reach_internal_host() {
        let policy = policy_with(&["*.internal.example", "git.corp.example"]);
        let request = EgressRequest {
            stem_authority: Some("git.corp.example"),
            url: "https://git.corp.example/team/lib/info/refs",
        };
        assert_eq!(policy.permit(&request), Ok(()));
    }

    #[test]
    fn internal_stem_may_not_reach_public_host() {
        // The G-10 rule: an internal stem must not trigger a public OSV lookup.
        let policy = policy_with(&["git.corp.example"]);
        let request = EgressRequest {
            stem_authority: Some("git.corp.example"),
            url: "https://api.osv.dev/v1/query",
        };
        assert!(matches!(
            policy.permit(&request),
            Err(EgressDenied::InternalStemPublicTarget { .. })
        ));
    }

    #[test]
    fn internal_stem_userinfo_disguised_public_target_is_denied() {
        // A public target dressed up with an internal userinfo must still be
        // judged on its parsed host (public.example), and denied.
        let policy = policy_with(&["internal.host"]);
        let request = EgressRequest {
            stem_authority: Some("internal.host"),
            url: "https://internal.host@public.example/leak",
        };
        assert!(
            matches!(
                policy.permit(&request),
                Err(EgressDenied::InternalStemPublicTarget { .. })
            ),
            "the parsed host is public.example — a leak"
        );
    }

    #[test]
    fn public_stem_may_reach_public_host() {
        let policy = policy_with(&["git.corp.example"]);
        let request = EgressRequest {
            stem_authority: Some("github.com"),
            url: "https://api.osv.dev/v1/query",
        };
        assert_eq!(
            policy.permit(&request),
            Ok(()),
            "public stems are unconstrained"
        );
    }

    #[test]
    fn unscoped_request_is_permitted() {
        let policy = policy_with(&["git.corp.example"]);
        let request = EgressRequest {
            stem_authority: None,
            url: "https://crates.io/api",
        };
        assert_eq!(policy.permit(&request), Ok(()));
    }

    #[test]
    fn hostless_or_unparseable_target_is_denied() {
        let policy = policy_with(&["git.corp.example"]);
        let request = EgressRequest {
            stem_authority: Some("git.corp.example"),
            url: "not a url",
        };
        assert!(matches!(
            policy.permit(&request),
            Err(EgressDenied::UnparseableTarget { .. })
        ));
    }

    /// Gate G-10 (REGISTRYLESS §10.2): a **mock transport** that consults the
    /// guard before every send proves that operating on behalf of an internal
    /// stem produces *zero* public egress — the guard short-circuits every
    /// public target (OSV, a public feed, Software-Heritage) while still allowing
    /// the internal host itself.
    #[test]
    fn mock_transport_makes_zero_public_egress_for_internal_stem() {
        use std::cell::RefCell;

        // A mock transport: records only the URLs it would actually send to, and
        // refuses anything the guard denies.
        struct MockTransport<'p> {
            policy: &'p EgressPolicy,
            sent: RefCell<Vec<String>>,
        }
        impl MockTransport<'_> {
            fn get(&self, stem_authority: &str, url: &str) -> Result<(), EgressDenied> {
                self.policy.permit(&EgressRequest {
                    stem_authority: Some(stem_authority),
                    url,
                })?;
                self.sent.borrow_mut().push(url.to_owned());
                Ok(())
            }
        }

        let policy = policy_with(&["*.internal.example"]);
        let transport = MockTransport {
            policy: &policy,
            sent: RefCell::new(Vec::new()),
        };
        let internal_stem = "code.internal.example";

        // The pipeline, operating for an internal stem, would attempt: a public
        // OSV lookup, a public vcpkg feed fetch, and a legitimate internal fetch.
        let public_osv = transport.get(internal_stem, "https://api.osv.dev/v1/query");
        let public_feed = transport.get(
            internal_stem,
            "https://raw.githubusercontent.com/microsoft/vcpkg/master/x",
        );
        let internal_fetch = transport.get(
            internal_stem,
            "https://code.internal.example/team/lib/info/refs",
        );

        assert!(
            public_osv.is_err(),
            "public OSV lookup for an internal stem must be denied"
        );
        assert!(
            public_feed.is_err(),
            "public feed fetch for an internal stem must be denied"
        );
        assert!(
            internal_fetch.is_ok(),
            "the internal host itself is reachable"
        );

        // Zero *public* URLs left the transport; only the internal host was hit.
        let sent = transport.sent.borrow();
        assert_eq!(sent.len(), 1, "exactly one request left the transport");
        assert!(
            sent.iter()
                .all(|url| parse_host(url).is_some_and(|h| policy.is_internal_host(&h))),
            "every sent request targeted an internal host — zero public egress"
        );
    }

    #[test]
    fn public_policy_permits_everything_parseable() {
        let policy = EgressPolicy::public();
        let request = EgressRequest {
            stem_authority: Some("anything"),
            url: "https://example.com/x",
        };
        assert_eq!(policy.permit(&request), Ok(()));
    }
}
