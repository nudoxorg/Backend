//! Defines session behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the session invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The session tree: every subject any surface opened, nested under what it was opened from.
//!
//! The tree is the tab system. A page opened from another page nests beneath it, a package
//! opened from the gallery nests beneath the gallery node, and a subject an agent opens over MCP
//! lands in the same tree with the agent's mark, so the reader sees the agent's work where their
//! own would be. Spellings here are the line codec every process writes and reads.

use core::{fmt, num::NonZeroU64};

use interface_core::PackageEcosystem;
use interface_documents::{Count, Text};
use interface_identity::{
    Address, AddressParseError, CoordinateParseError, PackageCoordinate, PackageName,
    ecosystem_tag, parse_ecosystem_tag,
};
use interface_search::{QueryText, QueryTextError};

use crate::{ExploreQuery, ExploreQueryError, LibraryEpoch, OwnerHandle, OwnerHandleError, Timestamp};

/// Most nodes one tree holds.
pub const MAX_TREE_NODES: usize = 2048;

/// Stable node identity, minted by the store and never reused within one tree file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TreeNodeId(NonZeroU64);

impl TreeNodeId {
    /// Wraps a nonzero identity.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// The identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Parses the decimal spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        text.parse::<u64>().ok().and_then(NonZeroU64::new).map(Self)
    }
}

impl fmt::Display for TreeNodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// What a node shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeSubject {
    /// A compiled package's root page.
    Package(PackageCoordinate),
    /// One declaration page.
    Page(Address),
    /// A registry package's detail page.
    Registry {
        /// Ecosystem.
        ecosystem: PackageEcosystem,
        /// Exact name.
        name: PackageName,
    },
    /// An owner page.
    Owner {
        /// Ecosystem restriction.
        ecosystem: Option<PackageEcosystem>,
        /// Handle.
        handle: OwnerHandle,
    },
    /// The gallery, possibly filtered.
    Explore {
        /// Ecosystem restriction.
        ecosystem: Option<PackageEcosystem>,
        /// Query.
        query: Option<ExploreQuery>,
    },
    /// A documentation search.
    Search(QueryText),
}

/// Exact subject-spelling failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeSubjectError {
    /// The spelling did not start with a known subject word.
    Kind,
    /// The ecosystem tag was unknown.
    Ecosystem,
    /// The coordinate was refused.
    Coordinate(CoordinateParseError),
    /// The address was refused.
    Address(AddressParseError),
    /// The handle was refused.
    Handle(OwnerHandleError),
    /// The explore query was refused.
    ExploreQuery(ExploreQueryError),
    /// The search query was refused.
    SearchQuery(QueryTextError),
}

impl TreeSubject {
    /// The stable subject word that leads every spelling.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Package(_) => "package",
            Self::Page(_) => "page",
            Self::Registry { .. } => "registry",
            Self::Owner { .. } => "owner",
            Self::Explore { .. } => "explore",
            Self::Search(_) => "search",
        }
    }

    /// Parses the one-line spelling [`fmt::Display`] writes.
    ///
    /// # Errors
    ///
    /// Returns the exact part that refused.
    pub fn parse(text: &str) -> Result<Self, TreeSubjectError> {
        let text = text.trim();
        let (kind, rest) = text.split_once(' ').unwrap_or((text, ""));
        let rest = rest.trim();
        match kind {
            "package" => PackageCoordinate::parse(rest)
                .map(Self::Package)
                .map_err(TreeSubjectError::Coordinate),
            "page" => Address::parse(rest)
                .map(Self::Page)
                .map_err(TreeSubjectError::Address),
            "registry" => {
                let (tag, name) = rest.split_once(':').ok_or(TreeSubjectError::Ecosystem)?;
                let ecosystem = parse_ecosystem_tag(tag).ok_or(TreeSubjectError::Ecosystem)?;
                let name = PackageName::new(name).map_err(TreeSubjectError::Coordinate)?;
                Ok(Self::Registry { ecosystem, name })
            }
            "owner" => {
                let (ecosystem, handle) = optional_ecosystem(rest)?;
                let handle = OwnerHandle::new(handle).map_err(TreeSubjectError::Handle)?;
                Ok(Self::Owner { ecosystem, handle })
            }
            "explore" => {
                let (ecosystem, query) = optional_ecosystem(rest)?;
                let query = if query.is_empty() {
                    None
                } else {
                    Some(ExploreQuery::new(query).map_err(TreeSubjectError::ExploreQuery)?)
                };
                Ok(Self::Explore { ecosystem, query })
            }
            "search" => QueryText::new(rest)
                .map(Self::Search)
                .map_err(TreeSubjectError::SearchQuery),
            _ => Err(TreeSubjectError::Kind),
        }
    }

    /// Infers a subject from whatever a person typed, so a surface can accept `cargo:serde@1.0.196`,
    /// an address, `cargo:serde`, `@dtolnay`, or plain words without asking for a kind first.
    ///
    /// The explicit spelling [`Self::parse`] reads always wins; after that a pinned coordinate is
    /// a package, an address with a path is a page, an ecosystem-qualified name is a registry
    /// entry, a leading `@` is an owner, and anything else is a documentation search.
    ///
    /// # Errors
    ///
    /// Returns the exact refusal of the last grammar tried.
    pub fn infer(text: &str) -> Result<Self, TreeSubjectError> {
        let text = text.trim();
        if let Ok(subject) = Self::parse(text) {
            return Ok(subject);
        }
        if let Ok(coordinate) = PackageCoordinate::parse(text) {
            return Ok(Self::Package(coordinate));
        }
        if let Ok(address) = Address::parse(text) {
            return Ok(if address.path.is_root() {
                Self::Package(address.package)
            } else {
                Self::Page(address)
            });
        }
        if let Some((tag, name)) = text.split_once(':')
            && let Some(ecosystem) = parse_ecosystem_tag(tag)
            && let Ok(name) = PackageName::new(name)
        {
            return Ok(Self::Registry { ecosystem, name });
        }
        if let Some(handle) = text.strip_prefix('@') {
            let handle = OwnerHandle::new(handle).map_err(TreeSubjectError::Handle)?;
            return Ok(Self::Owner {
                ecosystem: None,
                handle,
            });
        }
        QueryText::new(text)
            .map(Self::Search)
            .map_err(TreeSubjectError::SearchQuery)
    }

    /// The title a node takes when the opener supplied none.
    #[must_use]
    pub fn default_title(&self) -> Text {
        Text::new(match self {
            Self::Package(coordinate) => coordinate.name.as_str().to_owned(),
            Self::Page(address) => address
                .path
                .to_string()
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_owned(),
            Self::Registry { name, .. } => name.as_str().to_owned(),
            Self::Owner { handle, .. } => handle.as_str().to_owned(),
            Self::Explore {
                query: Some(query), ..
            } => query.as_str().to_owned(),
            Self::Explore {
                ecosystem: Some(ecosystem),
                query: None,
            } => ecosystem_tag(*ecosystem).as_str().to_owned(),
            Self::Explore {
                ecosystem: None,
                query: None,
            } => "Explore".to_owned(),
            Self::Search(query) => query.as_str().to_owned(),
        })
    }
}

/// Splits an optional leading `ecosystem:` from the rest of a subject spelling; `all:` and an
/// absent tag both mean every ecosystem.
fn optional_ecosystem(rest: &str) -> Result<(Option<PackageEcosystem>, &str), TreeSubjectError> {
    match rest.split_once(':') {
        Some(("all", tail)) => Ok((None, tail)),
        Some((tag, tail)) if !tag.contains(' ') => {
            let ecosystem = parse_ecosystem_tag(tag).ok_or(TreeSubjectError::Ecosystem)?;
            Ok((Some(ecosystem), tail))
        }
        _ => Ok((None, rest)),
    }
}

fn write_optional_ecosystem(
    formatter: &mut fmt::Formatter<'_>,
    ecosystem: Option<PackageEcosystem>,
) -> fmt::Result {
    match ecosystem {
        Some(ecosystem) => write!(formatter, "{}:", ecosystem_tag(ecosystem).as_str()),
        None => formatter.write_str("all:"),
    }
}

impl fmt::Display for TreeSubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ", self.kind())?;
        match self {
            Self::Package(coordinate) => write!(formatter, "{coordinate}"),
            Self::Page(address) => write!(formatter, "{address}"),
            Self::Registry { ecosystem, name } => write!(
                formatter,
                "{}:{}",
                ecosystem_tag(*ecosystem).as_str(),
                name.as_str()
            ),
            Self::Owner { ecosystem, handle } => {
                write_optional_ecosystem(formatter, *ecosystem)?;
                formatter.write_str(handle.as_str())
            }
            Self::Explore { ecosystem, query } => {
                write_optional_ecosystem(formatter, *ecosystem)?;
                match query {
                    Some(query) => formatter.write_str(query.as_str()),
                    None => Ok(()),
                }
            }
            Self::Search(query) => formatter.write_str(query.as_str()),
        }
    }
}

/// Which surface opened a node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Opener {
    /// The desktop reader.
    Gui,
    /// The command line.
    Cli,
    /// An agent over MCP, named by the client it announced at handshake.
    Mcp {
        /// Client name, bounded by the protocol layer.
        client: Text,
    },
}

impl Opener {
    /// Parses `gui`, `cli`, or `mcp:<client>`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "gui" => Some(Self::Gui),
            "cli" => Some(Self::Cli),
            _ => text.strip_prefix("mcp:").map(|client| Self::Mcp {
                client: Text::new(client),
            }),
        }
    }

    /// Whether an agent opened the node.
    #[must_use]
    pub const fn is_agent(&self) -> bool {
        matches!(self, Self::Mcp { .. })
    }
}

impl fmt::Display for Opener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gui => formatter.write_str("gui"),
            Self::Cli => formatter.write_str("cli"),
            Self::Mcp { client } => write!(formatter, "mcp:{client}"),
        }
    }
}

/// One node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeNode {
    /// Identity.
    pub id: TreeNodeId,
    /// The node it was opened from; `None` for a root.
    pub parent: Option<TreeNodeId>,
    /// What it shows.
    pub subject: TreeSubject,
    /// Short label.
    pub title: Text,
    /// Who opened it.
    pub opener: Opener,
    /// When it was opened.
    pub opened_at: Timestamp,
    /// When it was last focused by any surface.
    pub focused_at: Timestamp,
    /// Whether its children are hidden.
    pub collapsed: bool,
}

/// The whole tree at one epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTree {
    /// Nodes in tree order: every parent before its children, siblings by `opened_at`.
    pub nodes: Box<[TreeNode]>,
    /// The node the reader is looking at.
    pub active: Option<TreeNodeId>,
    /// Epoch the tree was read at.
    pub epoch: LibraryEpoch,
}

impl SessionTree {
    /// Finds one node.
    #[must_use]
    pub fn node(&self, id: TreeNodeId) -> Option<&TreeNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Direct children of one node, or the roots for `None`.
    pub fn children(&self, parent: Option<TreeNodeId>) -> impl Iterator<Item = &TreeNode> {
        self.nodes.iter().filter(move |node| node.parent == parent)
    }

    /// How many ancestors one node has; a missing parent ends the count.
    #[must_use]
    pub fn depth(&self, id: TreeNodeId) -> usize {
        let mut depth = 0;
        let mut cursor = self.node(id).and_then(|node| node.parent);
        while let Some(parent) = cursor {
            depth += 1;
            cursor = self.node(parent).and_then(|node| node.parent);
            if depth > MAX_TREE_NODES {
                break;
            }
        }
        depth
    }

    /// Depth-first walk in display order, with each node's depth and whether an ancestor hides it.
    pub fn walk(&self) -> impl Iterator<Item = (&TreeNode, usize, bool)> {
        let mut stack: Vec<(&TreeNode, usize, bool)> =
            self.children(None).collect::<Vec<_>>().into_iter().rev().map(|node| (node, 0, false)).collect();
        core::iter::from_fn(move || {
            let (node, depth, hidden) = stack.pop()?;
            let children: Vec<&TreeNode> = self.children(Some(node.id)).collect();
            let hide_children = hidden || node.collapsed;
            stack.extend(
                children
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth + 1, hide_children)),
            );
            Some((node, depth, hidden))
        })
    }

    /// Nodes an agent opened that the reader has not focused since.
    pub fn unvisited_agent_nodes(&self) -> impl Iterator<Item = &TreeNode> {
        self.nodes
            .iter()
            .filter(|node| node.opener.is_agent() && node.focused_at == node.opened_at)
    }
}

/// One open request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeOpenRequest {
    /// What to open.
    pub subject: TreeSubject,
    /// Where to nest it; `None` opens a root.
    pub parent: Option<TreeNodeId>,
    /// Who is opening.
    pub opener: Opener,
    /// Whether the node becomes the active one.
    pub focus: bool,
    /// Label override; `None` takes [`TreeSubject::default_title`].
    pub title: Option<Text>,
}

/// How much one close removes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TreeCloseScope {
    /// The node alone; its children move up to its parent.
    #[default]
    Node,
    /// The node and every descendant.
    Branch,
}

/// One close request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeCloseRequest {
    /// Which node.
    pub node: TreeNodeId,
    /// How much.
    pub scope: TreeCloseScope,
}

/// Terminal of one tree mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeOutcome {
    /// A node now shows the subject.
    Opened {
        /// The node.
        node: TreeNode,
        /// Whether an existing node under the same parent was reused rather than minted.
        reused: bool,
    },
    /// Nodes were removed.
    Closed {
        /// How many.
        removed: Count,
    },
}

/// Exact tree failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeError {
    /// The tree store failed.
    Store {
        /// Bounded description.
        detail: Box<str>,
    },
    /// No node carries this identity.
    UnknownNode {
        /// What was asked.
        node: TreeNodeId,
    },
    /// The tree is full.
    Full {
        /// Fixed maximum.
        maximum: usize,
    },
}

impl TreeError {
    /// Stable cause slug, the same word on every surface.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::Store { .. } => "tree-store",
            Self::UnknownNode { .. } => "tree-node-unknown",
            Self::Full { .. } => "tree-full",
        }
    }

    /// The exact operand that was refused.
    #[must_use]
    pub fn operand(&self) -> String {
        match self {
            Self::Store { .. } | Self::Full { .. } => String::new(),
            Self::UnknownNode { node } => node.to_string(),
        }
    }

    /// One line in the failure's own words.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Store { detail } => format!("the session tree store failed: {detail}"),
            Self::UnknownNode { .. } => "no tree node carries this identity".to_owned(),
            Self::Full { maximum } => format!("the tree holds its maximum of {maximum} nodes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(spelling: &str) {
        let parsed = TreeSubject::parse(spelling);
        assert_eq!(
            parsed.as_ref().map(ToString::to_string),
            Ok(spelling.to_owned()),
            "{spelling}"
        );
    }

    #[test]
    fn every_subject_spelling_round_trips_through_the_line_codec() {
        round_trip("package cargo:serde@1.0.196");
        round_trip("page cargo:serde@1.0.196::de::Deserializer[trait]");
        round_trip("registry npm:@types/node");
        round_trip("owner cargo:dtolnay");
        round_trip("owner all:dtolnay");
        round_trip("explore all:");
        round_trip("explore cargo:async runtime");
        round_trip("search deserialize map");
        assert_eq!(TreeSubject::parse("bogus x"), Err(TreeSubjectError::Kind));
    }

    #[test]
    fn inference_reads_what_a_person_would_type() {
        assert!(matches!(TreeSubject::infer("cargo:serde@1.0.196"), Ok(TreeSubject::Package(_))));
        assert!(matches!(
            TreeSubject::infer("cargo:serde@1.0.196::de::Deserializer"),
            Ok(TreeSubject::Page(_))
        ));
        assert!(matches!(TreeSubject::infer("npm:@types/node"), Ok(TreeSubject::Registry { .. })));
        assert!(matches!(TreeSubject::infer("@dtolnay"), Ok(TreeSubject::Owner { .. })));
        assert!(matches!(TreeSubject::infer("async runtime"), Ok(TreeSubject::Search(_))));
        assert!(matches!(TreeSubject::infer("explore cargo:"), Ok(TreeSubject::Explore { .. })));
    }

    #[test]
    fn openers_and_titles_are_derived_not_invented() {
        assert_eq!(Opener::parse("mcp:claude-code"), Some(Opener::Mcp { client: Text::new("claude-code") }));
        assert_eq!(Opener::parse("web"), None);
        let Ok(subject) = TreeSubject::parse("page cargo:serde@1.0.196::de::Deserializer") else {
            return;
        };
        assert_eq!(subject.default_title().as_str(), "Deserializer");
    }

    #[test]
    fn the_walk_hides_collapsed_branches_and_keeps_tree_order() {
        let Ok(a) = TreeSubject::parse("package cargo:a@1") else { return };
        let Ok(b) = TreeSubject::parse("package cargo:b@1") else { return };
        let Ok(c) = TreeSubject::parse("package cargo:c@1") else { return };
        let node = |id: u64, parent: Option<u64>, subject: TreeSubject, collapsed: bool| TreeNode {
            id: TreeNodeId::parse(&id.to_string()).unwrap_or(TreeNodeId(NonZeroU64::MIN)),
            parent: parent.and_then(|parent| TreeNodeId::parse(&parent.to_string())),
            title: subject.default_title(),
            subject,
            opener: Opener::Gui,
            opened_at: Timestamp(id),
            focused_at: Timestamp(id),
            collapsed,
        };
        let tree = SessionTree {
            nodes: Box::new([node(1, None, a, true), node(2, Some(1), b, false), node(3, None, c, false)]),
            active: None,
            epoch: LibraryEpoch(1),
        };
        let walked: Vec<(u64, usize, bool)> = tree
            .walk()
            .map(|(node, depth, hidden)| (node.id.get(), depth, hidden))
            .collect();
        assert_eq!(walked, vec![(1, 0, false), (2, 1, true), (3, 0, false)]);
        assert_eq!(tree.depth(TreeNodeId(NonZeroU64::new(2).unwrap_or(NonZeroU64::MIN))), 1);
    }
}
