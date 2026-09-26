//! The one rule for relations, every language (gui-plan §8.1).
//!
//! Left is what a symbol **comes from**; right is what it **goes into**.
//! Group words say how:
//!
//! - a type: `made of`, `made by`, `implemented by` ← T → `taken by`,
//!   `held by`, `calls it`, `used by`;
//! - a callable: `takes`, `called from` ← f → `gives`, `calls`, `used by`.
//!
//! `is` is not a direction: it is the capability line. [`relations_of`] is
//! `app.js` `relationsOf`, entry for entry: each symbol appears once, in
//! the first group that claims it; within a group, yours come first, then
//! by importance. A view drops the groups its own anatomy already shows
//! ([`except`]), and [`prism`] lays the rest into the two named columns the
//! graph's prism and the page's prism both draw.

use super::caps::minimal_derives;
pub use super::model::{Column, Entry, Group, Note, PrismRow, Side, Word};
use crate::graph::model::{Kind, NodeId, Rel, World};
use gpui::SharedString;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

fn is_type_like(kind: Kind) -> bool {
    matches!(kind, Kind::Struct | Kind::Enum | Kind::Trait | Kind::Type | Kind::Union)
}

/// Everything `i` comes from, is, and goes into, grouped (app.js
/// `relationsOf`).
#[must_use]
pub fn relations_of(world: &World, i: NodeId) -> Vec<Group> {
    let node = world.node(i);
    let mut groups: Vec<Group> = Vec::new();
    let mut seen: HashSet<NodeId> = std::iter::once(i).chain(world.kids(i).iter().copied()).collect();
    let rank = |a: &Entry, b: &Entry| -> Ordering {
        let (Some(a), Some(b)) = (a.node, b.node) else { return Ordering::Equal };
        world
            .yours(b)
            .cmp(&world.yours(a))
            .then_with(|| world.importance(b).partial_cmp(&world.importance(a)).unwrap_or(Ordering::Equal))
    };
    let mut add = |word: Word, side: Side, ids: Vec<NodeId>, extra: Vec<Entry>, seen: &mut HashSet<NodeId>| {
        let mut list: Vec<Entry> = Vec::new();
        for j in ids {
            if seen.insert(j) {
                list.push(Entry::node(j));
            }
        }
        list.sort_by(rank);
        let mut entries = extra;
        entries.extend(list);
        if !entries.is_empty() {
            groups.push(Group { word, side, entries });
        }
    };
    if is_type_like(node.kind) {
        let written: Vec<NodeId> = node.impls.iter().filter_map(|imp| imp.trait_).collect();
        let mut extra: Vec<Entry> =
            written.iter().map(|&t| Entry { note: Some(Note::Written), ..Entry::node(t) }).collect();
        seen.extend(written.iter().copied());
        let derives = minimal_derives(&node.derives);
        if !derives.is_empty() {
            extra.push(Entry {
                node: world.outs(i, Rel::DERIVES).first().copied(),
                text: Some(SharedString::from(derives.iter().map(AsRef::as_ref).collect::<Vec<&str>>().join(" · "))),
                note: Some(Note::Derived),
                caps: derives,
            });
        }
        for path in &node.impls_ext {
            extra.push(Entry {
                node: None,
                text: Some(SharedString::from(path.rsplit("::").next().unwrap_or(path).to_owned())),
                note: Some(Note::Written),
                caps: Vec::new(),
            });
        }
        if written.iter().any(|&t| world.node(t).name.as_ref() == "Display") {
            extra.push(Entry {
                node: None,
                text: Some(SharedString::new_static("ToString")),
                note: Some(Note::Via(SharedString::new_static("Display"))),
                caps: Vec::new(),
            });
        }
        add(Word::Is, Side::Is, world.outs(i, Rel::IS), extra, &mut seen);
        add(Word::MadeOf, Side::Left, world.outs(i, Rel::HAS), Vec::new(), &mut seen);
        add(Word::MadeBy, Side::Left, world.ins(i, Rel::GIVES), Vec::new(), &mut seen);
        if node.kind == Kind::Trait {
            add(Word::ImplementedBy, Side::Left, world.ins(i, Rel::IMPL | Rel::DERIVES), Vec::new(), &mut seen);
        }
        add(Word::TakenBy, Side::Right, world.ins(i, Rel::TAKES), Vec::new(), &mut seen);
        add(Word::HeldBy, Side::Right, world.ins(i, Rel::TYPE), Vec::new(), &mut seen);
        let callers: Vec<NodeId> = world.kids(i).iter().flat_map(|&m| world.ins(m, Rel::CALLS)).collect();
        add(Word::CallsIt, Side::Right, callers, Vec::new(), &mut seen);
        let users: Vec<NodeId> = world
            .ins(i, Rel::USES | Rel::CALLS)
            .into_iter()
            .filter(|&j| !world.node(j).parent.is_some_and(|p| seen.contains(&p)))
            .collect();
        add(Word::UsedBy, Side::Right, users, Vec::new(), &mut seen);
    } else {
        let own: Vec<NodeId> = std::iter::once(i).chain(world.kids(i).iter().copied()).collect();
        let from = |rel: Rel| -> Vec<NodeId> { own.iter().flat_map(|&t| world.outs(t, rel)).collect() };
        add(Word::Takes, Side::Left, from(Rel::TAKES), Vec::new(), &mut seen);
        add(Word::CalledFrom, Side::Left, world.ins(i, Rel::CALLS), Vec::new(), &mut seen);
        add(Word::Gives, Side::Right, from(Rel::GIVES), Vec::new(), &mut seen);
        add(Word::Calls, Side::Right, from(Rel::CALLS), Vec::new(), &mut seen);
        add(Word::UsedBy, Side::Right, world.ins(i, Rel::USES | Rel::TAKES | Rel::GIVES | Rel::TYPE), Vec::new(), &mut seen);
    }
    groups
}

/// The entry's display text: its own text, else the symbol's name (a member
/// with its type: `Page::new`, `Page.relations`).
#[must_use]
pub fn label(world: &World, entry: &Entry) -> SharedString {
    match (&entry.text, entry.node) {
        (Some(text), _) => text.clone(),
        (None, Some(node)) => world.name_of(node),
        (None, None) => SharedString::default(),
    }
}

/// The groups a symbol page's anatomy already shows (its parts, its inputs,
/// its output): the page's prism drops them.
pub const PAGE_SHOWS: [Word; 3] = [Word::MadeOf, Word::Takes, Word::Gives];

/// What a page's anatomy (and, on a type page, Getting one) already shows:
/// the page's prism drops these groups.
#[must_use]
pub fn page_shows(kind: Kind) -> Vec<Word> {
    let mut shown = PAGE_SHOWS.to_vec();
    if matches!(kind, Kind::Struct | Kind::Enum | Kind::Union | Kind::Type) {
        shown.push(Word::MadeBy);
    }
    shown
}

/// `groups` without the capability line and without any group whose word a
/// view already shows, so each thing is said once per view.
#[must_use]
pub fn except(groups: Vec<Group>, shown: &[Word]) -> Vec<Group> {
    groups
        .into_iter()
        .filter(|g| g.side != Side::Is && !shown.contains(&g.word))
        .collect()
}

/// Lays `groups` into the prism's two columns (left: comes from, right:
/// goes into), at most `max` rows per group. Rows from another package carry
/// its short name; names shown twice carry their module path.
#[must_use]
pub fn prism(world: &World, i: NodeId, groups: &[Group], max: usize) -> (Vec<Column>, Vec<Column>) {
    let shown: Vec<(&Group, &[Entry])> =
        groups.iter().filter(|g| g.side != Side::Is).map(|g| (g, &g.entries[..g.entries.len().min(max)])).collect();
    let mut counts: HashMap<SharedString, usize> = HashMap::new();
    for (_, entries) in &shown {
        for entry in *entries {
            *counts.entry(label(world, entry)).or_default() += 1;
        }
    }
    let here = world.node(i).pkg;
    let column = |(group, entries): &(&Group, &[Entry])| Column {
        word: group.word,
        rows: entries
            .iter()
            .map(|entry| {
                let text = label(world, entry);
                let note = entry.note.as_ref().map(Note::text).or_else(|| {
                    let node = entry.node?;
                    let pkg = world.node(node).pkg;
                    if pkg != here {
                        Some(SharedString::from(world.package_short(pkg).to_owned()))
                    } else if counts.get(&text).copied().unwrap_or(0) > 1 {
                        let module = world.node(world.top(node)).module;
                        Some(world.modules[module as usize].path.clone()).filter(|p| !p.is_empty())
                    } else {
                        None
                    }
                });
                PrismRow { node: entry.node, kind: entry.node.map(|n| world.node(n).kind), text, note }
            })
            .collect(),
        more: group.entries.len().saturating_sub(max),
    };
    let left = shown.iter().filter(|(g, _)| g.side == Side::Left).map(column).collect();
    let right = shown.iter().filter(|(g, _)| g.side == Side::Right).map(column).collect();
    (left, right)
}
