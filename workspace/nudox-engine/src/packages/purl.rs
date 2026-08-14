//! [`Purl`] — a typed package URL, the one thing a user can *type* that names a
//! package nudox has never seen.
//!
//! # Where this sits relative to the identity model, and why it is not a second one
//!
//! Two documents in this repo say apparently opposite things about PURL:
//!
//! * `docs/GLOBAL-IR-GRAPH.md` §1 — "Package identity **is** the canonical PURL …
//!   canonicalize at ingest, never at query."
//! * `docs/REGISTRYLESS-PLAN.md` RL-8 — "purl and SWHID are pure **renderings**.
//!   Derived from `(stem, version, source_rev)` at export/query time … never
//!   stored as identity."
//!
//! They are not in conflict once you notice they describe different planes.
//! GLOBAL-IR-GRAPH describes the *remote registry graph*, whose keyspace is
//! global and therefore has to be a globally agreed string. RL-8 describes the
//! *registryless catalog plane* (`workspace/index`), where there is no registry
//! to agree with and a purl can only ever be computed from the stem.
//!
//! The plane this crate serves is neither: `nudox-store` keys packages on
//! `PackageLineageId = (EcosystemId, PackageName)`, with the version carried
//! *beside* it in [`crate::versions::VersionRegistry`]. That is the identity
//! this feature must not fork.
//!
//! So `Purl` is deliberately **an input parser and a rendering, never an
//! identity**:
//!
//! ```text
//!   "pkg:cargo/serde@1.0.196"
//!        │  parse + canonicalize (once, here, at ingest)
//!        ▼
//!   Purl { ty: Cargo, namespace: None, name: "serde", version: Some("1.0.196") }
//!        │  Purl::lineage_name() + Purl::ecosystem() + Purl::language()
//!        ▼
//!   PackageSpec { name: "serde", version: "1.0.196", language: Rust, root }
//!        │  (the existing seam — unchanged)
//!        ▼
//!   PackageDescriptor::cargo(...) → PackageLineageId("cargo", "serde")
//! ```
//!
//! Nothing downstream of `PackageSpec` learns that a PURL was involved. A
//! package indexed from `pkg:cargo/serde@1.0.196` and the same package listed
//! in `nix/corpus.nix` produce the *same* `PackageLineageId` and land in
//! the same lineage — which is the whole point of not adding a second
//! vocabulary. [`Purl::render`] goes back the other way for display.
//!
//! # Where this deviates from purl-spec, deliberately
//!
//! purl-spec's canonicalization rules say the `golang` type "must be
//! lowercased". That rule is **wrong for Go** and following it would break this
//! feature outright: Go module paths are case-sensitive
//! (`github.com/BurntSushi/toml`), which is precisely why the module proxy
//! defines the `!`-escape in `golang.org/x/mod/module.EscapePath` — an escape
//! that exists only because case is significant. Lowercasing at parse time
//! would send `pkg:golang/github.com/BurntSushi/toml@v1.3.2` to
//! `proxy.golang.org/github.com/burntsushi/toml/@v/v1.3.2.zip`, a 404 reported
//! as "no such package" about a package that exists. Case is preserved for
//! `golang`, and [`Purl::render`] round-trips it.
//!
//! Similarly, `nuget` names are *not* lowercased here even though purl-spec
//! says to. The corpus already holds `nuget:AutoMapper`, `nuget:CsvHelper` and
//! twenty more in their published casing (see `nix/corpus.nix`), so
//! lowercasing at ingest would give one package two `PackageLineageId`s —
//! exactly the keyspace fragmentation GLOBAL-IR-GRAPH §1 warns about, arrived
//! at by obeying the rule instead of by ignoring it. The lowercasing that
//! `api.nuget.org`'s flat container genuinely requires happens in
//! [`crate::packages::acquire`], on the URL, where it belongs.

use std::fmt;

use crate::ProducerLanguage;

// ---------------------------------------------------------------------------
// PurlType
// ---------------------------------------------------------------------------

/// The `type` component of a PURL — the ecosystem the name is resolved in.
///
/// One variant per ecosystem this build can actually fetch *and* produce IR
/// for, which is why there is no `Cpp`/`generic`/`github` variant: `cpp`
/// packages have no URL convention at all (`nix build .#checks.corpus` requires an
/// explicit per-version `url` for every one of them, because GitHub's
/// auto-generated tag tarballs are not byte-stable), so a `pkg:generic/...`
/// input could be parsed but never resolved. An enum that can name a thing the
/// resolver cannot serve would move that failure from parse time to network
/// time and describe it as a 404.
///
/// `#[non_exhaustive]` because adding an ecosystem must not break `lindsey`'s
/// `match` arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PurlType {
    /// `pkg:cargo/<name>@<version>` — crates.io.
    Cargo,
    /// `pkg:npm/[@scope/]<name>@<version>` — registry.npmjs.org.
    Npm,
    /// `pkg:pypi/<name>@<version>` — pypi.org.
    PyPi,
    /// `pkg:golang/<module path>@<version>` — proxy.golang.org.
    Golang,
    /// `pkg:maven/<groupId>/<artifactId>@<version>` — repo1.maven.org.
    Maven,
    /// `pkg:nuget/<id>@<version>` — api.nuget.org.
    NuGet,
}

impl PurlType {
    /// Every type this build understands, in the order they are listed to a
    /// user who typed one that is not among them.
    pub const ALL: [PurlType; 6] = [
        PurlType::Cargo,
        PurlType::Npm,
        PurlType::PyPi,
        PurlType::Golang,
        PurlType::Maven,
        PurlType::NuGet,
    ];

    /// The purl-spec type string (`"cargo"`, `"golang"`, …).
    ///
    /// Note this is *not* the ecosystem id: purl says `golang`, the store says
    /// `go`. Keeping the two spellings in two methods rather than one shared
    /// constant is deliberate — they answer to different authorities and a
    /// future divergence must not be silently resolved by whichever caller
    /// happens to read the shared one.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::PyPi => "pypi",
            Self::Golang => "golang",
            Self::Maven => "maven",
            Self::NuGet => "nuget",
        }
    }

    /// The `EcosystemId` string this type maps onto in `nudox-store`.
    ///
    /// This is the mapping that keeps a PURL-indexed package in the *same*
    /// lineage as a manifest-provisioned one. `golang` → `"go"` is the only
    /// place the two vocabularies disagree, and it is why this is a method and
    /// not [`Self::as_str`].
    pub fn ecosystem(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::PyPi => "pypi",
            Self::Golang => "go",
            Self::Maven => "maven",
            Self::NuGet => "nuget",
        }
    }

    /// The producer that lowers packages of this type.
    ///
    /// Total, and that is the point: pairing an ecosystem with the wrong
    /// language builds a `PackageDescriptor` no registered producer answers,
    /// which surfaces at runtime as `ToolchainMissing` rather than as a
    /// compile error (see `PackageDescriptor::new`'s doc comment on exactly
    /// this trap).
    pub fn language(self) -> ProducerLanguage {
        match self {
            Self::Cargo => ProducerLanguage::Rust,
            Self::Npm => ProducerLanguage::TypeScript,
            Self::PyPi => ProducerLanguage::Python,
            Self::Golang => ProducerLanguage::Go,
            Self::Maven => ProducerLanguage::Java,
            Self::NuGet => ProducerLanguage::CSharp,
        }
    }

    /// The human name of the registry this type resolves against, for error
    /// messages that must say *where* we looked.
    pub fn registry(self) -> &'static str {
        match self {
            Self::Cargo => "crates.io",
            Self::Npm => "the npm registry",
            Self::PyPi => "PyPI",
            Self::Golang => "the Go module proxy",
            Self::Maven => "Maven Central",
            Self::NuGet => "nuget.org",
        }
    }

    /// Does this type's PURL carry a namespace, and what is it called there?
    ///
    /// Used by the parser's error messages so "you left out the namespace"
    /// names the thing the user actually has to supply (`groupId`, not
    /// "namespace").
    fn namespace_role(self) -> NamespaceRole {
        match self {
            // `@scope` is optional on npm and there is no other meaning for a
            // leading path segment.
            Self::Npm => NamespaceRole::Optional,
            // A Maven coordinate without a groupId is not a coordinate.
            Self::Maven => NamespaceRole::Required("groupId"),
            // A Go module path is host + path; the split into namespace/name is
            // purl bookkeeping, and we re-join it immediately.
            Self::Golang => NamespaceRole::Required("module path prefix"),
            Self::Cargo | Self::PyPi | Self::NuGet => NamespaceRole::Forbidden,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        // Type is case-insensitive per purl-spec and is lowercased on
        // canonicalization.
        let lower = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|t| t.as_str() == lower)
    }
}

impl fmt::Display for PurlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a [`PurlType`] takes a namespace component, and what that component
/// is called in the ecosystem's own vocabulary.
enum NamespaceRole {
    /// Absent is an error, and the message names this component the way the
    /// ecosystem does (`groupId`, not "namespace").
    Required(&'static str),
    /// Present or absent are both valid, so there is no message to write and
    /// nothing to name — an npm package is `left-pad` or `@types/node` and
    /// neither form is a mistake.
    Optional,
    /// Present is an error.
    Forbidden,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Why a string is not a PURL this build can act on.
///
/// Separate from `crate::packages::acquire::Error` because these are decidable
/// without touching the network: everything here is a property of the string
/// itself. Mixing them would make "you typed it wrong" and "the registry does
/// not have it" the same class of failure, which is the single most useful
/// distinction this feature draws.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No `pkg:` scheme.
    #[error("not a package URL: {input:?} does not start with 'pkg:'")]
    NotAPurl {
        /// The rejected input, echoed back.
        input: String,
    },

    /// The scheme is present but the type is empty or unknown.
    #[error("unknown package type {ty:?}; nudox can fetch {}", known_types())]
    UnknownType {
        /// The type as written.
        ty: String,
    },

    /// The name component is empty.
    #[error("package URL has no name: {input:?}")]
    EmptyName {
        /// The rejected input, echoed back.
        input: String,
    },

    /// A namespace was required and absent, or present and meaningless.
    #[error("{message}")]
    Namespace {
        /// Fully rendered because the correct wording depends on all three of
        /// type, role and direction, and a caller only ever prints it.
        message: String,
    },

    /// A `?key=value` qualifier was present.
    ///
    /// Rejected rather than ignored. `repository_url=` in particular
    /// *redirects the fetch to a different registry*; honouring the name and
    /// silently dropping the location would download a different package that
    /// happens to share a name — the exact "apparent success" failure mode
    /// `nix build .#checks.corpus`'s header warns about, arrived at without a hash
    /// mismatch to notice.
    #[error("package URL qualifier {key:?} is not supported; nudox resolves every package against its ecosystem's canonical registry and follows no caller-supplied location")]
    UnsupportedQualifier {
        /// The qualifier key that was present.
        key: String,
    },

    /// A `#subpath` was present.
    #[error("package URL subpath {subpath:?} is not supported; nudox indexes whole packages, not directories inside them")]
    UnsupportedSubpath {
        /// The subpath as written.
        subpath: String,
    },

    /// Percent-decoding produced bytes that are not UTF-8.
    #[error("package URL component {component} is not valid UTF-8 after percent-decoding")]
    NotUtf8 {
        /// Which component failed to decode.
        component: &'static str,
    },
}

fn known_types() -> String {
    PurlType::ALL
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Purl
// ---------------------------------------------------------------------------

/// A parsed, canonicalized package URL.
///
/// Constructed only by [`Purl::parse`], so every value of this type has already
/// been canonicalized — "canonicalize at ingest, never at query"
/// (GLOBAL-IR-GRAPH §1) is enforced by there being no other constructor and no
/// public fields to write after the fact.
///
/// The version is `Option` because purl-spec makes it optional, not because
/// nudox can act on a versionless one: [`crate::packages::acquire`] rejects `None` with a
/// message that lists the versions the registry actually publishes. Storing it
/// as `Option` and failing later — rather than refusing to parse — is what lets
/// that failure carry the list, which is the only genuinely useful thing to say
/// about a missing version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Purl {
    ty: PurlType,
    namespace: Option<String>,
    name: String,
    version: Option<String>,
}

impl Purl {
    /// Parse and canonicalize a package URL.
    ///
    /// Accepts the `pkg:type/namespace/name@version` form. Qualifiers and
    /// subpaths parse but are rejected — see [`Error`] for why
    /// rejecting beats ignoring.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let trimmed = input.trim();

        // Scheme. purl-spec permits `pkg://type/...` from older tooling; the
        // extra slashes carry no meaning and are stripped rather than rejected,
        // because a user pasting one has made no mistake we can help with.
        let rest = trimmed
            .strip_prefix("pkg:")
            .or_else(|| trimmed.strip_prefix("PKG:"))
            .ok_or_else(|| Error::NotAPurl {
                input: trimmed.to_owned(),
            })?
            .trim_start_matches('/');

        // Subpath, then qualifiers — in that order, because a subpath may
        // legally contain '?' after percent-encoding but a qualifier value may
        // not contain an unencoded '#'.
        let (rest, subpath) = split_once_opt(rest, '#');
        if let Some(subpath) = subpath.filter(|s| !s.is_empty()) {
            return Err(Error::UnsupportedSubpath {
                subpath: subpath.to_owned(),
            });
        }
        let (rest, qualifiers) = split_once_opt(rest, '?');
        if let Some(qualifiers) = qualifiers.filter(|q| !q.is_empty()) {
            let key = qualifiers
                .split('&')
                .next()
                .unwrap_or(qualifiers)
                .split('=')
                .next()
                .unwrap_or(qualifiers);
            return Err(Error::UnsupportedQualifier {
                key: key.to_owned(),
            });
        }

        // Type.
        let (ty_str, rest) = rest.split_once('/').unwrap_or((rest, ""));
        let ty = PurlType::parse(ty_str).ok_or_else(|| Error::UnknownType {
            ty: ty_str.to_owned(),
        })?;

        // Version. The '@' that opens it is the *last* one, so an npm scope
        // (`@scope/name`) does not eat it.
        let (path, version) = match rest.rfind('@') {
            // A leading '@' at position 0 is an npm scope, not a version.
            Some(0) | None => (rest, None),
            Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        };

        // Namespace / name.
        let (namespace, name) = match path.rfind('/') {
            Some(slash) => (Some(&path[..slash]), &path[slash + 1..]),
            None => (None, path),
        };

        let name = percent_decode(name, "name")?;
        if name.is_empty() {
            return Err(Error::EmptyName {
                input: trimmed.to_owned(),
            });
        }
        let namespace = match namespace {
            Some(ns) if !ns.is_empty() => Some(percent_decode(ns, "namespace")?),
            _ => None,
        };
        let version = match version {
            Some(v) if !v.is_empty() => Some(percent_decode(v, "version")?),
            _ => None,
        };

        // Namespace arity, per type.
        match (ty.namespace_role(), &namespace) {
            (NamespaceRole::Required(role), None) => {
                return Err(Error::Namespace {
                    message: format!(
                        "a {ty} package URL needs a {role}: expected 'pkg:{ty}/<{role}>/{name}@<version>', got no '/' before the name",
                    ),
                });
            }
            (NamespaceRole::Forbidden, Some(ns)) => {
                return Err(Error::Namespace {
                    message: format!(
                        "a {ty} package URL has no namespace component, but {ns:?} was given before the name; {ty} package names are a single segment",
                    ),
                });
            }
            _ => {}
        }

        Ok(Self {
            ty,
            namespace,
            name,
            version,
        }
        .canonicalized())
    }

    /// Apply the per-type canonicalization rules.
    ///
    /// Private and applied unconditionally in [`Self::parse`], so an
    /// un-canonicalized `Purl` cannot exist.
    fn canonicalized(mut self) -> Self {
        match self.ty {
            // purl-spec: npm names are lowercased. This is true of the registry
            // too — npm rejects new packages differing only in case.
            //
            // The `@` is stripped from the scope here and re-added by
            // `render`/`lineage_name`, so the sigil lives in exactly one place.
            // Storing it *inside* the namespace was the first attempt and it
            // produced `@@types/node`: two independent pieces of code each
            // believing they owned the character.
            PurlType::Npm => {
                self.name = self.name.to_ascii_lowercase();
                self.namespace = self
                    .namespace
                    .map(|ns| ns.trim_start_matches('@').to_ascii_lowercase())
                    .filter(|ns| !ns.is_empty());
            }
            // PEP 503 normalization: lowercase, and runs of `-_.` collapse to a
            // single `-`. PyPI's own JSON API accepts the normalized form and
            // redirects to it, so this is the spelling that resolves.
            PurlType::PyPi => self.name = normalize_pypi(&self.name),
            // Case preserved for golang and nuget — see the module docs for the
            // two separate reasons.
            PurlType::Golang | PurlType::NuGet | PurlType::Cargo | PurlType::Maven => {}
        }
        self
    }

    /// The ecosystem this package is resolved in.
    pub fn ty(&self) -> PurlType {
        self.ty
    }

    /// The namespace component: an npm `@scope` (without the `@`), a Maven
    /// `groupId`, or a Go module path prefix.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// The name component, as the ecosystem spells it.
    ///
    /// For Maven this is the bare `artifactId` and for Go the last path
    /// segment — neither is the name the corpus keys on. Use
    /// [`Self::lineage_name`] for that.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version, if the PURL carried one.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The same package at a specific version.
    ///
    /// Used by the resolver to answer a versionless PURL by naming the
    /// versions that exist, and by tests.
    pub fn with_version(&self, version: impl Into<String>) -> Self {
        Self {
            version: Some(version.into()),
            ..self.clone()
        }
    }

    /// The `EcosystemId` string this package's lineage uses.
    pub fn ecosystem(&self) -> &'static str {
        self.ty.ecosystem()
    }

    /// The producer language for this package.
    pub fn language(&self) -> ProducerLanguage {
        self.ty.language()
    }

    /// The `PackageName` this package occupies in the corpus — the *one* string
    /// that has to agree with what `nix/corpus.nix` and
    /// `nix build .#checks.corpus` already use, or a PURL-indexed package would land in a
    /// different lineage than the same package provisioned from the manifest.
    ///
    /// | type   | lineage name                | manifest example              |
    /// |--------|-----------------------------|-------------------------------|
    /// | cargo  | `serde`                     | `name = "serde"`              |
    /// | npm    | `@types/node`               | `name = "@types/node"`        |
    /// | pypi   | `attrs`                     | `name = "attrs"`              |
    /// | golang | `github.com/pkg/errors`     | `name = "github.com/pkg/errors"` |
    /// | maven  | `com.google.guava:guava`    | `name = "com.google.guava:guava"` |
    /// | nuget  | `AutoMapper`                | `name = "AutoMapper"`         |
    ///
    /// The Maven `groupId:artifactId` join is not cosmetic: `resolve-url` in
    /// `nix build .#checks.corpus` splits on exactly that colon to build the repo path,
    /// and `JavaProducer` reads the same coordinate back out.
    pub fn lineage_name(&self) -> String {
        match (self.ty, &self.namespace) {
            (PurlType::Maven, Some(group)) => format!("{group}:{}", self.name),
            (PurlType::Npm, Some(scope)) => format!("@{scope}/{}", self.name),
            (PurlType::Golang, Some(prefix)) => format!("{prefix}/{}", self.name),
            // Maven's `None` case is unreachable (the parser requires a
            // groupId); the others genuinely have no namespace.
            _ => self.name.clone(),
        }
    }

    /// Render back to canonical PURL text.
    ///
    /// Round-trips: `Purl::parse(&p.render()) == Ok(p)` for every `p` this
    /// module can produce. That property is what makes it safe to *store* the
    /// lineage and *display* the purl — RL-8's "purl is a rendering" applied to
    /// this plane.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(32);
        out.push_str("pkg:");
        out.push_str(self.ty.as_str());
        out.push('/');
        if let Some(ns) = &self.namespace {
            // npm scopes render as `@scope/name`; purl-spec encodes the `@` as
            // part of the namespace only in the "with leading @" reading, and
            // the round-trip test in this module pins whichever we chose.
            if self.ty == PurlType::Npm {
                out.push('@');
            }
            out.push_str(ns);
            out.push('/');
        }
        out.push_str(&self.name);
        if let Some(v) = &self.version {
            out.push('@');
            out.push_str(v);
        }
        out
    }

    /// A filesystem-safe directory name for this package's unpacked root.
    ///
    /// Mirrors `safe-dir-name` in `nix build .#checks.corpus` — package names contain `/`
    /// (Go modules, scoped npm) and `:` (Maven coordinates), neither of which
    /// may appear in a single path component. Kept byte-identical to that
    /// function's output so a PURL fetch and a manifest fetch of the same
    /// package produce the same directory name and the second one is a cache
    /// hit rather than a duplicate tree.
    pub fn cache_dir_name(&self) -> String {
        let clean = self.lineage_name().replace('/', "__").replace(':', "__");
        match &self.version {
            Some(v) => format!("{clean}-{v}"),
            None => clean,
        }
    }
}

impl fmt::Display for Purl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::str::FromStr for Purl {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn split_once_opt(s: &str, sep: char) -> (&str, Option<&str>) {
    match s.split_once(sep) {
        Some((head, tail)) => (head, Some(tail)),
        None => (s, None),
    }
}

/// Percent-decode one PURL component.
///
/// Hand-rolled rather than pulled from `percent-encoding`: this crate has no
/// URL dependency today and one three-line decoder is a smaller cost than a new
/// edge in the dependency graph of the crate `lindsey` links.
fn percent_decode(s: &str, component: &'static str) -> Result<String, Error> {
    if !s.contains('%') {
        return Ok(s.to_owned());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).map_err(|_| Error::NotUtf8 { component })
}

/// PEP 503 name normalization: lowercase, and any run of `-`, `_` or `.`
/// becomes a single `-`.
fn normalize_pypi(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !last_was_sep {
                out.push('-');
                last_was_sep = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            last_was_sep = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_cargo_purl_yields_the_lineage_the_manifest_would_have_produced() {
        let p = Purl::parse("pkg:cargo/serde@1.0.196").expect("valid purl");
        assert_eq!(p.ty(), PurlType::Cargo);
        assert_eq!(p.lineage_name(), "serde");
        assert_eq!(p.ecosystem(), "cargo");
        assert_eq!(p.version(), Some("1.0.196"));
        assert_eq!(p.language(), ProducerLanguage::Rust);
    }

    #[test]
    fn the_golang_type_maps_onto_the_go_ecosystem_id_not_its_own_spelling() {
        // The one place the purl vocabulary and the store's vocabulary
        // disagree. A package indexed from a purl must land in the same
        // lineage as one provisioned from `nix/corpus.nix`, which says
        // `ecosystem = "go"`.
        let p = Purl::parse("pkg:golang/github.com/pkg/errors@v0.9.1").expect("valid purl");
        assert_eq!(p.ty().as_str(), "golang");
        assert_eq!(p.ecosystem(), "go");
        assert_eq!(p.lineage_name(), "github.com/pkg/errors");
    }

    #[test]
    fn go_module_paths_keep_their_case_against_the_purl_spec_lowercasing_rule() {
        // purl-spec says lowercase `golang`. Following it would make this
        // module unfetchable, because the proxy's `!`-escape exists precisely
        // because case is significant. See the module docs.
        let p = Purl::parse("pkg:golang/github.com/BurntSushi/toml@v1.3.2").expect("valid purl");
        assert_eq!(p.lineage_name(), "github.com/BurntSushi/toml");
    }

    #[test]
    fn nuget_ids_keep_the_casing_the_corpus_already_uses() {
        // `nix/corpus.nix` holds `name = "AutoMapper"`. Lowercasing here
        // would give one package two `PackageLineageId`s.
        let p = Purl::parse("pkg:nuget/AutoMapper@13.0.1").expect("valid purl");
        assert_eq!(p.lineage_name(), "AutoMapper");
    }

    #[test]
    fn a_maven_purl_joins_group_and_artifact_the_way_fetch_nu_splits_them() {
        let p = Purl::parse("pkg:maven/com.google.guava/guava@33.0.0-jre").expect("valid purl");
        assert_eq!(p.lineage_name(), "com.google.guava:guava");
        assert_eq!(p.namespace(), Some("com.google.guava"));
        assert_eq!(p.name(), "guava");
    }

    #[test]
    fn an_npm_scope_is_not_mistaken_for_a_version_marker() {
        let p = Purl::parse("pkg:npm/@types/node@20.11.0").expect("valid purl");
        assert_eq!(p.lineage_name(), "@types/node");
        assert_eq!(p.version(), Some("20.11.0"));
    }

    #[test]
    fn an_unscoped_npm_name_is_lowercased_per_the_registrys_own_rule() {
        let p = Purl::parse("pkg:npm/LeftPad@1.0.0").expect("valid purl");
        assert_eq!(p.lineage_name(), "leftpad");
    }

    #[test]
    fn pypi_names_are_pep503_normalized_so_the_json_api_resolves_them() {
        let p = Purl::parse("pkg:pypi/Typing_Extensions@4.9.0").expect("valid purl");
        assert_eq!(p.lineage_name(), "typing-extensions");
    }

    #[test]
    fn every_parseable_purl_round_trips_through_render() {
        for input in [
            "pkg:cargo/serde@1.0.196",
            "pkg:npm/@types/node@20.11.0",
            "pkg:npm/left-pad@1.3.0",
            "pkg:pypi/attrs@23.2.0",
            "pkg:golang/github.com/pkg/errors@v0.9.1",
            "pkg:maven/com.google.guava/guava@33.0.0-jre",
            "pkg:nuget/AutoMapper@13.0.1",
            "pkg:cargo/serde",
        ] {
            let parsed = Purl::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            let rendered = parsed.render();
            let reparsed = Purl::parse(&rendered)
                .unwrap_or_else(|e| panic!("{input} rendered as {rendered}: {e}"));
            assert_eq!(parsed, reparsed, "{input} did not round-trip via {rendered}");
        }
    }

    #[test]
    fn a_qualifier_is_refused_rather_than_ignored() {
        // Honouring the name and dropping `repository_url` would fetch a
        // *different package that happens to share a name* and report success.
        let err = Purl::parse("pkg:maven/com.example/thing@1.0?repository_url=https://internal")
            .expect_err("qualifiers must not be silently dropped");
        assert_eq!(
            err,
            Error::UnsupportedQualifier {
                key: "repository_url".to_owned()
            }
        );
    }

    #[test]
    fn a_subpath_is_refused_because_nudox_indexes_whole_packages() {
        let err = Purl::parse("pkg:golang/github.com/pkg/errors@v0.9.1#internal/x")
            .expect_err("subpaths must be refused");
        assert!(matches!(err, Error::UnsupportedSubpath { .. }));
    }

    #[test]
    fn a_maven_purl_without_a_group_id_says_which_component_is_missing() {
        let err = Purl::parse("pkg:maven/guava@33.0.0-jre").expect_err("groupId is required");
        let text = err.to_string();
        assert!(text.contains("groupId"), "{text}");
        assert!(text.contains("pkg:maven/<groupId>/guava"), "{text}");
    }

    #[test]
    fn a_cargo_purl_with_a_namespace_is_refused_instead_of_silently_dropping_it() {
        let err = Purl::parse("pkg:cargo/some-org/serde@1.0.196")
            .expect_err("cargo has no namespace component");
        let text = err.to_string();
        assert!(text.contains("no namespace component"), "{text}");
    }

    #[test]
    fn an_unknown_type_lists_the_ones_that_work() {
        let err = Purl::parse("pkg:gem/rails@7.0.0").expect_err("gem is not fetchable here");
        let text = err.to_string();
        for expected in ["cargo", "npm", "pypi", "golang", "maven", "nuget"] {
            assert!(text.contains(expected), "{text} should list {expected}");
        }
    }

    #[test]
    fn a_plain_search_query_is_not_mistaken_for_a_purl() {
        // The omni-search calls this on every keystroke; anything that is not
        // unambiguously a purl must fall through to being a search query.
        for not_a_purl in ["serde", "Vec::push", "pkg", "package:serde", "http://x/y"] {
            assert!(
                matches!(
                    Purl::parse(not_a_purl),
                    Err(Error::NotAPurl { .. })
                ),
                "{not_a_purl:?} must not parse as a purl"
            );
        }
    }

    #[test]
    fn percent_encoded_components_decode_before_canonicalization() {
        let p = Purl::parse("pkg:maven/com.google.guava/guava@33.0.0%2Bjre").expect("valid purl");
        assert_eq!(p.version(), Some("33.0.0+jre"));
    }

    #[test]
    fn the_cache_directory_name_matches_fetch_nus_safe_dir_name() {
        // `safe-dir-name` in nix build .#checks.corpus: '/' and ':' both become '__',
        // then '-<version>'. Byte-identical output means a purl fetch of a
        // package the manifest already provisioned is a cache hit.
        assert_eq!(
            Purl::parse("pkg:golang/github.com/pkg/errors@v0.9.1")
                .unwrap()
                .cache_dir_name(),
            "github.com__pkg__errors-v0.9.1",
        );
        assert_eq!(
            Purl::parse("pkg:maven/com.google.guava/guava@33.0.0-jre")
                .unwrap()
                .cache_dir_name(),
            "com.google.guava__guava-33.0.0-jre",
        );
        assert_eq!(
            Purl::parse("pkg:cargo/serde@1.0.196").unwrap().cache_dir_name(),
            "serde-1.0.196",
        );
    }

    #[test]
    fn every_purl_type_pairs_with_the_language_its_ecosystem_actually_produces() {
        // Pairing an ecosystem with the wrong language builds a descriptor no
        // registered producer answers, which surfaces at runtime as
        // `ToolchainMissing` rather than as a compile error.
        let expected = [
            (PurlType::Cargo, ProducerLanguage::Rust, "cargo"),
            (PurlType::Npm, ProducerLanguage::TypeScript, "npm"),
            (PurlType::PyPi, ProducerLanguage::Python, "pypi"),
            (PurlType::Golang, ProducerLanguage::Go, "go"),
            (PurlType::Maven, ProducerLanguage::Java, "maven"),
            (PurlType::NuGet, ProducerLanguage::CSharp, "nuget"),
        ];
        for (ty, language, ecosystem) in expected {
            assert_eq!(ty.language(), language, "{ty}");
            assert_eq!(ty.ecosystem(), ecosystem, "{ty}");
        }
        assert_eq!(expected.len(), PurlType::ALL.len(), "a type was added without a pairing");
    }
}
