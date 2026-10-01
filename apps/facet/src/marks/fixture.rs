//! The marks lane's pinned fixture (`fixture.json`), for the gallery and
//! tests only: real packages, pinned.
//!
//! It is D-Marks' `v4/marks/data.json` narrowed to what the marks render,
//! written by the lane's `make_fixture.py`. That data was built offline
//! from the crates.io index cache (every release with its publish time),
//! this repository's `Cargo.lock` (pins, duplicates, the tree's licenses),
//! the registry sources (dependency tables, features, real usage edges),
//! `world.json` (workspace edges) and `releases.json` (API diffs). Purpose
//! phrases a usage rule could not derive were dropped, so the cards fall
//! back to the items actually used. Nothing here reads the live world or
//! the owner's registry.

use super::deps::{DepFacts, DepKind, InTree};
use super::eco::{Eco, EcoFacts};
use super::license::LicenseFacts;
use super::semver::ReleaseFact;
use super::spdx::{Family, Notable, TreeLicenses};
use super::version::{Also, Diff, Measured, VersionFacts};
use serde::Deserialize;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

const JSON: &str = include_str!("fixture.json");

/// The fixture, parsed once.
pub static FIXTURE: LazyLock<Fixture> =
    LazyLock::new(|| serde_json::from_str(JSON).unwrap_or_else(|error| panic!("marks fixture.json: {error}")));

/// The whole fixture.
#[derive(Debug, Deserialize)]
pub struct Fixture {
    /// Today, pinned (`ago` reads against it).
    pub now: String,
    /// The reader: this workspace.
    pub you: You,
    /// The lockfile's licenses.
    pub tree: Tree,
    /// Licenses of packages the board names.
    pub licenses: HashMap<String, serde_json::Value>,
    /// The packages.
    pub packages: HashMap<String, Package>,
}

/// The reader.
#[derive(Debug, Deserialize)]
pub struct You {
    /// Its name in consequence words.
    pub project: String,
    /// Its license.
    pub license: String,
}

/// The lockfile's licenses.
#[derive(Debug, Deserialize)]
pub struct Tree {
    total: usize,
    unread: usize,
    notable: Vec<NotableJson>,
}

#[derive(Debug, Deserialize)]
struct NotableJson {
    #[serde(rename = "crate")]
    krate: String,
    license: String,
    fam: String,
    #[serde(rename = "buildOnly")]
    build_only: bool,
}

/// One package.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    /// Its name.
    pub name: String,
    /// Its ecosystem.
    pub eco: String,
    /// The version the hero reads.
    pub version: Option<String>,
    /// Its description.
    pub description: Option<String>,
    /// Its license expression.
    pub license: Option<String>,
    /// Its install line.
    pub install: Option<String>,
    /// Your lockfile's pin.
    pub pin: Option<String>,
    releases: Vec<(String, Option<String>, bool)>,
    also: Vec<AlsoJson>,
    your_users: Vec<String>,
    #[serde(default)]
    pin_via: Vec<String>,
    measured: Option<MeasuredJson>,
    deps: Vec<DepJson>,
    /// A workspace package.
    pub local: bool,
    /// Its path in the workspace.
    pub path: Option<String>,
    /// Its API size, when indexed.
    pub api: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AlsoJson {
    v: String,
    via: Vec<String>,
    yours: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeasuredJson {
    your_uses: Option<usize>,
    api_size: HashMap<String, usize>,
    diffs: HashMap<String, DiffJson>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffJson {
    added: Option<usize>,
    removed: Option<usize>,
    changed: Option<usize>,
    your_sites_changed: Option<usize>,
    your_sites_touched: Option<usize>,
    your_items_respelled: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DepJson {
    name: String,
    kind: String,
    req: Option<String>,
    resolved: Option<String>,
    newest: Option<String>,
    optional: bool,
    features: Vec<String>,
    on: Option<OnJson>,
    uses: Option<usize>,
    items: Vec<(String, usize)>,
    purpose: Option<String>,
    local: bool,
    tree_note: Option<String>,
    in_tree: Option<InTreeJson>,
}

#[derive(Debug, Deserialize)]
struct OnJson {
    default: bool,
    tree: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InTreeJson {
    versions: Vec<String>,
    yours: Vec<String>,
    via: Vec<String>,
    via_count: Option<usize>,
    local: bool,
}

/// The package `name` (panics on a fixture typo: a scene bug).
#[must_use]
pub fn package(name: &str) -> &'static Package {
    FIXTURE.packages.get(name).unwrap_or_else(|| panic!("no package {name} in the marks fixture"))
}

/// The ecosystem facts of `name`.
#[must_use]
pub fn eco(name: &str) -> EcoFacts {
    let p = package(name);
    let eco = Eco::of(&p.eco).unwrap_or(Eco::Crates);
    let mut facts = EcoFacts::new(eco, p.install.as_deref());
    if p.local {
        facts.local = Some(p.path.clone().unwrap_or_default().into());
    }
    if eco == Eco::Cpp {
        facts.say = Some("No registry to install from: it is header-only, so the include is the install.".into());
    }
    facts
}

/// Your tree's licenses.
#[must_use]
pub fn tree() -> Rc<TreeLicenses> {
    let t = &FIXTURE.tree;
    Rc::new(TreeLicenses {
        total: t.total,
        unread: t.unread,
        notable: t
            .notable
            .iter()
            .map(|n| Notable {
                krate: n.krate.clone(),
                license: n.license.clone(),
                family: Family::of(&n.fam),
                build_only: n.build_only,
            })
            .collect(),
    })
}

/// A package licensed `spdx`, read by this workspace, knowing its tree.
#[must_use]
pub fn license(spdx: Option<&str>, package: Option<&str>) -> LicenseFacts {
    let mut facts = LicenseFacts::new(spdx, Some(&FIXTURE.you.license), &FIXTURE.you.project);
    facts.package = package.map(|p| p.to_owned().into());
    facts.tree = Some(tree());
    facts
}

/// A named example's license (`self_cell`, `option-ext`, `cfg_block`…).
#[must_use]
pub fn named_license(name: &str) -> LicenseFacts {
    match FIXTURE.licenses.get(name) {
        Some(serde_json::Value::String(spdx)) => license(Some(spdx), Some(name)),
        Some(serde_json::Value::Object(file)) => {
            let mut facts = license(None, Some(name));
            facts.file = file.get("file").and_then(serde_json::Value::as_str).map(|f| f.to_owned().into());
            facts
        }
        _ => panic!("no license {name} in the marks fixture"),
    }
}

/// The version facts of `name`.
#[must_use]
pub fn version(name: &str) -> VersionFacts {
    let p = package(name);
    VersionFacts {
        name: p.name.clone().into(),
        releases: p.releases.iter().map(|(v, at, yanked)| ReleaseFact::new(v.clone(), at.as_deref(), *yanked)).collect(),
        pin: if p.local { p.version.clone() } else { p.pin.clone() },
        also: p.also.iter().map(|a| Also { v: a.v.clone(), via: a.via.clone(), yours: a.yours.clone() }).collect(),
        yours: p.your_users.clone(),
        pin_via: p.pin_via.clone(),
        measured: p.measured.as_ref().map(|m| Measured {
            your_uses: m.your_uses,
            api_size: m.api_size.iter().map(|(v, n)| (v.clone(), *n)).collect(),
            diffs: m
                .diffs
                .iter()
                .map(|(v, d)| {
                    (v.clone(), Diff {
                        added: d.added.unwrap_or(0),
                        removed: d.removed.unwrap_or(0),
                        changed: d.changed.unwrap_or(0),
                        your_sites_changed: d.your_sites_changed.unwrap_or(0),
                        your_sites_touched: d.your_sites_touched.unwrap_or(0),
                        respelled: d.your_items_respelled.clone().unwrap_or_default(),
                    })
                })
                .collect(),
        }),
        local: p.local.then(|| p.path.clone().unwrap_or_default().into()),
        now: FIXTURE.now.clone().into(),
    }
}

/// The dependencies of `name`, each linking to its own page.
#[must_use]
pub fn deps(name: &str) -> Vec<DepFacts> {
    let p = package(name);
    let registry = Eco::of(&p.eco).unwrap_or(Eco::Crates).registry();
    p.deps
        .iter()
        .map(|d| DepFacts {
            name: d.name.clone().into(),
            kind: match d.kind.as_str() {
                "dev" => DepKind::Dev,
                "build" => DepKind::Build,
                _ => DepKind::Normal,
            },
            req: d.req.clone(),
            resolved: d.resolved.clone(),
            newest: d.newest.clone(),
            optional: d.optional,
            features: d.features.clone(),
            on_by_default: d.on.as_ref().map(|o| o.default),
            on_in_tree: d.on.as_ref().map(|o| o.tree),
            uses: d.uses,
            items: d.items.clone(),
            purpose: d.purpose.clone(),
            local: d.local,
            in_tree: d.in_tree.as_ref().map(|t| InTree {
                versions: t.versions.clone(),
                yours: t.yours.clone(),
                via: t.via.clone(),
                via_count: t.via_count,
                local: t.local,
            }),
            tree_note: d.tree_note.clone(),
            target: Some(if d.local || d.in_tree.as_ref().is_some_and(|t| t.local) {
                format!("workspace/{}", d.name).into()
            } else {
                format!("{registry}/{}", d.name).into()
            }),
        })
        .collect()
}

/// The one dependency `dep` of `name` (a normal one unless `kind` says).
#[must_use]
pub fn dep(name: &str, dep: &str, kind: DepKind) -> DepFacts {
    deps(name)
        .into_iter()
        .find(|d| d.name.as_ref() == dep && d.kind == kind)
        .unwrap_or_else(|| panic!("no dependency {dep} of {name} in the marks fixture"))
}
