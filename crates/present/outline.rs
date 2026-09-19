//! A package outline as a tree of names.
//!
//! The engine answers an outline query with a tree of [`SymbolKey`] values and
//! nothing else — the shape is there, the words are not. Rendering that tree
//! directly is what produced today's unusable output: eighteen lines of bare
//! hexadecimal. So the model requires a resolver: an outline is only built
//! once every node has been matched to a row that carries its coordinate, and
//! a node that cannot be matched says so with its tag rather than pretending
//! to be a name.

use crate::identity::{Identity, IdentityKey, KeyTag};
use backend_library::{DeclarationKind, OutlineExtent, OutlineNode, SymbolKey};

use crate::page::Truncation;

/// One node of an outline, with the name it resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineEntry {
    identity: Option<Identity>,
    tag: KeyTag,
    kind: Option<DeclarationKind>,
    children: Box<[OutlineEntry]>,
}

impl OutlineEntry {
    /// Resolves one outline node and its children through a row resolver.
    #[must_use]
    pub fn resolve(
        node: &OutlineNode,
        resolve: &mut impl FnMut(SymbolKey) -> Option<(Identity, Option<DeclarationKind>)>,
    ) -> Self {
        let resolved = resolve(node.symbol);
        let children = node
            .children
            .iter()
            .map(|child| Self::resolve(child, resolve))
            .collect();
        Self {
            tag: KeyTag::from_key(node.symbol.as_bytes()),
            identity: resolved.as_ref().map(|(identity, _)| identity.clone()),
            kind: resolved.and_then(|(_, kind)| kind),
            children,
        }
    }

    /// Returns the resolved identity, when the node matched a row.
    #[must_use]
    pub const fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Returns the display abbreviation of this node's key.
    #[must_use]
    pub const fn tag(&self) -> KeyTag {
        self.tag
    }

    /// Returns the node's declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the child nodes in outline order.
    #[must_use]
    pub fn children(&self) -> &[OutlineEntry] {
        &self.children
    }

    /// Returns the displayed name, falling back to the key tag.
    #[must_use]
    pub fn name(&self) -> String {
        self.identity
            .as_ref()
            .map_or_else(|| format!("‹{}›", self.tag), |identity| identity.name().to_owned())
    }

    /// Returns the number of nodes in this subtree, including itself.
    #[must_use]
    pub fn count(&self) -> usize {
        self.children
            .iter()
            .fold(1_usize, |total, child| total.saturating_add(child.count()))
    }

    /// Returns whether every node in this subtree resolved to a name.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.identity.is_some() && self.children.iter().all(Self::is_resolved)
    }
}

/// One package outline, resolved to names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineTree {
    package: Identity,
    roots: Box<[OutlineEntry]>,
    truncation: Truncation,
}

impl OutlineTree {
    /// Records one resolved outline.
    #[must_use]
    pub fn new(
        package: Identity,
        roots: impl Into<Box<[OutlineEntry]>>,
        extent: OutlineExtent,
    ) -> Self {
        Self {
            package,
            roots: roots.into(),
            truncation: match extent {
                OutlineExtent::Complete => Truncation::Complete,
                OutlineExtent::Truncated => Truncation::Truncated,
            },
        }
    }

    /// Returns the package this outline describes.
    #[must_use]
    pub const fn package(&self) -> &Identity {
        &self.package
    }

    /// Returns every top-level node.
    #[must_use]
    pub fn roots(&self) -> &[OutlineEntry] {
        &self.roots
    }

    /// Returns whether the bounded response carried the complete outline.
    #[must_use]
    pub const fn truncation(&self) -> Truncation {
        self.truncation
    }

    /// Returns the total number of nodes.
    #[must_use]
    pub fn count(&self) -> usize {
        self.roots
            .iter()
            .fold(0_usize, |total, root| total.saturating_add(root.count()))
    }

    /// Returns the number of nodes that did not resolve to a name.
    #[must_use]
    pub fn unresolved(&self) -> usize {
        fn walk(entry: &OutlineEntry) -> usize {
            entry
                .children()
                .iter()
                .fold(usize::from(entry.identity().is_none()), |total, child| {
                    total.saturating_add(walk(child))
                })
        }
        self.roots
            .iter()
            .fold(0_usize, |total, root| total.saturating_add(walk(root)))
    }
}

/// Builds a resolver over a row slice keyed by its symbol identity.
pub fn row_resolver(
    rows: &[backend_library::Row],
) -> impl FnMut(SymbolKey) -> Option<(Identity, Option<DeclarationKind>)> + '_ {
    move |symbol| {
        rows.iter()
            .find(|row| row.id == backend_library::RowId::Symbol(symbol))
            .map(|row| {
                (
                    Identity::parse_with_key(&row.label, IdentityKey::Symbol(symbol)),
                    row.kind,
                )
            })
    }
}
