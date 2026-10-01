//! The page's derivation: pure functions from what the shell read
//! ([`Facts`]) to what the page says ([`View`]), and from the places your
//! workspace names it to the verbs and counts on the page.
//!
//! Nothing here touches gpui, the index, or a file: it is tested on real
//! signatures (serde_json, toml, smallvec, a Python and a TypeScript
//! declaration) in milliseconds.

mod callable;
mod docs;
mod known;
mod rail;
mod shape;
mod text;
pub mod uses;
pub(crate) mod words;

use super::facts::Facts;
use super::view::{ErrorKind, Fails, Generic, Head, Shape, Uses, View};

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
    let fails = call.as_ref().and_then(|call| fails_section(facts, call, &generics));
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
fn fails_section(facts: &Facts, call: &super::view::Call, generics: &[Generic]) -> Option<Fails> {
    let failure = call.fails.as_ref()?;
    let (body, entries) = docs::errors_prose(facts).unwrap_or_default();
    let full = if body.trim().is_empty() { entries.first().map(|(_, text)| text.clone()).unwrap_or_default() } else { body };
    let kinds: Vec<ErrorKind> = facts
        .error_kinds
        .iter()
        .map(|(name, doc)| ErrorKind {
            name: name.clone(),
            doc: text::lead_out(&text::plain(doc)),
            impossible: known::Source::of(name).filter(|source| !source.possible(call, generics)).map(|source| source.why_not(name)),
        })
        .collect();
    (!kinds.is_empty() || full.chars().count() >= 90).then(|| Fails { ty: failure.ty.clone(), when: full, kinds, tells: facts.error_tells.clone() })
}

/// The page with the workspace's places read into it: the counts on cases,
/// fields and methods, and yours first.
#[must_use]
pub fn with_uses(mut view: View, uses: &Uses) -> View {
    let tally = uses.tally();
    let yours = |name: &str| tally.iter().find(|entry| entry.member == name).map(|entry| entry.yours).unwrap_or_default();
    match &mut view.shape {
        Some(Shape::OneOf(cases)) => {
            for case in cases {
                case.yours = yours(&case.name);
            }
        }
        Some(Shape::Holds { fields, .. }) => {
            for field in fields {
                field.yours = yours(&field.name);
            }
        }
        _ => {}
    }
    for group in &mut view.verbs {
        for row in &mut group.rows {
            row.yours = yours(&row.name).total() + row.also.iter().map(|name| yours(name).total()).sum::<u32>();
        }
        group.rows.sort_by(|a, b| b.yours.cmp(&a.yours));
    }
    view
}

#[cfg(test)]
mod tests;
