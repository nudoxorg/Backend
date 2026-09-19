//! The clickable crumb trail, and the one way this application shortens text.
//!
//! [`backend_present::Identity::trail_within`] renders the trail as one string,
//! which is what a terminal needs. A window needs the same trail as separately
//! navigable steps — `polyglot › src/lib.rs:2 › ferris`, where each step opens
//! something different — so this module splits the same typed identity into
//! steps rather than re-parsing the coordinate. Nothing here invents a part of
//! an identity the shared parser did not find.

use backend_present::{Identity, IdentityShape};

/// One step of a crumb trail, and what following it opens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Crumb {
    /// The project; opens that project's page.
    Project {
        /// Readable project name.
        name: String,
        /// Exact project root, as the producer spelled it.
        root: String,
    },
    /// The source file, with its line when the identity carries one.
    Path {
        /// Package-relative path, with `:line` appended when known.
        label: String,
        /// The package-relative path alone.
        path: String,
    },
    /// One symbol segment; opens that declaration.
    Symbol {
        /// The segment text.
        label: String,
        /// Zero-based depth within the symbol path.
        depth: usize,
        /// The coordinate this segment names, when it is the leaf.
        leaf: bool,
    },
}

impl Crumb {
    /// Returns the text drawn for this step.
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Project { name, .. } => name,
            Self::Path { label, .. } | Self::Symbol { label, .. } => label,
        }
    }

    /// Returns whether following this step reaches somewhere new.
    pub(crate) const fn is_navigable(&self) -> bool {
        match self {
            Self::Project { .. } | Self::Path { .. } => true,
            Self::Symbol { leaf, .. } => !*leaf,
        }
    }
}

/// Splits one identity into its separately navigable steps.
pub(crate) fn trail(identity: &Identity) -> Vec<Crumb> {
    let mut steps = Vec::with_capacity(3);
    if let Some(project) = identity.project() {
        steps.push(Crumb::Project {
            name: project.name().to_owned(),
            root: project.root().to_owned(),
        });
    }
    if let Some(path) = identity.path() {
        steps.push(Crumb::Path {
            label: identity.line().map_or_else(
                || path.as_str().to_owned(),
                |line| format!("{path}:{line}"),
            ),
            path: path.as_str().to_owned(),
        });
    }
    let segments = identity.trail().segments();
    let last = segments.len().saturating_sub(1);
    for (depth, segment) in segments.iter().enumerate() {
        steps.push(Crumb::Symbol {
            label: segment.as_str().to_owned(),
            depth,
            leaf: depth == last,
        });
    }
    if steps.is_empty() {
        steps.push(Crumb::Symbol {
            label: identity.coordinate().as_str().to_owned(),
            depth: 0,
            leaf: true,
        });
    }
    steps
}

/// Returns the dense one-line spelling drawn under a result row.
///
/// The project is dropped when the row is already known to be inside it, so a
/// scoped search does not repeat the scope on every line.
pub(crate) fn compact(identity: &Identity) -> String {
    let within = identity
        .project()
        .filter(|_| identity.shape() != IdentityShape::Package);
    identity.trail_within(within)
}

/// Shortens a long label by eliding its middle, never its ends.
///
/// Truncation is display-only. [`backend_present::Identity::coordinate`] keeps
/// the complete spelling, so a copied path is never the shortened one and a
/// tooltip can always show what the row actually says.
pub(crate) fn elide_middle(text: &str, budget: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= budget || budget < 6 {
        return text.to_owned();
    }
    let keep = budget.saturating_sub(1);
    let head = keep / 2;
    let tail = keep.saturating_sub(head);
    let mut out: String = characters.iter().take(head).collect();
    out.push('…');
    out.extend(characters.iter().skip(characters.len().saturating_sub(tail)));
    out
}
