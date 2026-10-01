//! What the package folio shows, read out of the dossier once per frame into
//! plain data: the modules and their public names, the releases, the
//! licence, the advisories, and (in the past) what the release being read
//! did to each name. Nothing here draws.

use crate::model::pages::{
    Gap, GapReason, Known, OutlineNode, OutlineTree, PackageDossier, PackageRecord, PackageRef,
    Standing, SymbolRef,
};
use crate::model::source_facts::{self, SourceFacts};
use backend_library::DeclarationKind;
use facet::folio::berg::{BergBlock, BergFacts};
use facet::folio::cards::Change;
use facet::folio::crest::{Advisories, Silence};
use facet::folio::features::{FeatureFacts, FeatureNode};
use facet::folio::heads::{Place, Sighting, Signals};
use facet::folio::state::{Build, Extent, Names, Standing as ReleaseStanding};
use facet::folio::ticker::{Release, TickerFacts};
use facet::icons::{Kind, Lang};
use facet::marks::license::LicenseFacts;
use facet::tokens::Family;
use gpui::SharedString;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// A module with more names than this opens on a page of its own.
pub(super) const BIG: usize = 14;

/// One public name.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ItemData {
    /// The exact coordinate its page is at.
    pub symbol: SymbolRef,
    /// Its name.
    pub name: SharedString,
    /// The index's kind for it.
    pub kind: Kind,
    /// Its family, which picks the shingle's hue.
    pub family: Family,
    /// The language it is written in.
    pub lang: Lang,
    /// The signature text the index recorded, when it recorded one.
    pub signature: Option<Arc<str>>,
    /// The author's first sentence, when the index recorded one.
    pub summary: Option<Arc<str>>,
}

/// One module: a file (or a namespace in one) and what it makes public.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ModuleData {
    /// Its path (`sync::mpsc`).
    pub name: SharedString,
    /// Whether the index recorded that its names are in it.
    pub placement: Placement,
    /// Its own first sentence.
    pub doc: Option<SharedString>,
    /// Its public names, in outline order.
    pub items: Vec<ItemData>,
}

/// Whether the index recorded which module a name is in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Placement {
    /// It did: the module is the one the index (or the source) names.
    Recorded,
    /// It did not: the names were gathered into one region for want of a module.
    Gathered,
}

/// What the modules on the page are, said with the count of names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Structure {
    /// Modules as the index and the source record them.
    Modules,
    /// One region, the crate root, whose names are re-exported from `hidden`
    /// private modules: the crate's public API is flat by design.
    Root { hidden: usize },
    /// The index records no module for these names.
    Gathered,
}

/// What `modules` (as `modules(.., source)` returned them) are: told apart so
/// the page says a flat root, or names with no module, in words rather than
/// drawing one region as though that were how the package is organised.
pub(super) fn structure(modules: &[ModuleData], source: Option<&SourceFacts>) -> Structure {
    let [only] = modules else {
        return Structure::Modules;
    };
    if only.placement == Placement::Gathered {
        return Structure::Gathered;
    }
    let Some(scanned) = source.and_then(|source| {
        source
            .module(only.name.as_ref())
            .or_else(|| source.module(&source_key(only.name.as_ref())))
    }) else {
        return Structure::Modules;
    };
    let mut origins: Vec<&str> = only
        .items
        .iter()
        .filter_map(|item| {
            scanned
                .items
                .iter()
                .find(|found| found.name.as_str() == item.name.as_ref())
                .and_then(|found| found.from.as_deref())
        })
        .collect();
    origins.sort_unstable();
    origins.dedup();
    if origins.is_empty() {
        Structure::Modules
    } else {
        Structure::Root {
            hidden: origins.len(),
        }
    }
}

impl ModuleData {
    /// How it opens: inline, or on a page of its own when it has many names.
    pub(super) fn extent(&self) -> Extent {
        if self.items.len() > BIG {
            Extent::Page
        } else {
            Extent::Inline
        }
    }
}

/// The language a declaration's frontend implies, in facet's spelling.
pub(super) const fn lang_of(language: backend_present::Language) -> Lang {
    use backend_present::Language;
    match language {
        Language::TypeScript => Lang::Typescript,
        Language::Python => Lang::Python,
        Language::Go => Lang::Go,
        Language::Java => Lang::Java,
        Language::CSharp => Lang::Csharp,
        Language::C | Language::Cxx => Lang::Cpp,
        Language::Rust | Language::Unknown => Lang::Rust,
    }
}

/// The index's kind as facet's mark.
pub(super) const fn kind_of(kind: Option<DeclarationKind>) -> Kind {
    crate::shell::kit::kind_of(kind)
}

/// Whether a declaration at a module's top level is a public name a
/// shingle stands for.
const fn is_name(kind: Option<DeclarationKind>) -> bool {
    matches!(
        kind,
        Some(
            DeclarationKind::Struct
                | DeclarationKind::Class
                | DeclarationKind::Enum
                | DeclarationKind::Union
                | DeclarationKind::Type
                | DeclarationKind::Trait
                | DeclarationKind::Interface
                | DeclarationKind::Function
                | DeclarationKind::Method
                | DeclarationKind::Macro
                | DeclarationKind::Constant
                | DeclarationKind::Variable
        )
    )
}

/// What the index recorded about a declaration beyond its name and kind:
/// the signature text and the author's first sentence. The outline node
/// carries neither today (the request is in FOLIO-A.md: `OutlineNode`
/// gains `signature` and `summary`, both from the row the index already
/// holds), so this is the one seam that changes when it does; until then
/// the source-facts reader fills what is on disk, labelled as such.
pub(super) fn brief_of(_node: &OutlineNode) -> (Option<Arc<str>>, Option<Arc<str>>) {
    (None, None)
}

/// A module's path from the file its root node is: `src/de/mod.rs` reads
/// `de`, `lib.rs` reads `lib`, in the language's own separator.
fn module_path(node: &OutlineNode) -> String {
    let separator = match node.decl.language {
        backend_present::Language::Rust
        | backend_present::Language::Cxx
        | backend_present::Language::C => "::",
        backend_present::Language::Python => ".",
        _ => "/",
    };
    let Some(path) = node.decl.path.as_deref() else {
        return super::super::super::shelf::shelf_name(node);
    };
    let stem = path.rsplit_once('.').map_or(path, |(stem, _)| stem);
    let mut parts: Vec<&str> = stem.split('/').filter(|part| !part.is_empty()).collect();
    if matches!(parts.first().copied(), Some("src" | "lib" | "source")) && parts.len() > 1 {
        parts.remove(0);
    }
    if matches!(parts.last().copied(), Some("mod" | "index" | "__init__")) && parts.len() > 1 {
        parts.pop();
    }
    if matches!(parts.as_slice(), ["main"] | ["index"] | ["__init__"]) {
        return "lib".to_owned();
    }
    parts.join(separator)
}

fn collect(
    node: &OutlineNode,
    name: String,
    package_name: &str,
    placement: Placement,
    out: &mut Vec<ModuleData>,
) {
    let _ = package_name;
    let mut items = Vec::new();
    let mut nested = Vec::new();
    for child in node.children.iter() {
        if child.decl.kind == Some(DeclarationKind::Module) {
            if !super::super::super::shelf::is_test_module(child) {
                nested.push(child);
            }
        } else if is_name(child.decl.kind) {
            let (signature, summary) = brief_of(child);
            items.push(ItemData {
                symbol: child.decl.coordinate.clone(),
                name: child.decl.name.to_string().into(),
                kind: kind_of(child.decl.kind),
                family: match child.decl.family {
                    crate::model::pages::KindFamily::Namespace => Family::Namespace,
                    crate::model::pages::KindFamily::Type => Family::Type,
                    crate::model::pages::KindFamily::Contract => Family::Contract,
                    crate::model::pages::KindFamily::Callable => Family::Callable,
                    crate::model::pages::KindFamily::Value => Family::Value,
                },
                lang: lang_of(child.decl.language),
                signature,
                summary,
            });
        }
    }
    if !items.is_empty() {
        out.push(ModuleData {
            name: name.clone().into(),
            placement,
            doc: None,
            items,
        });
    }
    for child in nested {
        let inner = super::super::super::shelf::shelf_name(child);
        collect(
            child,
            format!("{name}::{inner}"),
            package_name,
            placement,
            out,
        );
    }
}

/// The modules of a package: every non-test module with at least one public
/// name, `lib` first and then by size, the way the board orders them.
pub(super) fn modules(
    tree: &OutlineTree,
    package_name: &str,
    source: Option<&SourceFacts>,
) -> Vec<ModuleData> {
    let mut out = Vec::new();
    let mut top = Vec::new();
    for root in tree.roots.iter() {
        if root.decl.kind == Some(DeclarationKind::Module) {
            if !super::super::super::shelf::is_test_module(root) {
                collect(
                    root,
                    module_path(root),
                    package_name,
                    Placement::Recorded,
                    &mut out,
                );
            }
        } else if is_name(root.decl.kind) {
            top.push(root);
        }
    }
    if !top.is_empty() {
        // A compiler-backed Rust outline is flat: no file-module node holds a
        // file's names, but each name says which file it is declared in, and in
        // Rust a file is a module. So a name that carries its path is placed in
        // that module. What carries none (an implementation block, a type the
        // signatures merely mention) has no module to be placed in and is not a
        // public name of one.
        let placed: Vec<&OutlineNode> = top
            .iter()
            .copied()
            .filter(|node| {
                node.decl.language == backend_present::Language::Rust && node.decl.path.is_some()
            })
            .collect();
        if placed.is_empty() {
            // Top-level names of languages that have no file module (Go
            // packages, a Python `__init__`), and of an outline that places
            // none of its names, read as one module of the package.
            let synthetic = OutlineNode {
                children: top
                    .iter()
                    .map(|node| (*node).clone())
                    .collect::<Vec<_>>()
                    .into(),
                ..top[0].clone()
            };
            collect(
                &synthetic,
                package_name.to_owned(),
                package_name,
                Placement::Gathered,
                &mut out,
            );
        }
        let mut by_module: Vec<(String, Vec<OutlineNode>)> = Vec::new();
        for node in placed {
            let module = module_path(node);
            match by_module.iter_mut().find(|(name, _)| *name == module) {
                Some((_, names)) => names.push(node.clone()),
                None => by_module.push((module, vec![node.clone()])),
            }
        }
        for (name, names) in by_module {
            let group = OutlineNode {
                children: names.clone().into(),
                ..names[0].clone()
            };
            collect(&group, name, package_name, Placement::Recorded, &mut out);
        }
    }
    // The same module can be read from two files (`mod.rs` and `x.rs`):
    // merge by name.
    let mut merged: Vec<ModuleData> = Vec::new();
    for module in out {
        if let Some(found) = merged.iter_mut().find(|m| m.name == module.name) {
            found.items.extend(module.items);
            if found.doc.is_none() {
                found.doc = module.doc;
            }
        } else {
            merged.push(module);
        }
    }
    if let Some(source) = source {
        enrich(&mut merged, source);
    }
    merged.sort_by(|a, b| {
        (b.name == "lib")
            .cmp(&(a.name == "lib"))
            .then_with(|| b.items.len().cmp(&a.items.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    merged
}

/// The source key of an index module path: the board keeps a module's
/// first two segments (`a::b::c` reads as `a::b`).
fn source_key(name: &str) -> String {
    name.split("::").take(2).collect::<Vec<_>>().join("::")
}

/// What the source on disk adds: each module's own words, each name's
/// declaration and first sentence where the index recorded none, and (when
/// the source was scanned) only the names it makes public, in the modules it
/// does not make private.
fn enrich(modules: &mut Vec<ModuleData>, source: &SourceFacts) {
    let scanned = !source.modules.is_empty();
    let key_of = |name: &str| -> String {
        if source.module(name).is_some() || source.docs.contains_key(name) {
            name.to_owned()
        } else {
            source_key(name)
        }
    };
    // Every name the index put in a module, by the module that defines it:
    // what a `pub use` elsewhere makes public is found here (its page is
    // the definition's).
    let pool: Vec<(String, ItemData)> = modules
        .iter()
        .flat_map(|m| m.items.iter().map(|i| (key_of(m.name.as_ref()), i.clone())))
        .collect();
    let adopt = |module_key: &str, into: &mut Vec<ItemData>| {
        let Some(scanned_module) = source.module(module_key) else {
            return;
        };
        for item in scanned_module.items.iter().filter(|i| i.from.is_some()) {
            if into
                .iter()
                .any(|have| have.name.as_ref() == item.name.as_str())
            {
                continue;
            }
            let origin = item.from.as_deref().unwrap_or_default();
            if let Some((_, found)) = pool
                .iter()
                .find(|(key, i)| key == origin && i.name.as_ref() == item.name.as_str())
            {
                into.push(found.clone());
            }
        }
    };
    for module in modules.iter_mut() {
        let key = key_of(module.name.as_ref());
        adopt(&key, &mut module.items);
        if module.doc.is_none()
            && let Some(doc) = source.docs.get(&key)
        {
            module.doc = Some(doc.sentence.clone().into());
        }
        let Some(scanned_module) = source.module(&key) else {
            continue;
        };
        for item in &mut module.items {
            if let Some(found) = scanned_module
                .items
                .iter()
                .find(|i| i.name.as_str() == item.name.as_ref())
            {
                if item.signature.is_none() {
                    item.signature = Some(Arc::from(found.declaration.as_str()));
                }
                if item.summary.is_none()
                    && let Some(doc) = &found.doc
                {
                    item.summary = Some(Arc::from(doc.as_str()));
                }
            }
        }
        if scanned {
            let public: Vec<&str> = scanned_module
                .items
                .iter()
                .map(|i| i.name.as_str())
                .collect();
            module
                .items
                .retain(|item| public.contains(&item.name.as_ref()));
        }
    }
    // A module that is public only through what it re-exports may hold no
    // name of its own in the index (a crate root of `pub use`).
    if scanned {
        let known: Vec<String> = modules.iter().map(|m| key_of(m.name.as_ref())).collect();
        for module in &source.modules {
            if module.access == source_facts::docs::Access::Private
                || known.contains(&module.path)
                || !module.items.iter().any(|i| i.from.is_some())
            {
                continue;
            }
            let mut items = Vec::new();
            adopt(&module.path, &mut items);
            if !items.is_empty() {
                modules.push(ModuleData {
                    name: module.path.clone().into(),
                    placement: Placement::Recorded,
                    doc: source
                        .docs
                        .get(&module.path)
                        .map(|d| d.sentence.clone().into()),
                    items,
                });
            }
        }
        for module in modules.iter_mut() {
            if let Some(scanned_module) = source.module(&key_of(module.name.as_ref())) {
                for item in &mut module.items {
                    if let Some(found) = scanned_module
                        .items
                        .iter()
                        .find(|i| i.name.as_str() == item.name.as_ref())
                    {
                        if item.signature.is_none() {
                            item.signature = Some(Arc::from(found.declaration.as_str()));
                        }
                        if item.summary.is_none()
                            && let Some(doc) = &found.doc
                        {
                            item.summary = Some(Arc::from(doc.as_str()));
                        }
                    }
                }
            }
        }
        modules.retain(|module| {
            let key = key_of(module.name.as_ref());
            !module.items.is_empty()
                && !source
                    .module(&key)
                    .is_some_and(|m| m.access == source_facts::docs::Access::Private)
        });
    }
}

/// Every module's names and how many carry a summary.
pub(super) fn documented(modules: &[ModuleData]) -> Option<(usize, usize)> {
    let total: usize = modules.iter().map(|m| m.items.len()).sum();
    let with: usize = modules
        .iter()
        .flat_map(|m| m.items.iter())
        .filter(|item| item.summary.is_some())
        .count();
    (with > 0).then_some((with, total))
}

/// Whose page it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Subject {
    /// The reader's own project: its licence is not judged against itself.
    Project,
    /// A package the project may depend on.
    Dependency,
}

/// The licence the folio judges: the record's SPDX, against the active
/// project's own.
pub(super) fn licence(
    record: Option<&PackageRecord>,
    active_license: Option<&str>,
    project: &str,
    subject: Subject,
) -> Rc<LicenseFacts> {
    let spdx = record
        .and_then(|record| record.license.known())
        .map(ToString::to_string);
    let mut facts = LicenseFacts::new(spdx.as_deref(), active_license, project);
    facts.own = subject == Subject::Project;
    Rc::new(facts)
}

/// What the advisory feeds say, in the crest's three states.
pub(super) fn advisories(record: Option<&PackageRecord>) -> Advisories {
    use backend_library::{AdvisoryCoverage, FreshnessState, SeverityLevel};
    let unknown = |why: Silence, note: &str| Advisories::Unknown {
        why,
        note: note.to_owned().into(),
    };
    let Some(record) = record else {
        return unknown(
            Silence::NotRead,
            "The record this page is about has not been read.",
        );
    };
    match &record.advisory {
        Known::Known(summary) => {
            if summary.advisories > 0 {
                let worst = summary.worst.map(|level| match level {
                    SeverityLevel::Unknown => "unrated",
                    SeverityLevel::Low => "low",
                    SeverityLevel::Moderate => "moderate",
                    SeverityLevel::High => "high",
                    SeverityLevel::Critical => "critical",
                });
                return Advisories::Found {
                    count: summary.advisories,
                    worst: worst.map(|w| w.to_owned().into()),
                    decision: summary.decision.to_string().into(),
                };
            }
            let fresh = match summary.freshness {
                FreshnessState::Fresh | FreshnessState::NotModified => "up to date",
                FreshnessState::Stale => "the feeds are stale",
                FreshnessState::Unknown => "feed age unknown",
            };
            match summary.coverage {
                AdvisoryCoverage::Complete => Advisories::Clear {
                    note: format!(
                        "Every configured feed was read, {fresh}. None names this release."
                    )
                    .into(),
                },
                AdvisoryCoverage::Partial => Advisories::Clear {
                    note: format!("Some feeds were read, {fresh}. None named this release.").into(),
                },
                AdvisoryCoverage::Unknown | AdvisoryCoverage::Unavailable => unknown(
                    Silence::NoFeed,
                    "RustSec, OSV and GHSA can be read; none configured.",
                ),
            }
        }
        Known::Unknown(gap) => gap_state(gap),
    }
}

fn gap_state(gap: &Gap) -> Advisories {
    let unknown = |why: Silence, note: String| Advisories::Unknown {
        why,
        note: note.into(),
    };
    match gap.reason {
        GapReason::LocalProject => unknown(
            Silence::Yours,
            "Advisory feeds check published releases; this is a project of yours.".to_owned(),
        ),
        GapReason::Unconfigured | GapReason::NotRecorded | GapReason::NoSemanticPublication => {
            unknown(
                Silence::NoFeed,
                "RustSec, OSV and GHSA can be read; none configured.".to_owned(),
            )
        }
        _ => unknown(
            Silence::Unknown,
            crate::shell::kit::gap_words(gap).to_string(),
        ),
    }
}

/// What a release being read did to the names of the release you pin.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Past {
    /// The release being read.
    pub at: SharedString,
    /// Whether it came before the pin or after it.
    pub side: Side,
    /// Each affected name, by name.
    pub states: HashMap<SharedString, Change>,
    /// Names the pin has that this release did not yet: only before the pin.
    pub absent: usize,
    /// Names whose signature differs.
    pub changed: usize,
    /// Names the release no longer has (after the pin).
    pub gone: usize,
    /// Names this release added (after the pin), which the pin's map cannot show.
    pub added: usize,
    /// Whether the release data knew this release at all.
    pub diffs: Diffs,
}

/// A release read against the pin: before it, or after it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Side {
    /// An older release than the pin.
    #[default]
    Before,
    /// A newer one.
    After,
}

/// Whether the release data has the names this release changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Diffs {
    /// Only its date and size are known.
    #[default]
    Unknown,
    /// Its names are diffed against the pin's.
    Known,
}

/// The past for `at`, from the release fixture's diffs when it has them.
pub(super) fn past(package: &PackageRef, at: &str, cx: &mut gpui::App) -> Past {
    let mut out = Past {
        at: at.to_owned().into(),
        ..Past::default()
    };
    let Some(release) = crate::runtime::fixture_releases::release_data(package, cx) else {
        return out;
    };
    let Some(spelled) = crate::runtime::fixture_releases::spelled(release, at) else {
        return out;
    };
    let position = |v: &str| {
        release
            .versions
            .iter()
            .position(|known| known.v.as_ref() == v)
    };
    let (Some(pinned), Some(viewed)) = (
        position(release.pinned.as_ref()),
        position(spelled.as_ref()),
    ) else {
        return out;
    };
    out.side = if viewed < pinned {
        Side::Before
    } else {
        Side::After
    };
    let Some(diff) = release
        .diffs
        .iter()
        .find(|diff| diff.from == release.pinned && diff.to == spelled)
    else {
        return out;
    };
    out.diffs = Diffs::Known;
    for change in &diff.changes {
        let name: SharedString = change.name().to_owned().into();
        use facet::data::release::What;
        match change.what {
            What::Removed => {
                if out.side == Side::Before {
                    out.absent += 1;
                    out.states.insert(name, Change::Absent);
                } else {
                    out.gone += 1;
                    out.states.insert(name, Change::Gone);
                }
            }
            What::Changed
            | What::Deprecated
            | What::FieldAdded
            | What::FieldRemoved
            | What::VariantAdded
            | What::VariantRemoved => {
                if change.what == What::Changed && change.respelled() {
                    continue;
                }
                out.changed += 1;
                out.states.insert(name, Change::Changed);
            }
            What::Added => {
                out.added += 1;
            }
            What::Renamed => {}
        }
    }
    out
}

/// The ticker for a dossier: its recorded releases, with the dates the
/// release fixture knows, and the pin from the route.
pub(super) fn ticker(
    dossier: &PackageDossier,
    source: Option<&SourceFacts>,
    pin: Option<&str>,
    reading: Option<&str>,
    today: &str,
    cx: &mut gpui::App,
) -> Option<Rc<TickerFacts>> {
    // Every release the registry's index cache knows, dated; the ones the
    // index has read are the ones whose names the page can show. This holds
    // for a crate the library indexed from an unpacked registry directory
    // too, whose version list the dossier does not carry: the pin is the
    // release whose names are read then.
    if let Some(published) = source.map(|s| &s.releases).filter(|r| r.len() > 1) {
        let versions = dossier.versions.known();
        let same = |a: &str, b: &str| {
            a == b || facet::marks::semver::short(a) == facet::marks::semver::short(b)
        };
        let read = |version: &str| match versions {
            Some(list) => list
                .iter()
                .any(|entry| same(entry.version.as_ref(), version)),
            None => pin.is_some_and(|pin| same(pin, version)),
        };
        let releases: Vec<Release> = published
            .iter()
            .map(|release| Release {
                version: release.version.clone(),
                date: release.date.clone(),
                standing: release.standing,
                names: if read(&release.version) {
                    Names::Read
                } else {
                    Names::Unread
                },
            })
            .collect();
        return Some(Rc::new(
            TickerFacts::new(&releases, pin, today).reading(reading),
        ));
    }
    let versions = dossier.versions.known()?;
    if versions.is_empty() {
        return None;
    }
    let dated = crate::runtime::fixture_releases::release_data(&dossier.package, cx);
    let releases: Vec<Release> = versions
        .iter()
        .map(|entry| {
            let at = dated.and_then(|release| {
                release
                    .versions
                    .iter()
                    .find(|known| {
                        known.v.as_ref() == entry.version.as_ref()
                            || facet::data::release::short(&known.v) == entry.version.as_ref()
                    })
                    .map(|known| known.at.to_string())
            });
            Release {
                version: entry.version.to_string(),
                date: at,
                standing: if entry.standing == Standing::Yanked {
                    ReleaseStanding::Yanked
                } else {
                    ReleaseStanding::Available
                },
                names: Names::Read,
            }
        })
        .collect();
    Some(Rc::new(
        TickerFacts::new(&releases, pin, today).reading(reading),
    ))
}

/// The heads-up findings' facts, from what the source says it does.
pub(super) fn signals(source: &SourceFacts) -> Signals {
    let places = |cap: &source_facts::scan::Capability| -> Sighting {
        Sighting {
            count: cap.count,
            places: cap
                .examples
                .iter()
                .map(|e| Place {
                    file: e.file.clone().into(),
                    line: e.line,
                    text: e.text.clone().into(),
                })
                .collect(),
        }
    };
    Signals {
        build: if source.manifest.build == Build::Script || source.scan.build == Build::Script {
            Build::Script
        } else {
            Build::Plain
        },
        library: source.manifest.library,
        unsafe_code: source.scan.unsafe_code,
        unsafe_count: source.scan.unsafe_count,
        process: places(&source.scan.process),
        ffi: places(&source.scan.ffi),
        net: places(&source.scan.net),
        files: places(&source.scan.fs),
        env: places(&source.scan.env),
    }
}

/// The weight iceberg.
pub(super) fn berg(name: &str, source: &SourceFacts) -> BergFacts {
    BergFacts {
        name: name.to_owned().into(),
        own: source.berg.own,
        blocks: source
            .berg
            .blocks
            .iter()
            .map(|b| BergBlock {
                name: b.name.clone().into(),
                version: b.version.clone().into(),
                sloc: b.sloc,
                layer: b.layer,
                parent: b.parent,
                deps: b.deps.clone(),
            })
            .collect(),
        missing: source.berg.missing,
        basis: Some(source.berg.basis),
    }
}

/// The features: `None` when the package declares none.
pub(super) fn features(source: &SourceFacts) -> Option<FeatureFacts> {
    let manifest = &source.manifest;
    if manifest.features.is_empty() {
        return None;
    }
    Some(FeatureFacts {
        names: manifest.features.clone(),
        default: manifest.default.clone(),
        graph: manifest
            .graph
            .iter()
            .map(|(name, node)| {
                (
                    name.clone(),
                    FeatureNode {
                        enables: node.enables.clone(),
                        deps: node.deps.clone(),
                    },
                )
            })
            .collect(),
        sizes: source.dependency_lines.iter().cloned().collect(),
    })
}

/// The line under the lede: who made it, where, what it is about, and what
/// it needs to build, each as read from its manifest.
pub(super) fn byline(source: &SourceFacts) -> Vec<SharedString> {
    let manifest = &source.manifest;
    let mut parts: Vec<SharedString> = Vec::new();
    if !manifest.authors.is_empty() {
        parts.push(
            format!(
                "by {}",
                manifest
                    .authors
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into(),
        );
    }
    if let Some(repository) = &manifest.repository {
        let short = repository
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.")
            .trim_end_matches(".git")
            .trim_end_matches('/');
        parts.push(short.to_owned().into());
    }
    if !manifest.categories.is_empty() {
        parts.push(
            manifest
                .categories
                .iter()
                .take(2)
                .map(|c| c.replace("::", " › "))
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
        );
    }
    if let Some(edition) = &manifest.edition {
        parts.push(
            match &manifest.rust_version {
                Some(version) => format!("Rust {edition}, needs {version}"),
                None => format!("Rust {edition}"),
            }
            .into(),
        );
    }
    parts
}
