//! Members in words: parameters, receivers and look-alike folding.
//!
//! - [`params`]: a callable's parameters without its receiver, split into
//!   name and type text.
//! - [`Receiver`]: what a method does to the value it is called on (`reads
//!   it`, `changes it`, `uses it up`, `makes one`), Copy-aware: a by-value
//!   receiver on a type that copies freely only reads it.
//! - [`fold`]: four or more members sharing a name prefix, a result and a
//!   receiver read as one row (`visit_… 22 of them`).

use super::types::name_colon;

/// One parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Its name (`_` or a pattern as written; empty when none was given).
    pub name: String,
    /// Its type, as written.
    pub ty: String,
}

/// Whether a parameter text is the receiver (`self`, `&self`, `&'a mut
/// self`, `mut self`, `self: Box<Self>`).
#[must_use]
pub fn is_receiver(param: &str) -> bool {
    let p = param.trim();
    let p = p.strip_prefix('&').unwrap_or(p).trim_start();
    let p = if p.starts_with('\'') { p.split_once(' ').map_or("", |(_, rest)| rest).trim_start() } else { p };
    let p = p.strip_prefix("mut ").unwrap_or(p).trim_start();
    p == "self" || p.starts_with("self:") || p.starts_with("self :")
}

/// The parameters without the receiver.
#[must_use]
pub fn params<S: AsRef<str>>(raw: &[S]) -> Vec<Param> {
    raw.iter()
        .map(AsRef::as_ref)
        .filter(|p| !is_receiver(p))
        .map(|p| match name_colon(p) {
            Some(colon) => Param {
                name: p[..colon].trim().trim_start_matches("mut ").trim().to_owned(),
                ty: p[colon + 1..].trim().to_owned(),
            },
            None => Param { name: String::new(), ty: p.trim().to_owned() },
        })
        .collect()
}

/// What a method does to the value it is called on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Receiver {
    /// `&self` (or `self` on a type that copies freely).
    Reads,
    /// `&mut self`.
    Changes,
    /// `self`, on a type that does not copy.
    UsesUp,
    /// No receiver: a constructor or a free-standing helper.
    Makes,
}

impl Receiver {
    /// Every receiver, in the order the Does list groups them.
    pub const ORDER: [Self; 4] = [Self::Reads, Self::Changes, Self::UsesUp, Self::Makes];

    /// From the index's receiver word (`reads`, `changes`, `consumes`, none)
    /// and whether the owning type copies freely.
    #[must_use]
    pub fn of(recv: Option<&str>, copies: bool) -> Self {
        match recv.unwrap_or("") {
            "reads" => Self::Reads,
            "changes" => Self::Changes,
            "consumes" if copies => Self::Reads,
            "consumes" => Self::UsesUp,
            _ => Self::Makes,
        }
    }

    /// The Does list's group heading.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Reads => "reads it",
            Self::Changes => "changes it",
            Self::UsesUp => "uses it up",
            Self::Makes => "makes one, or stands alone",
        }
    }

    /// The pipe's first input (`None`: the callable takes no receiver).
    #[must_use]
    pub const fn input(self) -> Option<&'static str> {
        match self {
            Self::Reads => Some("reads it"),
            Self::Changes => Some("changes it"),
            Self::UsesUp => Some("uses it up"),
            Self::Makes => None,
        }
    }
}

/// What folding looks at in one member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look<'a> {
    /// The member's name.
    pub name: &'a str,
    /// Its result, as written.
    pub ret: Option<&'a str>,
    /// Its receiver word.
    pub recv: Option<&'a str>,
}

/// A name's fold prefix: its leading lowercase word and underscore
/// (`visit_` of `visit_bool`), if it has one.
#[must_use]
pub fn prefix(name: &str) -> Option<&str> {
    let head = name.bytes().take_while(u8::is_ascii_lowercase).count();
    (head > 0 && name.as_bytes().get(head) == Some(&b'_')).then(|| &name[..=head])
}

/// The fewest look-alikes that fold into one row.
pub const FOLD_AT: usize = 4;

/// Groups members for display, in first-appearance order: a group of
/// [`FOLD_AT`] or more look-alikes (same prefix, result and receiver) is one
/// entry; every other member is its own entry. Returns indices into `items`.
#[must_use]
pub fn fold(items: &[Look<'_>]) -> Vec<Vec<usize>> {
    fn key<'a>(look: &Look<'a>) -> Option<(&'a str, &'a str, &'a str)> {
        prefix(look.name).map(|p| (p, look.ret.unwrap_or(""), look.recv.unwrap_or("")))
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut done = vec![false; items.len()];
    for k in 0..items.len() {
        if done[k] {
            continue;
        }
        let group: Vec<usize> = match key(&items[k]) {
            Some(own) => (k..items.len()).filter(|&q| !done[q] && key(&items[q]) == Some(own)).collect(),
            None => vec![k],
        };
        if group.len() >= FOLD_AT {
            for &q in &group {
                done[q] = true;
            }
            out.push(group);
        } else {
            done[k] = true;
            out.push(vec![k]);
        }
    }
    out
}
