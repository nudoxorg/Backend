//! The single place where "what does ecosystem X need?" is answered.
//!
//! One sealed trait, [`EcosystemSpec`], one ZST implementation per [`Language`]
//! variant, and one exhaustive dispatch table ([`spec`]). Everything the
//! registry mirror, ingest, and search pipeline need to know about an ecosystem
//! — name grammar, version grammar, upstream endpoints, archive framing,
//! manifest shape, search normalization — lives behind it. No `match ecosystem`
//! arm may exist outside this crate.
//!
//! This crate is **pure**: string/version/URL/policy logic only. No IO, no
//! reqwest, no tokio. The IO side (`registry/upstream/`) consumes it.
//!
//! Dependency direction: this crate sits *above* `heart` (the light shared
//! vocabulary). `Language` lives in `heart` (`heart::Language`, re-exported
//! here); this crate provides the per-ecosystem grammar/spec that `heart` must
//! stay free of, so it cannot sit below `heart`.

pub mod archive;
pub mod license;
pub mod manifest;
pub mod name;
pub mod policy;
pub mod repo;
pub mod search;
pub mod upstream;
pub mod version;

#[cfg(test)]
mod name_tests;

pub mod cpp;
mod csharp;
mod go;
mod java;
mod python;
mod rust;
mod ts;

pub use go::{escape_module_path, unescape_module_path};
pub use heart::Language;

use core::marker::PhantomData;

mod sealed {
    /// Exactly the eight per-language ZSTs implement this; nothing outside the
    /// crate can add a ninth.
    pub trait Sealed {}
}

/// One implementation per [`Language`] variant. Everything the registry,
/// mirror, and search pipeline need to know about an ecosystem, in one place.
///
/// Object safety: this trait is NOT object safe (associated types). Runtime
/// dispatch goes through [`DynSpec`], an object-safe erasure implemented
/// blanket-style for every `EcosystemSpec`. Application code should prefer
/// `language.spec()` (returns `&'static dyn DynSpec`); generic code that knows
/// the ecosystem at compile time (tests, per-ecosystem modules) uses the trait
/// directly.
pub trait EcosystemSpec: sealed::Sealed + 'static {
    const LANGUAGE: Language;

    // ── Phase 1: names ──────────────────────────────────────────────────────
    /// Parse a raw package identifier into the structured form. Returns `None`
    /// for names invalid in this ecosystem. MUST be total over its own
    /// registry's population (fixture-tested in Phase 8).
    fn parse_name(raw: &str) -> Option<name::StructuredName>;

    /// Render the canonical (identity) string form. Round-trip law:
    /// `parse_name(&render_canonical(&n)).unwrap().canonical() == n.canonical()`.
    /// MUST be byte-identical to the legacy `canonicalize_*` output (PackageId
    /// is a hash of this string; drift orphans stored packages).
    fn render_canonical(name: &name::StructuredName) -> String;

    // ── Phase 2: versions ───────────────────────────────────────────────────
    type Version: version::VersionGrammar;

    // ── Phase 3: upstream ───────────────────────────────────────────────────
    /// The compression framing of this ecosystem's source archives.
    const ARCHIVE: archive::ArchiveKind;

    /// Static endpoint templates (listing + archive download).
    fn endpoints() -> upstream::UpstreamEndpoints;

    /// Rate-limit / retry / UA policy for this ecosystem's registry.
    const POLICY: policy::UpstreamPolicy;

    /// Parse the registry version-list/metadata body (raw bytes) into versions
    /// WITH listing status. JSON ecosystems call `serde_json::from_slice`
    /// internally; Go parses plain-text newlines; Maven parses XML. This
    /// replaces the `raw_versions` match in `resolve.rs`.
    fn parse_version_listing(body: &[u8]) -> Vec<upstream::ListedVersion<Self::Version>>;

    /// Merge per-version listing status from a secondary registry response
    /// into the already-parsed version list.
    ///
    /// Default: identity (correct for all ecosystems except NuGet). NuGet
    /// overrides this to parse the registration-index JSON and flip entries
    /// whose `catalogEntry.listed == false` to `Withdrawn`.
    ///
    /// IO seam: the IO layer fetches `endpoints().listing`, calls
    /// `parse_version_listing`, then (if `endpoints().listing_status.is_some()`)
    /// fetches the status URL and calls this to apply cross-reference. Parsing
    /// stays pure/unit-testable; the fetch loop stays thin.
    fn merge_listing_status(
        versions: Vec<upstream::ListedVersion<Self::Version>>,
        _status_body: &[u8],
    ) -> Vec<upstream::ListedVersion<Self::Version>> {
        versions
    }

    // ── Phase 4: manifests / facets ─────────────────────────────────────────
    type Manifest: manifest::ManifestFacts;

    /// Which file names inside the extracted archive are the manifest, in
    /// priority order. E.g. `["package.json"]`, `["pyproject.toml", "PKG-INFO"]`.
    fn manifest_candidates() -> &'static [manifest::ManifestCandidate];

    /// Parse one manifest candidate's bytes. `None` on unparseable input —
    /// facet extraction then degrades to name-only, never fails ingest.
    fn parse_manifest(
        candidate: &manifest::ManifestCandidate,
        bytes: &[u8],
    ) -> Option<Self::Manifest>;

    // ── Phase 5/6: search norms ─────────────────────────────────────────────
    fn search_norms() -> &'static search::SearchNorms;

    // ── Phase 6: download source (S4) ───────────────────────────────────────
    /// Where to fetch monthly download counts for this ecosystem, if anywhere.
    ///
    /// `None` means "no download-count endpoint": the `downloads` field will
    /// stay `None` in `SearchFacets` and all downloads-driven ranking stages
    /// will skip this package (fairness floor kicks in instead).
    ///
    /// Default: `None` (safe; ecosystems with a source override this).
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        None
    }

    /// Parse the raw HTTP response body from [`Self::download_source`]'s URL
    /// into a monthly download count.
    ///
    /// Returns `None` on any parse failure; never panics. The default
    /// implementation returns `None` (correct for ecosystems with no endpoint).
    fn parse_download_count(_body: &[u8]) -> Option<u64> {
        None
    }
}

/// The object-safe erasure of [`EcosystemSpec`]: speaks in the type-erased
/// currencies ([`name::StructuredName`], [`version::AnyVersion`],
/// [`manifest::ExtractedFacts`]) so call sites can dispatch on a runtime
/// [`Language`].
pub trait DynSpec: Send + Sync {
    fn language(&self) -> Language;
    fn parse_name(&self, raw: &str) -> Option<name::StructuredName>;
    fn render_canonical(&self, name: &name::StructuredName) -> String;
    fn parse_version(&self, raw: &str) -> Option<version::AnyVersion>;
    /// Order two raw version strings under the ecosystem grammar; `None` when
    /// either fails to parse.
    fn compare_versions(&self, a: &str, b: &str) -> Option<core::cmp::Ordering>;
    fn version_is_prerelease(&self, raw: &str) -> Option<bool>;
    /// Whether `candidate` satisfies the ecosystem-native range `spec`.
    fn range_matches(&self, spec: &str, candidate: &str) -> bool;
    /// Whether `spec` is a well-formed range under the ecosystem grammar.
    fn spec_is_valid(&self, spec: &str) -> bool;
    fn archive(&self) -> archive::ArchiveKind;
    fn endpoints(&self) -> upstream::UpstreamEndpoints;
    fn policy(&self) -> policy::UpstreamPolicy;
    /// Parse registry version-list body (raw bytes) into erased versions with
    /// listing status. JSON/plain-text/XML parsing handled inside each impl.
    fn parse_version_listing(
        &self,
        body: &[u8],
    ) -> Vec<upstream::ListedVersion<version::AnyVersion>>;
    /// Merge secondary listing-status response into the primary version list.
    /// Default: identity. NuGet overrides to flip unlisted entries to Withdrawn.
    fn merge_listing_status(
        &self,
        versions: Vec<upstream::ListedVersion<version::AnyVersion>>,
        status_body: &[u8],
    ) -> Vec<upstream::ListedVersion<version::AnyVersion>>;
    fn manifest_candidates(&self) -> &'static [manifest::ManifestCandidate];
    fn extract_facts(
        &self,
        candidate: &manifest::ManifestCandidate,
        bytes: &[u8],
    ) -> Option<manifest::ExtractedFacts>;
    fn search_norms(&self) -> &'static search::SearchNorms;

    /// Object-safe forwarding for [`EcosystemSpec::download_source`].
    fn download_source(&self) -> Option<upstream::DownloadEndpoint>;

    /// Object-safe forwarding for [`EcosystemSpec::parse_download_count`].
    /// Returns `None` for ecosystems with no download source.
    fn parse_download_count(&self, body: &[u8]) -> Option<u64>;
}

/// Blanket adapter: every [`EcosystemSpec`] is a [`DynSpec`] via a ZST wrapper.
/// (`PhantomData<fn() -> E>` keeps the wrapper `Send + Sync` regardless of `E`,
/// which is only ever a marker ZST.)
struct Erased<E: EcosystemSpec>(PhantomData<fn() -> E>);

impl<E: EcosystemSpec> DynSpec for Erased<E> {
    fn language(&self) -> Language {
        E::LANGUAGE
    }

    fn parse_name(&self, raw: &str) -> Option<name::StructuredName> {
        E::parse_name(raw)
    }

    fn render_canonical(&self, name: &name::StructuredName) -> String {
        E::render_canonical(name)
    }

    fn parse_version(&self, raw: &str) -> Option<version::AnyVersion> {
        use version::VersionGrammar;
        E::Version::parse(raw).map(version::VersionGrammar::erase)
    }

    fn compare_versions(&self, a: &str, b: &str) -> Option<core::cmp::Ordering> {
        use version::VersionGrammar;
        Some(E::Version::parse(a)?.cmp(&E::Version::parse(b)?))
    }

    fn version_is_prerelease(&self, raw: &str) -> Option<bool> {
        use version::VersionGrammar;
        E::Version::parse(raw).map(|v| v.is_prerelease())
    }

    fn range_matches(&self, spec: &str, candidate: &str) -> bool {
        use version::VersionGrammar;
        E::Version::parse(candidate).is_some_and(|v| E::Version::range_matches(spec, &v))
    }

    fn spec_is_valid(&self, spec: &str) -> bool {
        use version::VersionGrammar;
        E::Version::spec_is_valid(spec)
    }

    fn archive(&self) -> archive::ArchiveKind {
        E::ARCHIVE
    }

    fn endpoints(&self) -> upstream::UpstreamEndpoints {
        E::endpoints()
    }

    fn policy(&self) -> policy::UpstreamPolicy {
        E::POLICY
    }

    fn parse_version_listing(
        &self,
        body: &[u8],
    ) -> Vec<upstream::ListedVersion<version::AnyVersion>> {
        use version::VersionGrammar;
        E::parse_version_listing(body)
            .into_iter()
            .map(|lv| upstream::ListedVersion {
                version: lv.version.erase(),
                status: lv.status,
                raw: lv.raw,
            })
            .collect()
    }

    fn merge_listing_status(
        &self,
        versions: Vec<upstream::ListedVersion<version::AnyVersion>>,
        status_body: &[u8],
    ) -> Vec<upstream::ListedVersion<version::AnyVersion>> {
        use version::VersionGrammar;
        // Re-parse raw strings → typed E::Version, run the (possibly overridden)
        // merge, then re-erase back to AnyVersion. Versions that can no longer
        // parse are kept as-is with their original status (defensive).
        let typed: Vec<upstream::ListedVersion<E::Version>> = versions
            .iter()
            .filter_map(|lv| {
                E::Version::parse(lv.raw.as_str()).map(|v| upstream::ListedVersion {
                    version: v,
                    status: lv.status.clone(),
                    raw: lv.raw.clone(),
                })
            })
            .collect();
        let merged = E::merge_listing_status(typed, status_body);
        merged
            .into_iter()
            .map(|lv| upstream::ListedVersion {
                version: lv.version.erase(),
                status: lv.status,
                raw: lv.raw,
            })
            .collect()
    }

    fn manifest_candidates(&self) -> &'static [manifest::ManifestCandidate] {
        E::manifest_candidates()
    }

    fn extract_facts(
        &self,
        candidate: &manifest::ManifestCandidate,
        bytes: &[u8],
    ) -> Option<manifest::ExtractedFacts> {
        use manifest::ManifestFacts;
        E::parse_manifest(candidate, bytes).map(ManifestFacts::into_facts)
    }

    fn search_norms(&self) -> &'static search::SearchNorms {
        E::search_norms()
    }

    fn download_source(&self) -> Option<upstream::DownloadEndpoint> {
        E::download_source()
    }

    fn parse_download_count(&self, body: &[u8]) -> Option<u64> {
        E::parse_download_count(body)
    }
}

/// The dispatch table. New `Language` variants fail to compile until listed
/// here — the exhaustive match, not a fallible lookup, is the totality guard.
pub fn spec(language: Language) -> &'static dyn DynSpec {
    match language {
        Language::Rust => &Erased::<rust::Rust>(PhantomData),
        Language::Typescript => &Erased::<ts::TypeScript>(PhantomData),
        Language::Python => &Erased::<python::Python>(PhantomData),
        Language::Go => &Erased::<go::Go>(PhantomData),
        Language::Java => &Erased::<java::Java>(PhantomData),
        Language::CSharp => &Erased::<csharp::CSharp>(PhantomData),
        Language::Cpp => &Erased::<cpp::Cpp>(PhantomData),
    }
}

/// `language.spec()` — the natural call-site spelling of [`spec`].
pub trait LanguageExt {
    fn spec(self) -> &'static dyn DynSpec;
}

impl LanguageExt for Language {
    fn spec(self) -> &'static dyn DynSpec {
        spec(self)
    }
}

/// Grammar-aware construction / decomposition of `heart::PackageName`.
///
/// The name struct lives in `heart` (the shared vocabulary), but the parse /
/// canonicalize grammar lives here in the spec crate. This extension bridges
/// the two so `heart` need not depend on `ecosystem`.
pub trait PackageNameExt: Sized {
    /// Validate and canonicalize a raw package identifier under `ecosystem`'s
    /// grammar. Returns [`heart::NameError`] for empty / over-length / invalid
    /// names.
    fn new(ecosystem: Language, raw: impl Into<String>) -> Result<Self, heart::NameError>;

    /// Decompose this name into its structured form. Re-parses on demand; valid
    /// by construction (the name passed `parse_name` at creation time).
    fn structured(&self) -> name::StructuredName;
}

impl PackageNameExt for heart::package::PackageName {
    fn new(ecosystem: Language, raw: impl Into<String>) -> Result<Self, heart::NameError> {
        let original: String = raw.into();
        heart::package::PackageName::check_length(ecosystem, &original)?;
        let structured = ecosystem.spec().parse_name(&original).ok_or_else(|| {
            let bad: Vec<char> = original
                .chars()
                .filter(|&c| !heart::package::is_valid_char_for(ecosystem, c))
                .collect();
            if bad.is_empty() {
                heart::NameError::NameHasInvalidChars {
                    ecosystem,
                    invalid_chars: vec!['?'],
                }
            } else {
                heart::NameError::NameHasInvalidChars {
                    ecosystem,
                    invalid_chars: bad,
                }
            }
        })?;
        let canonical = ecosystem.spec().render_canonical(&structured);
        Ok(heart::package::PackageName::from_canonical(
            ecosystem, original, canonical,
        ))
    }

    fn structured(&self) -> name::StructuredName {
        self.ecosystem
            .spec()
            .parse_name(self.original())
            .expect("PackageName::original is always a valid name for its ecosystem")
    }
}

/// Match a [`heart::client::query::PackageSelector`] against a symbol, using the
/// per-ecosystem structured-name grammar. Heart only owns transport/domain data;
/// the matching logic lives here in the index/ecosystem plane.
pub trait PackageSelectorExt {
    /// Whether a symbol plausibly belongs to the selected package.
    ///
    /// Uses `StructuredName::symbol_roots()` so Go module paths, dotted Java
    /// groups, and npm scoped names all match correctly (Q2).
    fn matches(&self, symbol: &heart::Symbol) -> bool;
}

impl PackageSelectorExt for heart::client::query::PackageSelector {
    fn matches(&self, symbol: &heart::Symbol) -> bool {
        symbol.ecosystem == self.name.ecosystem()
            && self.name.structured().symbol_roots().iter().any(|root| {
                let fq = symbol.name.fully_qualified.as_str();
                fq == root
                    || fq
                        .strip_prefix(root.as_str())
                        .is_some_and(|rest| rest.starts_with([':', '.', '/']))
            })
    }
}

/// Match a [`heart::client::query::Filter`] against a symbol. Heart only owns
/// transport/domain data; the matching logic lives here.
pub trait FilterExt {
    /// Whether a symbol survives this filter. `None` dimensions are unbounded.
    fn admits(&self, symbol: &heart::Symbol) -> bool;
}

impl FilterExt for heart::client::query::Filter {
    fn admits(&self, symbol: &heart::Symbol) -> bool {
        self.ecosystems.as_ref().is_none_or(|ecosystems| {
            ecosystems
                .iter()
                .any(|ecosystem| *ecosystem == symbol.ecosystem)
        }) && self
            .packages
            .as_ref()
            .is_none_or(|packages| packages.iter().any(|selector| selector.matches(symbol)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    /// The `spec()` match is the real totality guard; this exercises it for
    /// every variant so a stubbed/panicking table entry cannot land.
    #[test]
    fn spec_table_is_total() {
        for language in Language::iter() {
            assert_eq!(spec(language).language(), language);
        }
    }

    /// Every language's `download_source()` should either be None or return a
    /// non-empty URL template containing `{name}`.
    #[test]
    fn download_source_urls_non_empty_when_some() {
        for language in Language::iter() {
            if let Some(ep) = spec(language).download_source() {
                assert!(
                    !ep.url.is_empty(),
                    "{language:?} download_source URL is empty"
                );
                assert!(
                    ep.url.contains("{name}"),
                    "{language:?} download_source URL missing {{name}} placeholder: {}",
                    ep.url
                );
            }
        }
    }
}
