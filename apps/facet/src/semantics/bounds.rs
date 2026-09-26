//! Generic bounds as sentences: `T is any Deserialize`.
//!
//! A callable's generic parameters come from its own list, its parent's
//! (an `impl<T>` block's) list and its where-clause, all as source text.
//! [`generics`] gathers them in the prototype's order (`page.js`
//! `genericsOf`); [`Scope::sentence`] writes one parameter's bounds in plain
//! words.

use super::types::{Piece, Scope, TypeExpr, parse, split_top};
use gpui::SharedString;

/// One generic parameter and what it must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generic {
    /// The parameter's name.
    pub name: String,
    /// Bounds as written (`de::Deserialize<'a>`), lifetimes included.
    pub bounds: Vec<String>,
    /// A `const N: usize` parameter: its type.
    pub constant: Option<String>,
}

/// Gathers generic parameters from generic lists (the item's own first,
/// then its parent's) and a where-clause. Lifetimes are dropped; a name
/// seen again is replaced, as the prototype's map does; where-bounds append
/// to their parameter (creating it if the list did not name it).
#[must_use]
pub fn generics(lists: &[&str], where_clause: &str) -> Vec<Generic> {
    let mut out: Vec<Generic> = Vec::new();
    let put = |generic: Generic, out: &mut Vec<Generic>| match out.iter_mut().find(|g| g.name == generic.name) {
        Some(slot) => *slot = generic,
        None => out.push(generic),
    };
    for part in lists.iter().flat_map(|list| split_top(list, ',')) {
        let part = part.trim();
        if part.starts_with('\'') {
            continue;
        }
        let (constant, part) = match part.strip_prefix("const ") {
            Some(rest) => (true, rest.trim()),
            None => (false, part),
        };
        let (name, bound) = match part.find(':') {
            Some(colon) => (part[..colon].trim(), Some(part[colon + 1..].trim())),
            None => (without_default(part), None),
        };
        if !is_ident(name) {
            continue;
        }
        let bound = bound.map(|b| without_default(b).to_owned());
        put(
            Generic {
                name: name.to_owned(),
                bounds: if constant { Vec::new() } else { bound.clone().into_iter().collect() },
                constant: if constant { bound } else { None },
            },
            &mut out,
        );
    }
    for part in split_top(where_clause, ',') {
        let Some(colon) = super::types::name_colon(&part) else { continue };
        let name = part[..colon].trim();
        if !is_ident(name) {
            continue;
        }
        let bound = part[colon + 1..].trim().to_owned();
        match out.iter_mut().find(|g| g.name == name) {
            Some(g) => g.bounds.push(bound),
            None => out.push(Generic { name: name.to_owned(), bounds: vec![bound], constant: None }),
        }
    }
    out
}

/// `Bound = Default` without its default: the first `=` outside `<>`, `()`
/// and `[]` (an `Item = T` binding inside the bound stays).
fn without_default(s: &str) -> &str {
    let mut depth = 0_i32;
    for (k, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            '=' if depth == 0 => return s[..k].trim(),
            _ => {}
        }
    }
    s.trim()
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

impl Scope<'_> {
    /// A parameter's bounds as a sentence after its name:
    /// `is any Deserialize and Clone`, `is any type`, `is a fixed usize`,
    /// `is a function of text → bool`. Lifetimes and `?Sized` drop out.
    #[must_use]
    pub fn sentence(&self, generic: &Generic) -> Vec<Piece> {
        let mut out = Vec::new();
        let word = |w: &str| Piece::Word(SharedString::from(w.to_owned()));
        if let Some(ty) = &generic.constant {
            out.push(word("is a fixed"));
            out.push(Piece::Space);
            out.extend(self.pieces(&parse(ty)));
            return out;
        }
        let bounds: Vec<TypeExpr> = generic
            .bounds
            .iter()
            .flat_map(|b| split_top(b, '+'))
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.starts_with('\'') && !b.starts_with('?'))
            .map(|b| parse(&b))
            .collect();
        if let [only @ TypeExpr::Func { .. }] = bounds.as_slice() {
            out.push(word("is"));
            out.push(Piece::Space);
            out.extend(self.pieces(only));
            return out;
        }
        out.push(word("is any"));
        out.push(Piece::Space);
        if bounds.is_empty() {
            out.push(word("type"));
        }
        for (k, bound) in bounds.iter().enumerate() {
            if k > 0 {
                out.push(Piece::Space);
                out.push(word("and"));
                out.push(Piece::Space);
            }
            out.extend(self.pieces(bound));
        }
        out
    }
}
