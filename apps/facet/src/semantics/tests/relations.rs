//! `relations_of` against the prototype: `relations.golden` is app.js
//! `relationsOf`, verbatim, over `world.js` (with the prototype's own
//! importance), for the four target pages and every 997th symbol.

use super::world::{find, world};
use crate::graph::model::{Kind, NodeId};
use crate::semantics::relations::{
    Group, Note, PAGE_SHOWS, Side, Word, except, page_shows, prism, relations_of,
};

fn line(i: NodeId, g: &Group) -> String {
    let w = world();
    let side = match g.side {
        Side::Left => -1,
        Side::Is => 0,
        Side::Right => 1,
    };
    let node = |n: Option<NodeId>| n.map_or_else(|| "-1".to_owned(), |n| n.to_string());
    let entries: Vec<String> = g
        .entries
        .iter()
        .map(|e| match (&e.text, &e.note) {
            (Some(text), Some(note)) => format!("{text}~{}~{}", note.text(), node(e.node)),
            (Some(text), None) => format!("{text}~~{}", node(e.node)),
            (None, Some(note)) => format!("{}~{}", node(e.node), note.text()),
            (None, None) => node(e.node),
        })
        .collect();
    format!(
        "{i}|{}|{}|{side}|{}",
        w.node(i).name,
        g.word.text(),
        entries.join(",")
    )
}

#[test]
fn relations_match_the_prototype_entry_for_entry() {
    let golden = include_str!("relations.golden");
    let mut want: Vec<&str> = Vec::new();
    let mut got: Vec<String> = Vec::new();
    let mut symbols = 0;
    for l in golden.lines().filter(|l| !l.contains("|callers|")) {
        want.push(l);
        if let Some(head) = l.strip_suffix("|end") {
            symbols += 1;
            let i: NodeId = head
                .split('|')
                .next()
                .and_then(|s| s.parse().ok())
                .expect("an id");
            for g in relations_of(world(), i) {
                got.push(line(i, &g));
            }
            got.push(format!("{head}|end"));
        }
    }
    assert!(symbols >= 60, "the golden covers {symbols} symbols");
    let diffs: Vec<String> = want
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| **a != b.as_str())
        .map(|(a, b)| format!("want {a}\n got  {b}"))
        .collect();
    assert!(
        diffs.is_empty() && want.len() == got.len(),
        "{} of {} lines differ:\n{}",
        diffs.len(),
        want.len(),
        diffs.join("\n")
    );
}

#[test]
fn a_type_is_its_capabilities_and_its_parts_come_first() {
    let w = world();
    let label = find("present::glyph::RelationLabel");
    let groups = relations_of(w, label);
    let words: Vec<&str> = groups.iter().map(|g| g.word.text()).collect();
    assert_eq!(words, ["is", "made of", "made by", "taken by", "held by"]);
    let is = &groups[0];
    assert_eq!(is.side, Side::Is);
    assert_eq!(is.entries[0].note, Some(Note::Written));
    assert_eq!(
        w.node(is.entries[0].node.expect("Display")).name.as_ref(),
        "Display"
    );
    assert_eq!(
        is.entries[1].text.as_deref(),
        Some("Copy · Debug · Hash · Ord")
    );
    assert_eq!(is.entries[2].note, Some(Note::Via("Display".into())));
}

#[test]
fn the_page_drops_what_its_anatomy_shows_and_the_capability_line() {
    let w = world();
    let label = find("present::glyph::RelationLabel");
    let words: Vec<Word> = except(relations_of(w, label), &PAGE_SHOWS)
        .iter()
        .map(|g| g.word)
        .collect();
    assert_eq!(words, [Word::MadeBy, Word::TakenBy, Word::HeldBy]);
    let from_str = find("serde_json::de::from_str");
    let words: Vec<Word> = except(relations_of(w, from_str), &PAGE_SHOWS)
        .iter()
        .map(|g| g.word)
        .collect();
    assert_eq!(words, [Word::CalledFrom, Word::Calls]);
}

fn column_text(cols: &[crate::semantics::relations::Column]) -> Vec<String> {
    cols.iter()
        .map(|c| {
            let rows: Vec<String> = c
                .rows
                .iter()
                .map(|r| match &r.note {
                    Some(note) => format!("{} ({note})", r.text),
                    None => r.text.to_string(),
                })
                .collect();
            let more = if c.more > 0 {
                format!(" +{}", c.more)
            } else {
                String::new()
            };
            format!("{}: {}{more}", c.word.text(), rows.join(", "))
        })
        .collect()
}

#[test]
fn relation_groups_prism_names_its_columns_and_tells_duplicates_apart() {
    let w = world();
    let group = find("present::page::RelationGroup");
    let groups = except(relations_of(w, group), &PAGE_SHOWS);
    let (left, right) = prism(w, group, &groups, 5);
    assert_eq!(
        column_text(&left),
        ["made by: relation_groups, Page::relations"]
    );
    assert_eq!(
        column_text(&right),
        [
            "taken by: push_relations (render::markdown), push_relations (render::text), Page::with_relations",
            "held by: Page.relations",
            "calls it: PageDto::new",
        ]
    );
}

#[test]
fn visitors_prism_notes_other_packages_and_counts_the_rest() {
    let w = world();
    let visitor = find("serde_core::de::Visitor");
    let groups = except(relations_of(w, visitor), &PAGE_SHOWS);
    let (left, right) = prism(w, visitor, &groups, 5);
    assert_eq!(
        column_text(&left),
        [
            "implemented by: OptionVisitor, ArrayVisitor, OsStringVisitor, FromStrVisitor, RangeVisitor +22"
        ]
    );
    assert_eq!(
        column_text(&right)[0],
        "calls it: U32Deserializer::deserialize_any"
    );
    assert_eq!(
        column_text(&right)[1],
        "used by: Value::deserialize (serde_json), Map::deserialize (serde_json), RawValue::deserialize (serde_json), deserialize, deserialize_in_place +12"
    );
}

#[test]
fn in_use_ranks_callers_as_the_prototype_does() {
    let golden = include_str!("relations.golden");
    let mut checked = 0;
    for l in golden.lines().filter(|l| l.contains("|callers|")) {
        let mut parts = l.split('|');
        let i: NodeId = parts.next().and_then(|s| s.parse().ok()).expect("an id");
        let want = parts.nth(2).unwrap_or_default();
        let got: Vec<String> = crate::semantics::page::callers(world(), i)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(got.join(","), want, "callers of {}", world().node(i).name);
        checked += 1;
    }
    assert_eq!(checked, 4);
}

#[test]
fn page_discovery_counts_the_whole_pinned_neighborhood_before_disclosure() {
    use crate::semantics::relations::rows::{Band, rows};
    let w = world();
    let visitor = find("serde_core::de::Visitor");
    let rows = rows(w, visitor, relations_of(w, visitor));
    let implementations = rows
        .iter()
        .find(|row| row.word == Word::ImplementedBy)
        .expect("implementations");
    assert_eq!(
        implementations.count(),
        "27 relations, 0 of yours, in 4 packages"
    );
    assert_eq!(
        implementations.invitation(3),
        "done by: OptionVisitor, ArrayVisitor, OsStringVisitor and 24 more"
    );
    assert_eq!(
        implementations.invitation(1),
        "done by: OptionVisitor and 26 more"
    );
    assert_eq!(
        implementations
            .scopes
            .iter()
            .map(|scope| scope.band)
            .collect::<Vec<_>>(),
        [Band::Here, Band::Elsewhere]
    );
    assert_eq!(implementations.scopes[0].packages[0].names.len(), 19);
    assert_eq!(
        implementations.scopes[1]
            .packages
            .iter()
            .map(|package| package.names.len())
            .sum::<usize>(),
        8
    );
    assert_eq!(
        implementations
            .scopes
            .iter()
            .flat_map(|scope| &scope.packages)
            .flat_map(|package| &package.names)
            .count(),
        27
    );
}

#[test]
fn producer_relations_are_not_claimed_by_shape_anatomy() {
    // The fork says its parts. Getting one is responsible for producers;
    // removing MadeBy with the shape omissions would lose those facts.
    assert!(!PAGE_SHOWS.contains(&Word::MadeBy));
    let w = world();
    let label = find("present::glyph::RelationLabel");
    let remaining = except(relations_of(w, label), &PAGE_SHOWS);
    let producers = remaining
        .iter()
        .find(|group| group.word == Word::MadeBy)
        .expect("producers");
    let identities: Vec<_> = producers
        .entries
        .iter()
        .map(|entry| {
            let node = w.node(entry.node.expect("pinned producer identity"));
            (
                node.name.as_ref(),
                node.file.as_deref().map(|file| file.as_ref()),
                node.line,
            )
        })
        .collect();
    assert_eq!(
        identities,
        [
            ("relation_label", Some("crates/present/glyph.rs"), 170),
            ("label", Some("crates/present/page.rs"), 235)
        ]
    );
}

#[test]
fn a_type_page_drops_producers_only_because_getting_one_covers_them() {
    let w = world();
    let kind = w.node(find("present::glyph::RelationLabel")).kind;
    assert_eq!(kind, Kind::Enum);

    let raw = relations_of(w, find("present::glyph::RelationLabel"));
    assert!(
        raw.iter().any(|group| group.word == Word::MadeBy),
        "the pinned enum has a producer row"
    );

    let shown = page_shows(kind);
    assert!(
        shown.contains(&Word::MadeBy),
        "the enum's Getting one section owns producer coverage"
    );
    assert!(
        !page_shows(Kind::Function).contains(&Word::MadeBy),
        "functions do not claim type producer coverage"
    );
    let prism = except(raw, &shown);
    assert!(
        !prism.iter().any(|group| group.word == Word::MadeBy),
        "the type page does not repeat the producer row in its prism"
    );
}
