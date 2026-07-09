//! The linker: the resolution pass that turns the IR's name-strings into
//! symbol IRIs, so every relation in the graph is a real link.
//!
//! Resolution is **total**: a name that matches no index entry still yields an
//! IRI — a stub [`model::Symbol`] (`kind: Unresolved`, `resolved: false`) is
//! minted under a deterministic address, so edges never dangle and cross
//! package boundaries. When the defining package is ingested later under the
//! same identity scheme, its real node lands at a knowable address.
//!
//! Lookup order: exact fq path → alias → unique last-segment suffix → stub.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use ir::entry::{Index, NudoxPath};
use terminusdb_schema::{EntityIDFor, TdbLazy};

use super::model as m;

/// The pseudo-package for names whose owning package is unknown (bare
/// identifiers in signatures that resolve nowhere). References with a known
/// dependency (`NudoxPath::External`) go under the real package name instead.
pub const EXTERN_PACKAGE: &str = "~extern";

/// Coordinates of the package being projected.
#[derive(Debug, Clone)]
pub struct PackageCtx {
    pub language: String,
    pub package: String,
    pub version: Option<String>,
}

/// Keep IRIs to a conservative charset so client-minted ids are always valid
/// TerminusDB document ids. `::` survives (the legacy `Entry/...` URIs proved
/// it); everything exotic collapses to `_`.
fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '~') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hierarchical coordinate → single id segment.
///
/// [`EntityIDFor`] only accepts `TypeName/id` with a **single** id segment;
/// extra `/` is parsed as a subdocument path and the type check fails
/// (`expected 'Symbol', found 'pkg'`). The logical hierarchy is therefore
/// encoded with `%2F` so the wire form stays one segment while remaining
/// reversible via [`decode_id_segment`].
fn encode_id_segment(parts: &[&str]) -> String {
    parts.iter().map(|p| sanitize(p)).collect::<Vec<_>>().join("%2F")
}

/// Reverse of [`encode_id_segment`].
pub fn decode_id_segment(encoded: &str) -> Vec<String> {
    encoded.split("%2F").map(str::to_string).collect()
}

/// `Symbol/{language}%2F{package}%2F{fq_path}` — the one place symbol IRIs are
/// minted. Logical hierarchy is `Symbol/{lang}/{package}/{fq}`; the `%2F`
/// encoding is required by EntityIDFor (see [`encode_id_segment`]).
pub fn symbol_iri(language: &str, package: &str, fq_name: &str) -> String {
    format!(
        "Symbol/{}",
        encode_id_segment(&[language, package, fq_name])
    )
}

/// `Package/{language}%2F{name}`.
pub fn package_iri(language: &str, package: &str) -> String {
    format!("Package/{}", encode_id_segment(&[language, package]))
}

/// `PackageVersion/{language}%2F{name}@{version}`.
pub fn package_version_iri(language: &str, package: &str, version: &str) -> String {
    format!(
        "PackageVersion/{}",
        encode_id_segment(&[language, &format!("{}@{}", package, version)])
    )
}

/// `(owning package, path segments)` of a [`NudoxPath`], relative to the
/// package being projected.
fn coordinates<'c>(path: &NudoxPath, ctx: &'c PackageCtx) -> (String, Vec<String>) {
    let comps = |pb: &std::path::Path| {
        pb.iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    match path {
        NudoxPath::Local(pb) => (ctx.package.clone(), comps(pb)),
        NudoxPath::External { path, dependency } => (dependency.clone(), comps(path)),
    }
}

/// A resolved symbol address plus its display metadata (used for stubs).
struct StubSeed {
    package: String,
    segments: Vec<String>,
}

pub struct Linker {
    ctx: PackageCtx,
    /// fq name (and every alias spelling) → IRI, for entries in the index.
    exact: HashMap<String, String>,
    /// Every IRI that belongs to a real index entry.
    known: std::collections::HashSet<String>,
    /// last path segment → IRI, iff unambiguous across the index.
    suffix: HashMap<String, Option<String>>,
    /// child IRI → parent IRI, inverted from the IR's parent-side `members`.
    parent_of: HashMap<String, String>,
    /// Stub symbols minted during resolution, keyed by IRI.
    stubs: RefCell<BTreeMap<String, StubSeed>>,
    /// Packages referenced by stubs (they need bare `Package` nodes too).
    extern_packages: RefCell<BTreeSet<String>>,
}

impl Linker {
    pub fn build(index: &Index, ctx: PackageCtx) -> Self {
        let mut exact = HashMap::new();
        let mut known = std::collections::HashSet::new();
        let mut suffix: HashMap<String, Option<String>> = HashMap::new();
        let mut parent_of = HashMap::new();

        let mut note_suffix = |name: &str, iri: &str| match suffix.entry(name.to_string()) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Some(iri.to_string()));
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if o.get().as_deref() != Some(iri) {
                    *o.get_mut() = None; // ambiguous
                }
            }
        };

        for (path, entry) in &index.entries_by_path {
            let (package, segments) = coordinates(path, &ctx);
            let fq = segments.join("::");
            let iri = symbol_iri(&ctx.language, &package, &fq);

            exact.insert(fq, iri.clone());
            known.insert(iri.clone());
            if let Some(last) = segments.last() {
                note_suffix(last, &iri);
            }
            if let Some(aliases) = entry.aliases() {
                for alias in aliases {
                    exact.insert(alias.join("::"), iri.clone());
                    if let Some(last) = alias.last() {
                        note_suffix(last, &iri);
                    }
                }
            }

            let members = match entry {
                ir::kind::Entry::Module(s) => s.inner.members.as_deref(),
                ir::kind::Entry::RecordType(s) => s.inner.members.as_deref(),
                ir::kind::Entry::TraitDef(s) => s.inner.members.as_deref(),
                ir::kind::Entry::TraitImpl(s) => s.inner.members.as_deref(),
                ir::kind::Entry::Function(s) => s.inner.members.as_deref(),
                _ => None,
            };
            for member in members.unwrap_or(&[]) {
                let (mp, msegs) = coordinates(member, &ctx);
                let member_iri = symbol_iri(&ctx.language, &mp, &msegs.join("::"));
                // First parent wins; extra parents are re-export style aliases.
                parent_of.entry(member_iri).or_insert_with(|| iri.clone());
            }
        }

        Self {
            ctx,
            exact,
            known,
            suffix,
            parent_of,
            stubs: RefCell::new(BTreeMap::new()),
            extern_packages: RefCell::new(BTreeSet::new()),
        }
    }

    pub fn ctx(&self) -> &PackageCtx {
        &self.ctx
    }

    /// The projected symbol's own IRI (also resolves External index entries).
    pub fn iri_of(&self, path: &NudoxPath) -> String {
        let (package, segments) = coordinates(path, &self.ctx);
        symbol_iri(&self.ctx.language, &package, &segments.join("::"))
    }

    /// The containment parent of `iri`, if any entry listed it as a member.
    pub fn parent_of(&self, iri: &str) -> Option<&str> {
        self.parent_of.get(iri).map(String::as_str)
    }

    /// Resolve a full path reference (members, implemented protocols, body
    /// reference targets). The path carries its own coordinates, so this never
    /// falls back to `~extern` — but it still mints a stub if the target is
    /// not an entry of this index.
    pub fn resolve_path(&self, path: &NudoxPath) -> String {
        let (package, segments) = coordinates(path, &self.ctx);
        let iri = symbol_iri(&self.ctx.language, &package, &segments.join("::"));
        if !self.known.contains(&iri) {
            self.note_stub(&iri, package, segments);
        }
        iri
    }

    /// Resolve a name-string from a signature (`TypeReference.identifier`,
    /// `TraitRef.name`). Total: unresolvable names get a `~extern` stub.
    pub fn resolve_name(&self, identifier: &str) -> String {
        let normalized = if identifier.contains("::") {
            identifier.to_string()
        } else {
            identifier.replace('.', "::")
        };
        if let Some(iri) = self.exact.get(&normalized) {
            return iri.clone();
        }
        let segments: Vec<String> = normalized.split("::").map(str::to_string).collect();
        if let Some(last) = segments.last() {
            if let Some(Some(iri)) = self.suffix.get(last) {
                return iri.clone();
            }
        }
        let iri = symbol_iri(&self.ctx.language, EXTERN_PACKAGE, &normalized);
        self.note_stub(&iri, EXTERN_PACKAGE.to_string(), segments);
        iri
    }

    /// A lazy link to a symbol by IRI (the unloaded/external-reference form,
    /// so inserting the referrer never re-nests the referent).
    pub fn lazy(&self, iri: &str) -> TdbLazy<m::Symbol> {
        TdbLazy::new_id_unchecked(iri)
    }

    fn note_stub(&self, iri: &str, package: String, segments: Vec<String>) {
        if package != self.ctx.package {
            self.extern_packages.borrow_mut().insert(package.clone());
        }
        self.stubs
            .borrow_mut()
            .entry(iri.to_string())
            .or_insert(StubSeed { package, segments });
    }

    /// Materialize everything resolution minted: stub symbols and the bare
    /// `Package` nodes they hang off. Call once, after projecting all entries.
    pub fn take_stubs(&self) -> (Vec<m::Symbol>, Vec<m::Package>) {
        let stubs = std::mem::take(&mut *self.stubs.borrow_mut());
        let symbols = stubs
            .into_iter()
            .map(|(iri, seed)| {
                let fq = seed.segments.join("::");
                m::Symbol {
                    id: EntityIDFor::new(&iri).expect("sanitized iri"),
                    // Logical `{lang}/{package}/{fq}` (decoded from %2F id segment).
                    uri: decode_id_segment(iri.trim_start_matches("Symbol/")).join("/"),
                    fq_name: fq.clone(),
                    name: seed.segments.last().cloned().unwrap_or(fq),
                    path: seed.segments,
                    aliases: BTreeSet::new(),
                    symbol_id: None,
                    kind: m::SymbolKind::Unresolved,
                    visibility: m::Visibility::Public,
                    documentation: None,
                    resolved: false,
                    package: TdbLazy::new_id_unchecked(&package_iri(
                        &self.ctx.language,
                        &seed.package,
                    )),
                    member_of: None,
                    implements: Vec::new(),
                    extends: Vec::new(),
                    mentions: Vec::new(),
                    takes: Vec::new(),
                    returns: Vec::new(),
                    shape: None,
                }
            })
            .collect();
        let packages = std::mem::take(&mut *self.extern_packages.borrow_mut())
            .into_iter()
            .map(|name| m::Package {
                id: EntityIDFor::new(&package_iri(&self.ctx.language, &name))
                    .expect("sanitized iri"),
                name,
                language: self.ctx.language.clone(),
            })
            .collect();
        (symbols, packages)
    }
}
