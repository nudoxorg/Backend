//! The release fixture: toml 0.5.11 … 1.1.6 and smallvec 1.15.1 … 1.16.1,
//! with this workspace's uses of each — a slice of the prototype's
//! `releases.json` (schema: `v4/graph/releases.mjs`), copied here so tests
//! and scenes never read the prototype's file.

use super::json::{self, Json};
use super::{Change, Crate, Impacted, ReleaseDiff, Severity, UseSite, Version, What};
use gpui::SharedString;
use std::collections::HashMap;
use std::sync::OnceLock;

const FIXTURE: &str = include_str!("fixture.json");

fn text(value: Option<&Json>) -> Option<SharedString> {
    value.and_then(Json::str).map(|s| SharedString::from(s.to_owned()))
}

fn word(value: Option<&Json>) -> SharedString {
    text(value).unwrap_or_default()
}

impl What {
    /// The release data's word for a change (`"changed"`, `"field added"`).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "added" => Self::Added,
            "removed" => Self::Removed,
            "changed" => Self::Changed,
            "deprecated" => Self::Deprecated,
            "renamed" => Self::Renamed,
            "field added" => Self::FieldAdded,
            "field removed" => Self::FieldRemoved,
            "variant added" => Self::VariantAdded,
            "variant removed" => Self::VariantRemoved,
            _ => return None,
        })
    }
}

fn severity(value: Option<&Json>, default: Severity) -> Severity {
    match value.and_then(Json::str) {
        Some("breaking") => Severity::Breaking,
        Some("additive") => Severity::Additive,
        _ => default,
    }
}

/// One change object (`{path, k, sig}` / `{path, before, after, kind}`).
fn change(x: &Json, what: What) -> Change {
    let default = match what {
        What::Added | What::Deprecated | What::FieldAdded => Severity::Additive,
        _ => Severity::Breaking,
    };
    let sig = text(x.get("sig"));
    let (before, after) = match what {
        What::Added => (None, sig.or_else(|| text(x.get("after")))),
        What::Removed => (sig.or_else(|| text(x.get("before"))), None),
        _ => (text(x.get("before")), text(x.get("after"))),
    };
    Change { path: word(x.get("path")), what, severity: severity(x.get("kind"), default), before, after }
}

/// Every change in one diff object, in the release's order.
fn changes(d: &Json) -> Vec<Change> {
    let mut out = Vec::new();
    for (key, what) in [
        ("added", What::Added),
        ("removed", What::Removed),
        ("changed", What::Changed),
        ("deprecated", What::Deprecated),
    ] {
        out.extend(d.get(key).map(Json::items).unwrap_or_default().iter().map(|x| change(x, what)));
    }
    for x in d.get("renamed").map(Json::items).unwrap_or_default() {
        out.push(Change {
            path: word(x.get("to")),
            what: What::Renamed,
            severity: severity(x.get("kind"), Severity::Breaking),
            before: text(x.get("from")),
            after: text(x.get("to")),
        });
    }
    // `fields` / `variants`: [{ path, added: [{name, kind}], removed: [{name, kind}] }].
    for (key, added, removed) in [
        ("fields", What::FieldAdded, What::FieldRemoved),
        ("variants", What::VariantAdded, What::VariantRemoved),
    ] {
        for owner in d.get(key).map(Json::items).unwrap_or_default() {
            let path = word(owner.get("path"));
            for (side, what, default) in [("added", added, Severity::Additive), ("removed", removed, Severity::Breaking)] {
                for member in owner.get(side).map(Json::items).unwrap_or_default() {
                    let name = member.get("name").and_then(Json::str).or(member.str()).unwrap_or("");
                    out.push(Change {
                        path: SharedString::from(format!("{path}::{name}")),
                        what,
                        severity: severity(member.get("kind").or(owner.get("kind")), default),
                        before: None,
                        after: None,
                    });
                }
            }
        }
    }
    out
}

fn site(x: &Json) -> UseSite {
    UseSite {
        path: word(x.get("path")),
        file: word(x.get("file")),
        line: x.get("line").and_then(Json::num).unwrap_or(0.0) as u32,
        text: SharedString::from(x.get("text").and_then(Json::str).unwrap_or("").trim().to_owned()),
    }
}

fn pair(key: &str) -> (SharedString, SharedString) {
    let (a, b) = key.split_once('→').unwrap_or((key, ""));
    (SharedString::from(a.to_owned()), SharedString::from(b.to_owned()))
}

/// One crate from its fixture object.
fn krate(name: &str, c: &Json) -> Crate {
    let versions = c
        .get("versions")
        .map(Json::items)
        .unwrap_or_default()
        .iter()
        .map(|v| Version {
            v: word(v.get("v")),
            at: word(v.get("at")),
            yanked: v.get("yanked").is_some_and(Json::truthy),
            local: v.get("local").is_some_and(Json::truthy),
        })
        .collect();
    // Every public path of every local release → its declared path (itself,
    // when it has no `via`).
    let aliases = c
        .get("api")
        .map(|api| {
            api.entries()
                .map(|(version, items)| {
                    let map: HashMap<SharedString, SharedString> = items
                        .entries()
                        .map(|(path, entry)| {
                            let path = SharedString::from(path.clone());
                            (path.clone(), text(entry.get("via")).unwrap_or(path))
                        })
                        .collect();
                    (SharedString::from(version.clone()), map)
                })
                .collect()
        })
        .unwrap_or_default();
    let diffs = c
        .get("diff")
        .map(|d| {
            d.entries()
                .map(|(key, d)| {
                    let (from, to) = pair(key);
                    ReleaseDiff {
                        from: text(d.get("from")).unwrap_or(from),
                        to: text(d.get("to")).unwrap_or(to),
                        changes: changes(d),
                        semver_slip: d.get("semverSlip").is_some_and(Json::truthy),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let impact = c
        .get("impact")
        .map(|i| {
            i.entries()
                .map(|(key, list)| {
                    let (from, to) = pair(key);
                    let list = list
                        .items()
                        .iter()
                        .map(|u| {
                            let ch = u.get("change").cloned().unwrap_or(Json::Null);
                            let what = ch.get("type").or(ch.get("what")).and_then(Json::str).and_then(What::parse).unwrap_or(What::Changed);
                            Impacted { site: site(u), change: change(&ch, what) }
                        })
                        .collect();
                    (from, to, list)
                })
                .collect()
        })
        .unwrap_or_default();
    Crate {
        name: SharedString::from(name.to_owned()),
        pinned: word(c.get("pinned")),
        versions,
        aliases,
        diffs,
        uses: c.get("uses").map(Json::items).unwrap_or_default().iter().map(site).collect(),
        impact,
    }
}

/// Every crate in the fixture, parsed once.
///
/// # Panics
/// If the fixture is not JSON (a broken copy).
pub fn crates() -> &'static [Crate] {
    static CRATES: OnceLock<Vec<Crate>> = OnceLock::new();
    CRATES.get_or_init(|| {
        let root = json::parse(FIXTURE).unwrap_or_else(|at| panic!("release fixture is not JSON at byte {at}"));
        root.entries().map(|(name, c)| krate(name, c)).collect()
    })
}

/// One crate of the fixture (`"toml"`, `"smallvec"`).
#[must_use]
pub fn get(name: &str) -> Option<&'static Crate> {
    crates().iter().find(|c| c.name == name)
}
