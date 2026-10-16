//! Borrowed spelling of the existing catalog reply grammar.
//! Rows and fragments stream in order; no payload-sized wire copy is built.
use super::super::reply_content::FactWire;
use super::*;
use crate::{
    DeclarationFacts, Fact, Fragment, SourceAvailability, SourceExcerpt, SourceExcerptExtent,
};
use serde::ser::SerializeSeq;

#[derive(Serialize)]
struct Envelope<'a> {
    version: u16,
    request_id: u64,
    reply: CatalogReply<'a>,
    certificate: Option<&'a WireCertificate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_search: Option<SemanticSearchStatus>,
}
#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum CatalogReply<'a> {
    Names(Snapshot<'a>),
    Search(Snapshot<'a>),
    ProjectionPage(Page<'a>),
}
#[derive(Serialize)]
struct Page<'a> {
    snapshot: Snapshot<'a>,
    terminal: PageTerminalWire,
}
#[derive(Serialize)]
struct Snapshot<'a> {
    root: Root<'a>,
    freshness: FreshnessWire,
    next: Option<CursorWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_relations: Option<Relations<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rich_graph: Option<&'a crate::RichGraphSnapshot>,
}
#[derive(Serialize)]
struct Root<'a> {
    recipe: String,
    version: String,
    root: String,
    basis: BasisWire,
    frontier: FrontierWire,
    rows: Rows<'a>,
    coverage: Coverages<'a>,
}
struct Rows<'a>(&'a [Row]);
impl Serialize for Rows<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            sequence.serialize_element(&RowRef::new(row))?;
        }
        sequence.end()
    }
}
struct Coverages<'a>(&'a [crate::Coverage]);
impl Serialize for Coverages<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for coverage in self.0 {
            sequence.serialize_element(&coverage_to_wire(*coverage))?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct Relations<'a> {
    schema: u16,
    relations: Edges<'a>,
}
struct Edges<'a>(&'a [GraphRelation]);
impl Serialize for Edges<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for edge in self.0 {
            sequence.serialize_element(&GraphRelationWire {
                from: row_id_to_wire(edge.from),
                to: row_id_to_wire(edge.to),
                relation: edge.relation,
                confidence: edge.confidence,
            })?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct RowRef<'a> {
    id: RowIdWire,
    basis: BasisWire,
    state: RowStateWire,
    label: &'a str,
    score: Option<u32>,
    package: Option<String>,
    parent: Option<String>,
    document: Fragments<'a>,
    signature: Option<&'a str>,
    kind: Option<&'static str>,
    source: Source<'a>,
    excerpt: Excerpt<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    facts: Option<Facts<'a>>,
}
impl<'a> RowRef<'a> {
    fn new(row: &'a Row) -> Self {
        Self {
            id: row_id_to_wire(row.id),
            basis: basis_to_wire(row.basis),
            state: row_state_to_wire(row.state),
            label: &row.label,
            score: row.score,
            package: row.package.map(|key| encode_id(key.as_bytes())),
            parent: row.parent.map(|key| encode_id(key.as_bytes())),
            document: Fragments(&row.document),
            signature: row.signature.as_deref(),
            kind: row.kind.map(|kind| kind.name()),
            source: Source::new(&row.source),
            excerpt: Excerpt::new(&row.excerpt),
            facts: Facts::new(&row.facts),
        }
    }
}
struct Fragments<'a>(&'a [Fragment]);
impl Serialize for Fragments<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for fragment in self.0 {
            sequence.serialize_element(&FragmentRef::new(fragment))?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum FragmentRef<'a> {
    Text(Text<'a>),
    Code(Text<'a>),
    Link(Link<'a>),
    Break(EmptyWire),
}
#[derive(Serialize)]
struct Text<'a> {
    text: &'a str,
}
#[derive(Serialize)]
struct Link<'a> {
    label: &'a str,
    target: String,
}
impl<'a> FragmentRef<'a> {
    fn new(fragment: &'a Fragment) -> Self {
        match fragment {
            Fragment::Text(text) => Self::Text(Text { text }),
            Fragment::Code(text) => Self::Code(Text { text }),
            Fragment::Link { label, target } => Self::Link(Link {
                label,
                target: encode_id(target.as_bytes()),
            }),
            Fragment::Break => Self::Break(EmptyWire {}),
        }
    }
}
#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum Source<'a> {
    Captured(Location<'a>),
    NotCaptured,
    NotHydrated,
    Unconfigured,
    StaleFile { path: &'a str },
}
#[derive(Serialize)]
struct Location<'a> {
    path: &'a str,
    start_line: u32,
}
impl<'a> Source<'a> {
    fn new(source: &'a SourceAvailability) -> Self {
        match source {
            SourceAvailability::Captured(location) => Self::Captured(Location {
                path: location.path(),
                start_line: location.start_line(),
            }),
            SourceAvailability::NotCaptured => Self::NotCaptured,
            SourceAvailability::NotHydrated => Self::NotHydrated,
            SourceAvailability::Unconfigured => Self::Unconfigured,
            SourceAvailability::StaleFile { path } => Self::StaleFile { path },
        }
    }
}
#[derive(Serialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
enum Excerpt<'a> {
    Captured(ExcerptText<'a>),
    NotCaptured,
    NotHydrated,
    Unconfigured,
}
#[derive(Serialize)]
struct ExcerptText<'a> {
    text: &'a str,
    extent: &'static str,
}
impl<'a> Excerpt<'a> {
    fn new(excerpt: &'a SourceExcerpt) -> Self {
        match excerpt {
            SourceExcerpt::Captured { text, extent } => Self::Captured(ExcerptText {
                text,
                extent: match extent {
                    SourceExcerptExtent::Complete => "complete",
                    SourceExcerptExtent::Truncated => "truncated",
                },
            }),
            SourceExcerpt::NotCaptured => Self::NotCaptured,
            SourceExcerpt::NotHydrated => Self::NotHydrated,
            SourceExcerpt::Unconfigured => Self::Unconfigured,
        }
    }
}
#[derive(Serialize)]
struct Facts<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecation: Option<FactWire<Notice<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    obligation: Option<FactWire<&'a str>>,
}
#[derive(Serialize)]
struct Notice<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}
impl<'a> Facts<'a> {
    fn new(facts: &'a DeclarationFacts) -> Option<Self> {
        if facts.is_unobserved() {
            return None;
        }
        Some(Self {
            deprecation: match &facts.deprecation {
                Fact::Unobserved => None,
                Fact::Absent => Some(FactWire::Absent),
                Fact::Present(notice) => Some(FactWire::Present(Notice {
                    since: notice.since(),
                    note: notice.note(),
                })),
            },
            obligation: match &facts.obligation {
                Fact::Unobserved => None,
                Fact::Absent => Some(FactWire::Absent),
                Fact::Present(obligation) => Some(FactWire::Present(obligation.name())),
            },
        })
    }
}
impl<'a> Snapshot<'a> {
    fn new(snapshot: &'a ViewSnapshot) -> Self {
        let root = &snapshot.root;
        Self {
            root: Root {
                recipe: encode_id(root.recipe().as_bytes()),
                version: encode_id(root.version().as_bytes()),
                root: encode_id(root.root().as_bytes()),
                basis: basis_to_wire(root.basis()),
                frontier: frontier_to_wire(root.frontier()),
                rows: Rows(root.rows()),
                coverage: Coverages(root.coverage()),
            },
            freshness: freshness_to_wire(snapshot.freshness),
            next: snapshot.next.map(cursor_to_wire),
            graph_relations: snapshot.graph_relations.as_deref().map(|edges| Relations {
                schema: GRAPH_RELATIONS_SCHEMA,
                relations: Edges(edges),
            }),
            rich_graph: snapshot.rich_graph.as_ref(),
        }
    }
}

pub(super) fn serialize<S: serde::Serializer>(
    reply: &ReplyDto,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let body = match &reply.reply {
        CommandReply::Names(snapshot) => CatalogReply::Names(Snapshot::new(snapshot)),
        CommandReply::Search(snapshot) => CatalogReply::Search(Snapshot::new(snapshot)),
        CommandReply::ProjectionPage(page) => CatalogReply::ProjectionPage(Page {
            snapshot: Snapshot::new(&page.snapshot),
            terminal: match page.terminal {
                PageTerminal::Complete => PageTerminalWire::Complete(EmptyWire {}),
                PageTerminal::More(cursor) => {
                    PageTerminalWire::More(cursor_to_wire(cursor.cursor()))
                }
                PageTerminal::Cancelled => PageTerminalWire::Cancelled(EmptyWire {}),
            },
        }),
        _ => {
            return Err(serde::ser::Error::custom(
                "catalog serializer received another reply family",
            ));
        }
    };
    Envelope {
        version: DTO_VERSION,
        request_id: reply.request_id,
        reply: body,
        certificate: reply.certificate(),
        semantic_search: reply.semantic_search_status(),
    }
    .serialize(serializer)
}
