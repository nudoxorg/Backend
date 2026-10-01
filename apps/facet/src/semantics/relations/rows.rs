//! A relation group has a small invitation, then scope and package doors.
//! Nothing is truncated silently: every omitted name has an exact remainder.

use super::{Entry, Group, Word, label};
use crate::graph::{NodeId, World};
use gpui::SharedString;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Band { Yours, Here, Elsewhere }

impl Band {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self { Self::Yours => "Yours", Self::Here => "Here", Self::Elsewhere => "Elsewhere" }
    }
}

#[derive(Clone, Debug)]
pub struct Name {
    pub entry: Entry,
    pub label: SharedString,
    pub yours: bool,
}

#[derive(Clone, Debug)]
pub struct PackageRow {
    pub package: Option<u32>,
    pub name: SharedString,
    pub names: Vec<Name>,
}

#[derive(Clone, Debug)]
pub struct ScopeRow { pub band: Band, pub packages: Vec<PackageRow> }

#[derive(Clone, Debug)]
pub struct RelationRow {
    pub word: Word,
    pub names: Vec<Name>,
    pub scopes: Vec<ScopeRow>,
    pub yours: usize,
    pub packages: usize,
}

impl RelationRow {
    #[must_use]
    pub fn count(&self) -> String {
        format!("{} relations, {} of yours, in {} packages", self.names.len(), self.yours, self.packages)
    }

    #[must_use]
    pub fn invitation(&self, limit: usize) -> String {
        let mut words = self.names.iter().take(limit).map(|n| n.label.as_ref()).collect::<Vec<_>>().join(", ");
        let more = self.names.len().saturating_sub(limit);
        if more > 0 { words.push_str(&format!(" and {more} more")); }
        format!("{}: {words}", verb(self.word))
    }
}

/// Page vocabulary is independent of the graph's directional labels.
#[must_use]
pub const fn verb(word: Word) -> &'static str {
    match word {
        Word::MadeBy => "comes from", Word::ImplementedBy => "done by",
        Word::CalledFrom => "called by", Word::CallsIt => "called on by",
        Word::TakenBy => "taken by", Word::HeldBy => "held by",
        Word::UsedBy => "used by", Word::Calls => "calls",
        Word::Takes => "inputs", Word::Gives => "result",
        Word::MadeOf => "parts", Word::Is => "capabilities",
    }
}

#[must_use]
pub const fn meaning(word: Word) -> &'static str {
    match word {
        Word::MadeBy => "Callables that return this type.",
        Word::ImplementedBy => "Types that fulfil this contract.",
        Word::CalledFrom => "Code that invokes this callable.",
        Word::CallsIt => "Code that invokes this type’s methods.",
        Word::TakenBy => "Callables with a parameter of this type.",
        Word::HeldBy => "Fields and variants that store this type.",
        Word::UsedBy => "Other resolved mentions of this declaration.",
        Word::Calls => "Callables invoked inside this declaration.",
        Word::Takes => "Types accepted by this callable.",
        Word::Gives => "Types returned by this callable.",
        Word::MadeOf => "Declared parts of this type.",
        Word::Is => "Contracts this type fulfils.",
    }
}

/// Stable order: your packages first, the viewed package, then elsewhere.
/// Names keep the semantic authority's importance order inside each package.
#[must_use]
pub fn rows(world: &World, subject: NodeId, groups: Vec<Group>) -> Vec<RelationRow> {
    let here = world.node(subject).pkg;
    groups.into_iter().map(|group| {
        let names: Vec<Name> = group.entries.into_iter().map(|entry| Name {
            label: label(world, &entry), yours: entry.node.is_some_and(|n| world.yours(n)), entry,
        }).collect();
        let mut scopes: Vec<ScopeRow> = Vec::new();
        let mut packages = std::collections::BTreeSet::new();
        for name in &names {
            let package = name.entry.node.map(|n| world.node(n).pkg);
            let band = if name.yours { Band::Yours } else if package == Some(here) { Band::Here } else { Band::Elsewhere };
            let at = scopes.iter().position(|s| s.band == band).unwrap_or_else(|| {
                scopes.push(ScopeRow { band, packages: Vec::new() }); scopes.len() - 1
            });
            let list = &mut scopes[at].packages;
            let at = list.iter().position(|p| p.package == package).unwrap_or_else(|| {
                list.push(PackageRow { package, name: package.map_or_else(|| "Package not resolved".into(), |p| world.package_short(p).to_owned().into()), names: Vec::new() }); list.len() - 1
            });
            list[at].names.push(name.clone());
            if let Some(package) = package { packages.insert(package); }
        }
        scopes.sort_by_key(|s| s.band);
        RelationRow { word: group.word, yours: names.iter().filter(|n| n.yours).count(), packages: packages.len(), names, scopes }
    }).collect()
}
