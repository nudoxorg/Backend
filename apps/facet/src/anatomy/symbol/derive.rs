//! The page's derivation: pure functions from what the shell read
//! ([`Facts`]) to what the page says ([`View`]), and from the places your
//! workspace names it to the verbs and counts on the page.
//!
//! Nothing here touches gpui, the index, or a file: it is tested on real
//! signatures (serde_json, toml, smallvec, a Python and a TypeScript
//! declaration) in milliseconds.

mod callable;
mod docs;
mod rail;
mod shape;
mod text;
pub mod uses;
pub(crate) mod words;

use super::facts::{Facts, SectionKind};
use super::view::{ErrorKind, Fails, Generic, Head, Kind, Lang, Shape, Uses, Verb, View};

/// Markup as plain words: code ticks and link targets dropped.
#[must_use]
pub fn plain_text(markup: &str) -> String {
    text::plain(markup)
}

/// The page for `facts`, without the workspace.
#[must_use]
pub fn compile(facts: &Facts) -> View {
    let derived = callable::callable(facts).filter(|_| facts.kind.callable());
    let (call, generics) = match derived {
        Some(derived) => (Some(derived.call), derived.generics),
        None => (None, Vec::new()),
    };
    let untyped = facts.lang.undeclared() || call.as_ref().is_some_and(|call| call.ports.iter().any(|p| p.ty.origin.dotted()));
    let fails = call.as_ref().and_then(|call| fails_section(facts, call));
    let shape = shape::shape(facts);
    let verbs = shape::verbs(facts);
    View {
        head: Head { kind: facts.kind, path: facts.path.clone(), lang: facts.lang, name: facts.name.clone(), lede: docs::lede(facts) },
        call,
        generics,
        docs: docs::docs(facts),
        fails,
        shape,
        verbs,
        rail: rail::rail(facts, untyped),
    }
}

/// "If it fails": only when there is more to say than the call's row.
fn fails_section(facts: &Facts, call: &super::view::Call) -> Option<Fails> {
    let failure = call.fails.as_ref()?;
    let (body, entries) = docs::errors_prose(facts).unwrap_or_default();
    let full = if body.trim().is_empty() { entries.first().map(|(_, text)| text.clone()).unwrap_or_default() } else { body };
    let reads_input = call.ports.iter().any(|port| {
        let lower = port.ty.word.to_ascii_lowercase();
        lower.contains("reader") || lower.contains("stream") || lower.contains("file") || port.ty.generic.as_deref().is_some_and(|g| g == "R")
    });
    let kinds: Vec<ErrorKind> = facts
        .error_kinds
        .iter()
        .map(|(name, doc)| ErrorKind {
            name: name.clone(),
            doc: text::plain(doc).trim_start_matches("The error was caused by ").trim_end_matches('.').to_owned(),
            impossible: (name == "Io" && !reads_input).then(|| "Io can't happen here: the text is already in memory.".to_owned()),
        })
        .collect();
    (!kinds.is_empty() || full.chars().count() >= 90).then(|| Fails { ty: failure.ty.clone(), when: full, kinds, tells: facts.error_tells.clone() })
}

/// The page with the workspace's places read into it: the counts on cases,
/// fields and methods, and yours first.
#[must_use]
pub fn with_uses(mut view: View, uses: &Uses) -> View {
    let counts = uses.by_member();
    let count_of = |name: &str| counts.iter().find(|(member, _)| member == name).map_or(0, |(_, n)| *n);
    match &mut view.shape {
        Some(Shape::OneOf(cases)) => {
            for case in cases {
                case.yours = count_of(&case.name);
            }
        }
        Some(Shape::Holds { fields, .. }) => {
            for field in fields {
                field.yours = count_of(&field.name);
            }
        }
        _ => {}
    }
    for group in &mut view.verbs {
        for row in &mut group.rows {
            row.yours = match row.name.as_str() {
                "parse" => count_of("parse") + count_of("from_str"),
                name => count_of(name),
            };
        }
        group.rows.sort_by(|a, b| b.yours.cmp(&a.yours));
    }
    view
}

/// The generic parameters as `(name, role)` for a workspace-fill lookup.
#[must_use]
pub fn generic_names(view: &View) -> Vec<&Generic> {
    view.generics.iter().collect()
}

/// Whether the page's language declares no types.
#[must_use]
pub const fn undeclared(lang: Lang) -> bool {
    lang.undeclared()
}

/// The section kinds a page reads (for the shell's mapping).
#[must_use]
pub const fn reads_section(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Errors | SectionKind::Parameters | SectionKind::Returns | SectionKind::Examples | SectionKind::Safety | SectionKind::Deprecated | SectionKind::Panics)
}

/// The verb order of the chips.
#[must_use]
pub fn chip_order(verbs: impl IntoIterator<Item = Verb>) -> Vec<Verb> {
    let set: std::collections::BTreeSet<Verb> = verbs.into_iter().collect();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests;
