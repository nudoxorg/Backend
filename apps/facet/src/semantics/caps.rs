//! Capabilities in plain words: what a type can do, and how it came to.
//!
//! Every trait a type implements becomes a word (`Copy` → `copies freely`,
//! `Ord` → `sorts`, `Display` → `prints`) marked with how it arrives:
//! derived, written by hand, or given through another capability
//! (`to text` via `Display`). Derives that another derive implies drop out:
//! `Copy` covers `Clone`, `Eq` covers `PartialEq`, `Ord` covers `PartialOrd`
//! and `Eq`.

use super::types::NodeId;
use gpui::SharedString;

/// How a capability arrives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Arrives {
    /// `#[derive(..)]`: drawn hollow.
    Derived,
    /// An `impl` written by hand: drawn solid.
    Written,
    /// Given through another capability (`to text` via `Display`): dashed.
    Via(SharedString),
}

impl Arrives {
    /// The words (`derived`, `written`, `via Display`).
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Derived => "derived".to_owned(),
            Self::Written => "written".to_owned(),
            Self::Via(from) => format!("via {from}"),
        }
    }
}

/// One capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cap {
    /// The plain word (`copies freely`).
    pub word: SharedString,
    /// The trait's name (`Copy`).
    pub trait_name: SharedString,
    /// How it arrives.
    pub arrives: Arrives,
    /// The trait's symbol, when the world holds it.
    pub node: Option<NodeId>,
}

/// The plain word for a trait (its own name when there is none).
#[must_use]
pub fn word(trait_name: &str) -> &str {
    match trait_name {
        "Copy" => "copies freely",
        "Clone" => "clones",
        "Debug" => "debug-prints",
        "Display" => "prints",
        "PartialEq" | "Eq" => "compares",
        "PartialOrd" => "orders",
        "Ord" => "sorts",
        "Hash" => "hashes",
        "Default" => "has a default",
        "Serialize" => "serializes",
        "Deserialize" => "deserializes",
        "Error" => "is an error",
        "Iterator" => "iterates",
        "From" => "converts",
        "ToString" => "to text",
        "FromStr" => "parses",
        "Send" => "crosses threads",
        "Sync" => "shares across threads",
        "Deref" => "derefs",
        "Drop" => "cleans up",
        "AsRef" => "borrows as",
        other => other,
    }
}

/// What each implied derive is implied by.
const IMPLIED: [(&str, &[&str]); 3] = [("Clone", &["Copy"]), ("PartialEq", &["Eq"]), ("PartialOrd", &["Ord"])];

/// Derives without the ones another derive in the list implies, in order.
/// `Copy` covers `Clone`; `Eq` covers `PartialEq`; `Ord` covers `PartialOrd`
/// and `Eq`.
#[must_use]
pub fn minimal_derives<S: AsRef<str>>(derives: &[S]) -> Vec<SharedString> {
    let has = |name: &str| derives.iter().any(|d| d.as_ref() == name);
    derives
        .iter()
        .map(AsRef::as_ref)
        .filter(|d| {
            let implied = match *d {
                "Eq" => has("Ord"),
                other => IMPLIED
                    .iter()
                    .find(|(name, _)| *name == other)
                    .is_some_and(|(_, by)| by.iter().any(|b| has(b))),
            };
            !implied
        })
        .map(|d| SharedString::from(d.to_owned()))
        .collect()
}

/// The last path segment of a trait path, generics dropped
/// (`std::fmt::Display` → `Display`, `From<u8>` → `From`).
#[must_use]
pub fn trait_name(path: &str) -> &str {
    let last = path.rsplit("::").next().unwrap_or(path);
    last.split('<').next().unwrap_or(last).trim()
}

/// Capabilities from a type's derives, its written impls (trait name and
/// symbol, when known) and impls of traits outside the world, in that
/// order, one per word.
#[must_use]
pub fn caps<D: AsRef<str>, E: AsRef<str>>(
    derives: &[D],
    written: &[(SharedString, Option<NodeId>)],
    external: &[E],
) -> Vec<Cap> {
    let mut out: Vec<Cap> = Vec::new();
    let push = |name: &str, arrives: Arrives, node: Option<NodeId>, out: &mut Vec<Cap>| {
        let word = word(name);
        if out.iter().any(|c| c.word.as_ref() == word) {
            return;
        }
        out.push(Cap {
            word: SharedString::from(word.to_owned()),
            trait_name: SharedString::from(name.to_owned()),
            arrives,
            node,
        });
    };
    for derive in minimal_derives(derives) {
        push(&derive, Arrives::Derived, None, &mut out);
    }
    for (name, node) in written {
        push(name, Arrives::Written, *node, &mut out);
    }
    for path in external {
        push(trait_name(path.as_ref()), Arrives::Written, None, &mut out);
    }
    if out.iter().any(|c| c.trait_name.as_ref() == "Display") {
        push("ToString", Arrives::Via(SharedString::new_static("Display")), None, &mut out);
    }
    out
}
