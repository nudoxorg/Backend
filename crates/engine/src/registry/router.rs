//! Typed multi-registry source routing.
//!
//! The registry protocol owner is deliberately single-source: its cursor,
//! journal, credentials, and feed grammar all belong to one exact authority.
//! This module composes those owners into the process-wide source set used by
//! the product surfaces.  Routing is pure and therefore safe to use during
//! startup, while owners remain a local-service concern and can be opened on
//! first use.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use super::{
    AcquisitionError, AcquisitionPolicy, AuthenticationToken, PackageCoordinate,
    RegistryCoordinate, RegistryEcosystem, RegistryEndpoint, RegistryId, admit_registry_coordinate,
};

mod set;

/// The versioned root namespace reserved for composed registry sources.
pub const REGISTRY_SOURCE_ROOT_VERSION: &str = "v1";

/// Official Cargo sparse index.  Cargo source archives are served from the
/// official static archive host and are admitted by the native transport.
pub const CARGO_SPARSE_INDEX: &str = "https://index.crates.io";
/// Official npm registry.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";
/// Official Python Simple API authority.
pub const PYPI_SIMPLE_API: &str = "https://pypi.org";
/// Official Maven Central repository.
pub const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";
/// Official NuGet v3 service index.
pub const NUGET_V3: &str = "https://api.nuget.org/v3/index.json";
/// Official Go module proxy.
pub const GO_MODULE_PROXY: &str = "https://proxy.golang.org";
/// Official Conan Center service.
pub const CONAN_CENTER: &str = "https://center2.conan.io";

const ROUTER_SOURCE_ID_DOMAIN: &[u8] = b"backend.registry.router-source.v1\0";

/// Failure while composing the compile-time official source set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryConfigurationError {
    /// An official endpoint constant failed endpoint admission.
    OfficialEndpoint(RegistryEcosystem),
    /// An official source failed source-set admission.
    OfficialSource(RegistryEcosystem),
    /// The composed official set did not cover exactly the supported sources.
    OfficialCardinality {
        /// Number of sources required by the closed ecosystem set.
        expected: usize,
        /// Number of sources actually composed.
        actual: usize,
    },
}

impl fmt::Display for RegistryConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OfficialEndpoint(ecosystem) => {
                write!(
                    formatter,
                    "official {ecosystem} registry endpoint is invalid"
                )
            }
            Self::OfficialSource(ecosystem) => {
                write!(formatter, "official {ecosystem} registry source is invalid")
            }
            Self::OfficialCardinality { expected, actual } => write!(
                formatter,
                "official registry source set has {actual} sources; expected {expected}"
            ),
        }
    }
}

impl std::error::Error for RegistryConfigurationError {}

/// One source authority admitted for one package ecosystem.
///
/// A source owns its credential and policy.  The credential is never part of
/// the endpoint identity or durable source root, and the HTTP transport sends
/// it only to the exact source authority.  `namespace` optionally narrows a
/// source to one private/forge package namespace; an unscoped source matches
/// every coordinate in its ecosystem.
#[derive(Clone, Eq, PartialEq)]
pub struct RegistrySource {
    endpoint: RegistryEndpoint,
    source_id: RegistryId,
    priority: u16,
    policy: AcquisitionPolicy,
    native: bool,
    authentication: Option<AuthenticationToken>,
    namespace: Option<Arc<str>>,
}

impl fmt::Debug for RegistrySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrySource")
            .field("id", &self.id())
            .field("ecosystem", &self.ecosystem())
            .field("priority", &self.priority)
            .field("policy", &self.policy)
            .field("native", &self.native)
            .field("has_authentication", &self.authentication.is_some())
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl RegistrySource {
    /// Creates an online native source for an admitted endpoint.
    #[must_use]
    pub fn new(endpoint: RegistryEndpoint) -> Self {
        let source_id = source_identity(&endpoint, true, None);
        Self {
            endpoint,
            source_id,
            // Official/default sources use the stable low priority value;
            // configured mirrors can choose a lower value to take precedence.
            priority: 100,
            policy: AcquisitionPolicy::Online,
            native: true,
            authentication: None,
            namespace: None,
        }
    }

    /// Stable credential-free source identity.
    #[must_use]
    pub const fn id(&self) -> RegistryId {
        self.source_id
    }

    /// Source ecosystem.
    #[must_use]
    pub const fn ecosystem(&self) -> RegistryEcosystem {
        self.endpoint.ecosystem()
    }

    /// Validated source endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &RegistryEndpoint {
        &self.endpoint
    }

    /// Explicit source priority. Lower values are tried first.
    #[must_use]
    pub const fn priority(&self) -> u16 {
        self.priority
    }

    /// Network policy for this source.
    #[must_use]
    pub const fn policy(&self) -> AcquisitionPolicy {
        self.policy
    }

    /// Whether the ecosystem-native protocol is selected.
    #[must_use]
    pub const fn native(&self) -> bool {
        self.native
    }

    /// Returns the exact package namespace restriction, if any.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// Returns whether this source has a credential without exposing it.
    #[must_use]
    pub const fn has_authentication(&self) -> bool {
        self.authentication.is_some()
    }

    /// Returns the process-local credential for transport composition.
    #[must_use]
    pub fn authentication(&self) -> Option<AuthenticationToken> {
        self.authentication.clone()
    }

    /// Returns an endpoint whose durable owner identity includes this source's
    /// adapter and namespace semantics. Credentials are never included.
    #[must_use]
    pub fn endpoint_for_owner(&self) -> RegistryEndpoint {
        self.endpoint.with_id(self.source_id)
    }

    /// Sets deterministic source priority.
    #[must_use]
    pub const fn with_priority(mut self, priority: u16) -> Self {
        self.priority = priority;
        self
    }

    /// Sets source online/offline policy.
    #[must_use]
    pub const fn with_policy(mut self, policy: AcquisitionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Selects the native ecosystem protocol, or the canonical feed grammar.
    #[must_use]
    pub fn with_native(mut self, native: bool) -> Self {
        self.native = native;
        self.refresh_identity();
        self
    }

    /// Associates a token with this exact source authority.
    #[must_use]
    pub fn with_authentication(mut self, authentication: AuthenticationToken) -> Self {
        self.authentication = Some(authentication);
        self
    }

    /// Restricts this source to one exact registry-native namespace prefix.
    ///
    /// Namespaces are matched against the admitted qualified package name,
    /// with a separator boundary, so `acme` does not match `acme-tools`.
    #[must_use]
    pub fn for_namespace(mut self, namespace: impl Into<String>) -> Result<Self, AcquisitionError> {
        let namespace = namespace.into();
        if namespace.is_empty()
            || namespace.len() > 256
            || namespace.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'+' | b':'))
            })
        {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        self.namespace = Some(Arc::from(namespace));
        self.refresh_identity();
        Ok(self)
    }

    fn refresh_identity(&mut self) {
        self.source_id = source_identity(&self.endpoint, self.native, self.namespace());
    }

    fn matches(&self, coordinate: &RegistryCoordinate) -> bool {
        if self.ecosystem() != coordinate.ecosystem() {
            return false;
        }
        let Some(namespace) = self.namespace() else {
            return true;
        };
        let name = coordinate.qualified_name().as_str();
        name == namespace
            || name
                .strip_prefix(namespace)
                .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with(':'))
    }
}

/// A typed, deterministic set of sources partitioned by ecosystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySourceSet {
    sources: BTreeMap<RegistryEcosystem, Vec<RegistrySource>>,
}

fn source_order(source: &RegistrySource) -> (u16, RegistryId, Option<Arc<str>>) {
    (source.priority(), source.id(), source.namespace.clone())
}

fn source_identity(
    endpoint: &RegistryEndpoint,
    native: bool,
    namespace: Option<&str>,
) -> RegistryId {
    let mut hash = blake3::Hasher::new();
    hash.update(ROUTER_SOURCE_ID_DOMAIN);
    hash.update(&endpoint.id().as_bytes());
    hash.update(if native {
        b"native-adapter-v1".as_slice()
    } else {
        b"canonical-feed-v1".as_slice()
    });
    if let Some(namespace) = namespace {
        hash.update(&(namespace.len() as u64).to_be_bytes());
        hash.update(namespace.as_bytes());
    } else {
        hash.update(&0_u64.to_be_bytes());
    }
    RegistryId::from_bytes(*hash.finalize().as_bytes())
}

fn authority_key(value: &str) -> Option<String> {
    let uri = value.parse::<ureq::http::Uri>().ok()?;
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return None;
    };
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.as_str().to_ascii_lowercase()
    ))
}

/// A typed source route for one exact package coordinate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryRoute {
    coordinate: PackageCoordinate,
    admitted: RegistryCoordinate,
    candidates: Vec<RegistrySource>,
}

impl RegistryRoute {
    /// Exact coordinate admitted for this route.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageCoordinate {
        &self.coordinate
    }

    /// Decoded registry coordinate used by source adapters.
    #[must_use]
    pub const fn admitted(&self) -> &RegistryCoordinate {
        &self.admitted
    }

    /// Ordered source candidates, from highest priority to fallback.
    #[must_use]
    pub fn candidates(&self) -> &[RegistrySource] {
        &self.candidates
    }

    /// Returns the selected primary source.
    #[must_use]
    pub fn primary(&self) -> &RegistrySource {
        // Route construction proves at least one candidate.
        &self.candidates[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn coordinate(value: &str) -> PackageCoordinate {
        PackageCoordinate::parse(value).expect("coordinate")
    }

    #[test]
    fn official_defaults_cover_every_supported_ecosystem() {
        let set = RegistrySourceSet::defaults().expect("official defaults");
        assert_eq!(set.len(), 7);
        for (ecosystem, endpoint) in [
            (RegistryEcosystem::Cargo, CARGO_SPARSE_INDEX),
            (RegistryEcosystem::Npm, NPM_REGISTRY),
            (RegistryEcosystem::Pypi, PYPI_SIMPLE_API),
            (RegistryEcosystem::Maven, MAVEN_CENTRAL),
            (RegistryEcosystem::Nuget, NUGET_V3),
            (RegistryEcosystem::Golang, GO_MODULE_PROXY),
            (RegistryEcosystem::Cpp, CONAN_CENTER),
        ] {
            let source = set
                .for_ecosystem(ecosystem)
                .first()
                .expect("default source");
            assert_eq!(source.endpoint().as_str(), endpoint);
            assert_eq!(
                set.route(&coordinate(match ecosystem {
                    RegistryEcosystem::Cargo => "pkg:cargo/serde@1.0.0",
                    RegistryEcosystem::Npm => "pkg:npm/lodash@4.17.21",
                    RegistryEcosystem::Pypi => "pkg:pypi/attrs@24.2.0",
                    RegistryEcosystem::Maven => "pkg:maven/org.example/widget@1.0.0",
                    RegistryEcosystem::Nuget => "pkg:nuget/nullable@1.3.1",
                    RegistryEcosystem::Golang => "pkg:golang/github.com/acme/widget@v1.0.0",
                    RegistryEcosystem::Cpp => "pkg:generic/acme/zlib@1.0.0",
                }))
                .expect("route")
                .primary()
                .endpoint()
                .as_str(),
                endpoint
            );
        }
    }

    #[test]
    fn source_order_is_priority_then_credential_free_identity() {
        let endpoint_a = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://mirror-a.test")
            .expect("endpoint");
        let endpoint_b = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://mirror-b.test")
            .expect("endpoint");
        let set = RegistrySourceSet::official()
            .expect("official defaults")
            .with_source(RegistrySource::new(endpoint_a).with_priority(1))
            .expect("source")
            .with_source(RegistrySource::new(endpoint_b).with_priority(1))
            .expect("source");
        let route = set
            .route(&coordinate("pkg:cargo/serde@1.0.0"))
            .expect("route");
        assert_eq!(route.candidates().len(), 3);
        assert_eq!(route.candidates()[0].priority(), 1);
        assert!(route.candidates()[0].id() < route.candidates()[1].id());
    }

    #[test]
    fn source_identity_binds_adapter_and_namespace_but_not_credentials() {
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://mirror.test")
            .expect("endpoint");
        let native = RegistrySource::new(endpoint.clone());
        let canonical = RegistrySource::new(endpoint.clone()).with_native(false);
        let private = RegistrySource::new(endpoint.clone())
            .for_namespace("acme")
            .expect("namespace");
        let credentialed = native
            .clone()
            .with_authentication(AuthenticationToken::new("Bearer secret").expect("token"));
        assert_ne!(native.id(), canonical.id());
        assert_ne!(native.id(), private.id());
        assert_eq!(native.id(), credentialed.id());
        let debug = format!("{credentialed:?}");
        assert!(debug.contains("has_authentication: true"));
        assert!(!debug.contains("Bearer secret"));
    }

    #[test]
    fn configured_mirror_is_a_deterministic_fallback_before_the_official_source() {
        let endpoint =
            RegistryEndpoint::new(RegistryEcosystem::Npm, "https://mirror.test").expect("endpoint");
        let set = RegistrySourceSet::official()
            .expect("official defaults")
            .with_source(RegistrySource::new(endpoint).with_priority(10))
            .expect("mirror source");
        let route = set
            .route(&coordinate("pkg:npm/lodash@4.17.21"))
            .expect("route");
        assert_eq!(route.candidates().len(), 2);
        assert_eq!(route.candidates()[0].priority(), 10);
        assert_eq!(route.candidates()[1].priority(), 100);
    }

    #[test]
    fn authentication_is_scoped_to_the_exact_source_authority() {
        let mirror = RegistryEndpoint::new(
            RegistryEcosystem::Maven,
            "https://mirror.test/repository/maven-public",
        )
        .expect("mirror endpoint");
        let other = RegistryEndpoint::new(
            RegistryEcosystem::Maven,
            "https://other.test/repository/maven-public",
        )
        .expect("other endpoint");
        let mut set = RegistrySourceSet::official()
            .expect("official defaults")
            .with_source(RegistrySource::new(mirror))
            .expect("mirror source")
            .with_source(RegistrySource::new(other))
            .expect("other source");
        assert!(
            set.authenticate_authority(
                "https://mirror.test/repository/maven-private",
                AuthenticationToken::new("Bearer scoped").expect("token"),
            )
            .expect("auth scope")
        );
        assert!(
            set.for_ecosystem(RegistryEcosystem::Maven)
                .iter()
                .find(|source| source
                    .endpoint()
                    .as_str()
                    .starts_with("https://mirror.test"))
                .is_some_and(RegistrySource::has_authentication)
        );
        assert!(
            set.for_ecosystem(RegistryEcosystem::Maven)
                .iter()
                .find(|source| source.endpoint().as_str().starts_with("https://other.test"))
                .is_some_and(|source| !source.has_authentication())
        );
    }

    #[test]
    fn namespace_sources_are_scoped_to_private_coordinates() {
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Npm, "https://forge.example.test")
            .expect("endpoint");
        let private = RegistrySource::new(endpoint)
            .with_priority(0)
            .for_namespace("acme")
            .expect("namespace");
        let set = RegistrySourceSet::official()
            .expect("official defaults")
            .with_source(private)
            .expect("source");
        let private_route = set
            .route(&coordinate("pkg:npm/acme/widgets@1.0.0"))
            .expect("private route");
        assert_eq!(
            private_route.primary().endpoint().as_str(),
            "https://forge.example.test"
        );
        let public_route = set
            .route(&coordinate("pkg:npm/widgets@1.0.0"))
            .expect("public route");
        assert_eq!(public_route.primary().endpoint().as_str(), NPM_REGISTRY);
    }

    #[test]
    fn offline_override_changes_policy_without_changing_identity() {
        let set = RegistrySourceSet::official()
            .expect("official defaults")
            .offline();
        let route = set
            .route(&coordinate("pkg:cargo/serde@1.0.0"))
            .expect("route");
        let source = route.primary();
        assert_eq!(source.policy(), AcquisitionPolicy::Offline);
        assert_eq!(source.endpoint().as_str(), CARGO_SPARSE_INDEX);
    }

    #[test]
    fn enriched_source_owner_reloads_from_its_versioned_root_without_network() {
        let source = RegistrySource::new(
            RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://mirror.test")
                .expect("endpoint"),
        );
        let root = std::env::temp_dir().join(format!(
            "backend-registry-router-cold-reload-{}-{}",
            std::process::id(),
            source.id().as_bytes()[0]
        ));
        let _ = fs::remove_dir_all(&root);
        let versioned = root.join(REGISTRY_SOURCE_ROOT_VERSION);
        let objects = versioned.join("cas").join("objects");
        let (_, recovery) = super::super::owner::RegistryOwner::open_with_shared_objects(
            &versioned,
            source.endpoint_for_owner(),
            AcquisitionPolicy::Offline,
            super::super::identity::AcquisitionLimits::default(),
            &objects,
        )
        .expect("cold owner");
        assert_eq!(recovery.cursor.sequence(), 0);
        let layout = super::super::owner::storage_root(&versioned, &source.endpoint_for_owner());
        assert!(layout.join("registry.journal").is_file());
        assert!(objects.is_dir());
        let (_, reloaded) = super::super::owner::RegistryOwner::open_with_shared_objects(
            &versioned,
            source.endpoint_for_owner(),
            AcquisitionPolicy::Offline,
            super::super::identity::AcquisitionLimits::default(),
            &objects,
        )
        .expect("reload owner");
        assert_eq!(reloaded.cursor.sequence(), 0);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
