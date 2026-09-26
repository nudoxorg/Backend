//! Engine replies → page read models.
//!
//! Every function here is pure over already-admitted library values: no
//! socket, no clock, no filesystem. The read pool calls them on its worker
//! threads, so the UI thread only ever receives finished models. Each one
//! keeps the engine's availability distinctions: a reply that could not
//! answer becomes a typed [`Gap`], never an empty list.

use crate::model::local_package::{DependencyKind, LocalPackage};
use crate::model::pages::{
    AdvisorySummary, Arrival, ByteSpan, DeclRef, Dependency, DependencyScope, Derivation,
    DocFragment, Downloads, Excerpt, FaultProgress, FileSpan, Gap, GapReason, HealthModel,
    IdentifierSpan, IndexedPackage, IngestModel, Known, LanguageProgress, LineSpan, MatchReason,
    Member, Members, MethodGroup, OrbitModel, OrbitProject, OutlineNode, OutlinePosition,
    OutlineTree, PackageDossier, PackageRecord, PackageRef, Provenance, Readiness, Receiver,
    RecordSource, ReferenceScope, ReferenceSite, Relation, RelationKind, Rose, SearchContinuation,
    SearchPage, SearchRow, SignatureText, SignatureToken, SourceLocation, SourceOrigin,
    SourceSite, SourceText, SourceView, Standing, SymbolLink, SymbolPage, SymbolRef, TokenClass,
    TreeNode, TreeOpener, TreeSubject, VersionEntry,
};
use backend_client::ClientError;
use backend_library::{
    DeclarationKind, Document, Fragment, GraphEdgeKind, GraphNodeId, GraphRelation, HealthReport,
    RegistryDownloadCount, RegistryFactAvailability, RegistryPackageRecord,
    RegistryReleaseStanding, Row, RowId, SemanticConfidence, SemanticLinkKind,
    SemanticLinkTarget, SourceAvailability, SourceExcerpt, SourceExcerptExtent, SurfaceReply,
    SymbolKey, ViewSnapshot,
};
use backend_present::{CoverageLine, Language, TokenKind};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Outline index
// ---------------------------------------------------------------------------

/// Every outline row of one package, indexed for members, ancestry, name
/// resolution, and the mosaic tree.
#[derive(Debug)]
pub struct OutlineIndex {
    rows: Vec<Row>,
    by_key: HashMap<SymbolKey, usize>,
    by_label: HashMap<String, usize>,
    children: HashMap<Option<SymbolKey>, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    complete: bool,
}

impl OutlineIndex {
    /// Indexes one package's flat outline rows. `complete` is whether every
    /// producer page was read.
    #[must_use]
    pub fn new(rows: Vec<Row>, complete: bool) -> Self {
        let mut by_key = HashMap::with_capacity(rows.len());
        let mut by_label = HashMap::with_capacity(rows.len());
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            if let RowId::Symbol(key) = row.id {
                by_key.insert(key, index);
            }
            by_label.insert(row.label.clone(), index);
            by_name
                .entry(leaf_name(&row.label).to_owned())
                .or_default()
                .push(index);
        }
        let mut children: HashMap<Option<SymbolKey>, Vec<usize>> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            let parent = row.parent.filter(|parent| by_key.contains_key(parent));
            children.entry(parent).or_default().push(index);
        }
        for list in children.values_mut() {
            list.sort_by(|left, right| outline_order(&rows[*left], &rows[*right]));
        }
        Self {
            rows,
            by_key,
            by_label,
            children,
            by_name,
            complete,
        }
    }

    /// Returns whether every outline page was read.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the number of indexed rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns whether the index holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns one row by key.
    #[must_use]
    pub fn row(&self, key: SymbolKey) -> Option<&Row> {
        self.by_key.get(&key).map(|index| &self.rows[*index])
    }

    /// Returns one row by exact coordinate.
    #[must_use]
    pub fn row_by_label(&self, label: &str) -> Option<&Row> {
        self.by_label.get(label).map(|index| &self.rows[*index])
    }

    /// Returns the children of `parent` in outline order.
    pub fn children(&self, parent: SymbolKey) -> impl Iterator<Item = &Row> {
        self.children
            .get(&Some(parent))
            .into_iter()
            .flatten()
            .map(|index| &self.rows[*index])
    }

    fn parent_of(&self, key: SymbolKey) -> Option<SymbolKey> {
        self.row(key)?
            .parent
            .filter(|parent| self.by_key.contains_key(parent))
    }

    /// Returns the root-first ancestor chain of `key`.
    #[must_use]
    pub fn ancestors(&self, key: SymbolKey) -> Vec<&Row> {
        let mut chain = Vec::new();
        let mut current = self.parent_of(key);
        while let Some(parent) = current {
            if chain.len() > self.rows.len() {
                break;
            }
            let Some(row) = self.row(parent) else {
                break;
            };
            chain.push(row);
            current = self.parent_of(parent);
        }
        chain.reverse();
        chain
    }

    /// Resolves an identifier to one declaration by name, preferring
    /// nominal types and contracts. Ambiguous names resolve to nothing.
    #[must_use]
    pub fn resolve_name(&self, name: &str, accept: impl Fn(Option<DeclarationKind>) -> bool) -> Option<&Row> {
        let candidates = self.by_name.get(name)?;
        let mut best: Option<(u8, &Row)> = None;
        let mut tie = false;
        for index in candidates {
            let row = &self.rows[*index];
            if !accept(row.kind) || is_encoded_signature(row.signature.as_deref().unwrap_or("")) {
                continue;
            }
            let rank = name_rank(row.kind);
            match best {
                Some((current, _)) if rank > current => {}
                Some((current, _)) if rank == current => tie = true,
                _ => {
                    best = Some((rank, row));
                    tie = false;
                }
            }
        }
        if tie { None } else { best.map(|(_, row)| row) }
    }

    /// Builds the mosaic forest.
    #[must_use]
    pub fn tree(&self) -> OutlineTree {
        let roots = self
            .children
            .get(&None)
            .into_iter()
            .flatten()
            .filter_map(|index| self.node(*index, 0))
            .collect::<Vec<_>>();
        OutlineTree {
            roots: roots.into(),
            complete: self.complete,
        }
    }

    fn node(&self, index: usize, depth: usize) -> Option<OutlineNode> {
        let row = &self.rows[index];
        let decl = DeclRef::from_row(row)?;
        let children = match (row.id, depth < 64) {
            (RowId::Symbol(key), true) => self
                .children
                .get(&Some(key))
                .into_iter()
                .flatten()
                .filter_map(|child| self.node(*child, depth + 1))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        Some(OutlineNode {
            decl,
            children: children.into(),
        })
    }
}

fn outline_order(left: &Row, right: &Row) -> std::cmp::Ordering {
    let site = |row: &Row| {
        row.source
            .captured()
            .map(|location| (location.path().to_owned(), location.start_line()))
    };
    // Captured rows first in file/line order; uncaptured rows after, by name.
    match (site(left), site(right)) {
        (Some(a), Some(b)) => a.cmp(&b).then_with(|| left.label.cmp(&right.label)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => leaf_name(&left.label)
            .cmp(leaf_name(&right.label))
            .then_with(|| left.label.cmp(&right.label)),
    }
}

const fn name_rank(kind: Option<DeclarationKind>) -> u8 {
    match kind {
        Some(
            DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Union
            | DeclarationKind::Class
            | DeclarationKind::Trait
            | DeclarationKind::Interface,
        ) => 0,
        Some(DeclarationKind::Type) => 1,
        _ => 2,
    }
}

const fn is_type_like(kind: Option<DeclarationKind>) -> bool {
    matches!(
        kind,
        Some(
            DeclarationKind::Struct
                | DeclarationKind::Enum
                | DeclarationKind::Union
                | DeclarationKind::Class
                | DeclarationKind::Trait
                | DeclarationKind::Interface
                | DeclarationKind::Type
        )
    )
}

const fn is_callable(kind: Option<DeclarationKind>) -> bool {
    matches!(
        kind,
        Some(
            DeclarationKind::Function
                | DeclarationKind::Method
                | DeclarationKind::Constructor
                | DeclarationKind::Macro
        )
    )
}

fn leaf_name(label: &str) -> &str {
    label.rsplit("::").next().unwrap_or(label)
}

/// Returns whether a signature is the compiler's canonical type encoding
/// (`nominal(entity(family=…))`, `builtin(i32)`, `reference(mutability=…)`)
/// rather than declaration source text. Such text is not a signature a reader
/// can use, so it is reported as [`GapReason::Encoded`].
#[must_use]
pub fn is_encoded_signature(text: &str) -> bool {
    const HEADS: [&str; 30] = [
        "annotated(", "applied(", "array(", "builtin(", "c-qualified(", "channel(",
        "conditional(", "cxx-member-pointer(", "cxx-reference(", "entity(", "external(",
        "fixed(", "function(parameters=", "import(", "indexed(", "infer(", "inferred(",
        "map(", "mapped(", "namespace(", "nominal(", "package(", "pointer(", "qualified(",
        "rectangular(", "reference(", "tuple(", "typeof(", "unknown(", "literal.",
    ];
    let text = text.trim_start();
    HEADS.iter().any(|head| text.starts_with(head))
}

// ---------------------------------------------------------------------------
// Signatures, docs, source sites
// ---------------------------------------------------------------------------

const fn token_class(kind: TokenKind) -> TokenClass {
    match kind {
        TokenKind::Keyword => TokenClass::Keyword,
        TokenKind::Name => TokenClass::Name,
        TokenKind::Type => TokenClass::Type,
        TokenKind::Binding => TokenClass::Binding,
        TokenKind::Lifetime => TokenClass::Lifetime,
        TokenKind::Punctuation => TokenClass::Punctuation,
        TokenKind::Literal => TokenClass::Literal,
        TokenKind::Text => TokenClass::Text,
    }
}

/// Tokenizes one signature and links its type identifiers by name inside
/// the package. The declaration's own key is never linked to itself.
#[must_use]
pub fn signature_text(
    text: Option<&str>,
    language: Language,
    own: Option<SymbolKey>,
    outline: Option<&OutlineIndex>,
) -> Known<SignatureText> {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return Known::unknown(
            GapReason::NotCaptured,
            "the producer captured no signature for this declaration",
        );
    };
    if is_encoded_signature(text) {
        return Known::unknown(
            GapReason::Encoded,
            format!("the engine returned an encoded type, not source text: {text}"),
        );
    }
    let signature = backend_present::Signature::tokenize(text, language);
    let mut tokens = Vec::with_capacity(signature.tokens().len());
    let mut offset = 0_usize;
    for token in signature.tokens() {
        let start = offset;
        offset += token.text().len();
        let Some(span) = u32::try_from(start)
            .ok()
            .zip(u32::try_from(offset).ok())
            .and_then(|(start, end)| ByteSpan::new(start, end))
        else {
            break;
        };
        let class = token_class(token.kind());
        let link = (class == TokenClass::Type)
            .then(|| {
                outline?
                    .resolve_name(token.text(), is_type_like)
                    .filter(|row| own.is_none_or(|own| row.id != RowId::Symbol(own)))
                    .and_then(|row| SymbolRef::new(&row.label).ok())
            })
            .flatten()
            .map(|target| SymbolLink {
                target,
                provenance: Provenance::ByName,
            });
        tokens.push(SignatureToken { span, class, link });
    }
    Known::Known(SignatureText {
        text: Arc::from(text),
        tokens: tokens.into(),
    })
}

/// Lowers producer fragments, resolving link keys to coordinates through
/// the package outline when it names them.
#[must_use]
pub fn doc_fragments(fragments: &[Fragment], outline: Option<&OutlineIndex>) -> Arc<[DocFragment]> {
    fragments
        .iter()
        .map(|fragment| match fragment {
            Fragment::Text(text) => DocFragment::Text(Arc::from(text.as_str())),
            Fragment::Code(code) => DocFragment::Code(Arc::from(code.as_str())),
            Fragment::Link { label, target } => DocFragment::Link {
                label: Arc::from(label.as_str()),
                target: *target,
                coordinate: outline
                    .and_then(|outline| outline.row(*target))
                    .and_then(|row| SymbolRef::new(&row.label).ok()),
            },
            Fragment::Break => DocFragment::Break,
        })
        .collect()
}

fn availability_gap(availability: &SourceAvailability) -> Gap {
    match availability {
        SourceAvailability::Captured(_) | SourceAvailability::NotCaptured => Gap::new(
            GapReason::NotCaptured,
            "the producer retained no source span for this declaration",
        ),
        SourceAvailability::NotHydrated => Gap::new(
            GapReason::NotHydrated,
            "a source span exists, but its bytes are not resident locally",
        ),
        SourceAvailability::Unconfigured => Gap::new(
            GapReason::Unconfigured,
            "this deployment has no source provider for the declaration's origin",
        ),
    }
}

fn excerpt_gap(excerpt: &SourceExcerpt) -> Gap {
    match excerpt {
        SourceExcerpt::Captured { .. } | SourceExcerpt::NotCaptured => Gap::new(
            GapReason::NotCaptured,
            "the producer did not capture declaration source text",
        ),
        SourceExcerpt::NotHydrated => Gap::new(
            GapReason::NotHydrated,
            "declaration source text exists but is not resident in this process",
        ),
        SourceExcerpt::Unconfigured => Gap::new(
            GapReason::Unconfigured,
            "this deployment has no source-text provider",
        ),
    }
}

/// Lowers a document's location and excerpt into one source site.
#[must_use]
pub fn source_site(location: &SourceAvailability, excerpt: &SourceExcerpt) -> SourceSite {
    let captured = location.captured();
    let location_known = captured.map_or_else(
        || Known::Unknown(availability_gap(location)),
        |location| {
            Known::Known(SourceLocation {
                path: Arc::from(location.path()),
                line: location.start_line(),
            })
        },
    );
    let excerpt_known = match excerpt {
        SourceExcerpt::Captured { text, extent } => {
            let lines = captured.map(|location| {
                let count = u32::try_from(text.lines().count().max(1)).unwrap_or(u32::MAX);
                LineSpan {
                    first: location.start_line(),
                    last: location.start_line().saturating_add(count - 1),
                }
            });
            Known::Known(Excerpt {
                text: Arc::clone(text),
                lines,
                complete: matches!(extent, SourceExcerptExtent::Complete),
            })
        }
        other => Known::Unknown(excerpt_gap(other)),
    };
    SourceSite {
        location: location_known,
        excerpt: excerpt_known,
    }
}

// ---------------------------------------------------------------------------
// Members, rose, references, outline position
// ---------------------------------------------------------------------------

/// Reads a Rust receiver from a method signature.
#[must_use]
pub fn receiver(signature: Option<&str>, language: Language) -> Receiver {
    let Some(signature) = signature else {
        return Receiver::Unknown;
    };
    if language != Language::Rust || is_encoded_signature(signature) {
        return Receiver::Unknown;
    }
    let Some(open) = signature.find('(') else {
        return Receiver::Unknown;
    };
    let params = &signature[open + 1..];
    let first = params
        .split([',', ')'])
        .next()
        .unwrap_or_default()
        .trim();
    let compact: String = first.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.starts_with("&mutself") || compact.starts_with("self:&mut") {
        return Receiver::Changes;
    }
    if compact.starts_with("&self")
        || compact.starts_with("self:&")
        || (compact.starts_with("&'") && compact.ends_with("self") && !compact.contains("mut"))
    {
        return Receiver::Reads;
    }
    if compact.starts_with("&'") && compact.ends_with("mutself") {
        return Receiver::Changes;
    }
    if compact == "self" || compact == "mutself" || compact.starts_with("self:") || compact.starts_with("mutself:") {
        return Receiver::Consumes;
    }
    Receiver::Makes
}

fn summary_of(row: &Row) -> Option<Arc<str>> {
    let text = row.document.iter().find_map(|fragment| match fragment {
        Fragment::Text(text) => Some(text.as_str()),
        _ => None,
    })?;
    let first = text.lines().next().unwrap_or_default().trim();
    (!first.is_empty() && first != row.label).then(|| Arc::from(first))
}

fn member_of(row: &Row, outline: Option<&OutlineIndex>) -> Option<Member> {
    let decl = DeclRef::from_row(row)?;
    let own = decl.key;
    let signature = signature_text(row.signature.as_deref(), decl.language, own, outline);
    Some(Member {
        decl,
        signature,
        summary: summary_of(row),
    })
}

/// Groups one declaration's children into the members ledger.
#[must_use]
pub fn members(
    centre_kind: Option<DeclarationKind>,
    children: &[&Row],
    outline: Option<&OutlineIndex>,
) -> Members {
    let mut made_of = Vec::new();
    let mut does: BTreeMap<Receiver, Vec<Member>> = BTreeMap::new();
    let mut other = Vec::new();
    // A callable's children are its local bindings, not members.
    if is_callable(centre_kind) {
        return Members {
            made_of: Arc::from([]),
            does: Arc::from([]),
            other: Arc::from([]),
        };
    }
    for row in children {
        let Some(member) = member_of(row, outline) else {
            continue;
        };
        match row.kind {
            Some(DeclarationKind::Field | DeclarationKind::Variant | DeclarationKind::Property) => {
                made_of.push(member);
            }
            // The semantic lane publishes enum cases as constants.
            Some(DeclarationKind::Constant) if centre_kind == Some(DeclarationKind::Enum) => {
                made_of.push(member);
            }
            Some(DeclarationKind::Method | DeclarationKind::Function | DeclarationKind::Constructor) => {
                let receiver = receiver(row.signature.as_deref(), member.decl.language);
                does.entry(receiver).or_default().push(member);
            }
            _ => other.push(member),
        }
    }
    let does = does
        .into_iter()
        .map(|(receiver, mut members)| {
            members.sort_by(|a, b| a.decl.name.cmp(&b.decl.name));
            MethodGroup {
                receiver,
                members: members.into(),
            }
        })
        .collect::<Vec<_>>();
    Members {
        made_of: made_of.into(),
        does: does.into(),
        other: other.into(),
    }
}

/// The related reply as the mapping needs it.
#[derive(Clone, Copy, Debug)]
pub struct Neighbourhood<'a> {
    /// Rows the reply carried, including the centre.
    pub rows: &'a [Row],
    /// Typed edges, when the compiler graph authority answered.
    pub relations: Option<&'a [GraphRelation]>,
    /// Rich graph edges carrying per-edge confidence, when present.
    pub rich: Option<&'a backend_library::RichGraphSnapshot>,
}

impl<'a> Neighbourhood<'a> {
    /// Borrows one graph snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &'a ViewSnapshot) -> Self {
        Self {
            rows: snapshot.root.rows(),
            relations: snapshot.graph_relations.as_deref(),
            rich: snapshot.rich_graph.as_ref(),
        }
    }

    fn row(&self, id: RowId) -> Option<&'a Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    fn confidence(&self, edge: &GraphRelation) -> SemanticConfidence {
        let from = GraphNodeId::for_row(edge.from);
        let to = GraphNodeId::for_row(edge.to);
        self.rich
            .and_then(|rich| {
                rich.edges.iter().find(|candidate| {
                    candidate.from == from
                        && candidate.to == to
                        && candidate.kind == GraphEdgeKind::Code { relation: edge.relation }
                })
            })
            .and_then(|candidate| candidate.provenance.confidence)
            // The sidecar is emitted only by the compiler graph authority.
            .unwrap_or(SemanticConfidence::Compiler)
    }
}

const fn is_up(kind: SemanticLinkKind) -> bool {
    matches!(
        kind,
        SemanticLinkKind::Implements | SemanticLinkKind::Inherits | SemanticLinkKind::Overrides
    )
}

fn relation(decl: DeclRef, kind: RelationKind, provenance: Provenance) -> Relation {
    Relation {
        decl,
        kind,
        provenance,
        arrival: Arrival::NotReported,
        via: None,
    }
}

/// Builds the rose for `centre` from its neighbourhood.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
pub fn rose(
    centre: RowId,
    centre_kind: Option<DeclarationKind>,
    neighbourhood: Option<Neighbourhood<'_>>,
    failure: Option<Gap>,
) -> Rose {
    let Some(neighbourhood) = neighbourhood else {
        let gap = failure.unwrap_or_else(|| {
            Gap::new(GapReason::ReadFailed, "the related neighbourhood was not read")
        });
        return Rose {
            up: Known::Unknown(gap.clone()),
            down: Known::Unknown(gap.clone()),
            left: Known::Unknown(gap.clone()),
            right: Known::Unknown(gap.clone()),
            implemented_by: Known::Unknown(gap),
        };
    };
    let down = if is_callable(centre_kind) {
        Vec::new()
    } else {
        neighbourhood
            .rows
            .iter()
            .filter(|row| row.id != centre && row.parent.map(RowId::Symbol) == Some(centre))
            .filter_map(DeclRef::from_row)
            .map(|decl| relation(decl, RelationKind::Contains, Provenance::Structural))
            .collect::<Vec<_>>()
    };
    let Some(edges) = neighbourhood.relations else {
        let gap = Gap::new(
            GapReason::NoSemanticPublication,
            "the engine answered with containment only; typed relations need a compiler publication for this package",
        );
        return Rose {
            up: Known::Unknown(gap.clone()),
            down: Known::Known(down.into()),
            left: Known::Unknown(gap.clone()),
            right: Known::Unknown(gap.clone()),
            implemented_by: Known::Unknown(gap),
        };
    };
    let mut up = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut implemented_by = Vec::new();
    for edge in edges {
        let provenance = Provenance::Semantic(neighbourhood.confidence(edge));
        if edge.from == centre && edge.to != centre {
            let Some(decl) = neighbourhood.row(edge.to).and_then(DeclRef::from_row) else {
                continue;
            };
            let item = relation(decl, RelationKind::Semantic(edge.relation), provenance);
            if is_up(edge.relation) {
                up.push(item);
            } else {
                right.push(item);
            }
        } else if edge.to == centre && edge.from != centre {
            let Some(decl) = neighbourhood.row(edge.from).and_then(DeclRef::from_row) else {
                continue;
            };
            let item = relation(decl, RelationKind::Semantic(edge.relation), provenance);
            if is_up(edge.relation) {
                implemented_by.push(item);
            } else {
                left.push(item);
            }
        }
    }
    derive_impl_blocks(centre, centre_kind, &neighbourhood, edges, &mut up, &mut implemented_by);
    Rose {
        up: Known::Known(up.into()),
        down: Known::Known(down.into()),
        left: Known::Known(left.into()),
        right: Known::Known(right.into()),
        implemented_by: Known::Known(implemented_by.into()),
    }
}

/// An impl block the semantic lane publishes as a `Type` row whose signature
/// is the encoded nominal self type.
fn is_impl_block(row: &Row) -> bool {
    row.kind == Some(DeclarationKind::Type)
        && row
            .signature
            .as_deref()
            .is_some_and(|signature| signature.trim_start().starts_with("nominal("))
}

/// Derives "self type implements trait" through impl-block rows, because the
/// engine publishes no `Implements` edge for `impl Trait for Type`: it emits
/// a `Type` row named after the self type with type references to both.
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
fn derive_impl_blocks(
    centre: RowId,
    centre_kind: Option<DeclarationKind>,
    neighbourhood: &Neighbourhood<'_>,
    edges: &[GraphRelation],
    up: &mut Vec<Relation>,
    implemented_by: &mut Vec<Relation>,
) {
    let contract = matches!(
        centre_kind,
        Some(DeclarationKind::Trait | DeclarationKind::Interface)
    );
    let references = |from: RowId| {
        edges.iter().filter(move |edge| {
            edge.from == from && edge.relation == SemanticLinkKind::TypeReference
        })
    };
    for block in neighbourhood.rows.iter().filter(|row| row.id != centre && is_impl_block(row)) {
        let Some(via) = DeclRef::from_row(block) else {
            continue;
        };
        if contract {
            // Trait page: an impl block that references this trait implements it.
            if let Some(edge) = references(block.id).find(|edge| edge.to == centre) {
                if implemented_by
                    .iter()
                    .any(|existing| existing.decl.coordinate == via.coordinate)
                {
                    continue;
                }
                implemented_by.push(Relation {
                    decl: via.clone(),
                    kind: RelationKind::Semantic(SemanticLinkKind::Implements),
                    provenance: Provenance::Derived {
                        via: Derivation::ImplBlock,
                        confidence: neighbourhood.confidence(edge),
                    },
                    arrival: Arrival::NotReported,
                    via: Some(via),
                });
            }
            continue;
        }
        // Type page: an impl block named after this type that references it
        // and a contract means this type implements that contract.
        let Some(centre_row) = neighbourhood.row(centre) else {
            continue;
        };
        if leaf_name(&block.label) != leaf_name(&centre_row.label) {
            continue;
        }
        let Some(self_edge) = references(block.id).find(|edge| edge.to == centre) else {
            continue;
        };
        for edge in references(block.id).filter(|edge| edge.to != centre) {
            let Some(target) = neighbourhood.row(edge.to) else {
                continue;
            };
            if !matches!(
                target.kind,
                Some(DeclarationKind::Trait | DeclarationKind::Interface)
            ) {
                continue;
            }
            let Some(decl) = DeclRef::from_row(target) else {
                continue;
            };
            let confidence = neighbourhood
                .confidence(edge)
                .min(neighbourhood.confidence(self_edge));
            up.push(Relation {
                decl,
                kind: RelationKind::Semantic(SemanticLinkKind::Implements),
                provenance: Provenance::Derived {
                    via: Derivation::ImplBlock,
                    confidence,
                },
                arrival: Arrival::NotReported,
                via: Some(via.clone()),
            });
        }
    }
}

/// Lowers a references reply (or its failure) into use sites.
#[must_use]
pub fn references(
    reply: Result<&SurfaceReply, &ClientError>,
    outline: Option<&OutlineIndex>,
) -> Known<Arc<[ReferenceSite]>> {
    let records = match reply {
        Ok(SurfaceReply::References { references, .. }) => references,
        Ok(other) => {
            return Known::unknown(
                GapReason::ReadFailed,
                format!("references reply changed shape to {:?}", other.id()),
            );
        }
        Err(error) => return Known::Unknown(client_gap(error)),
    };
    let sites = records
        .iter()
        .filter_map(|record| {
            let label = record.site.as_str();
            let row = outline.and_then(|outline| outline.row_by_label(label));
            let site = row.map_or_else(
                || DeclRef::from_label(label, None, None, None),
                DeclRef::from_row,
            )?;
            let span = record.evidence.source.as_ref().map_or_else(
                || {
                    Known::unknown(
                        GapReason::NotCaptured,
                        "the authority captured no source span for this use",
                    )
                },
                |span| {
                    ByteSpan::new(span.start, span.end).map_or_else(
                        || Known::unknown(GapReason::ReadFailed, "inverted reference span"),
                        |bytes| {
                            Known::Known(FileSpan {
                                file: Arc::from(span.file.as_str()),
                                bytes,
                            })
                        },
                    )
                },
            );
            Some(ReferenceSite {
                site,
                relation: record.relation,
                confidence: record.evidence.confidence,
                span,
                scope: match record.target {
                    SemanticLinkTarget::Local { .. } => ReferenceScope::Local,
                    SemanticLinkTarget::Stable { .. } => ReferenceScope::Stable,
                    SemanticLinkTarget::Foreign { .. } => ReferenceScope::Foreign,
                    SemanticLinkTarget::FragmentEntity { .. } => ReferenceScope::FragmentEntity,
                },
            })
        })
        .collect::<Vec<_>>();
    Known::Known(sites.into())
}

/// Lowers one client failure into a typed gap.
#[must_use]
pub fn client_gap(error: &ClientError) -> Gap {
    let text = error.to_string();
    let reason = if text.contains("semantic publication") {
        GapReason::NoSemanticPublication
    } else {
        GapReason::ReadFailed
    };
    Gap::new(reason, text)
}

/// Places `centre` inside its package outline.
#[must_use]
pub fn outline_position(centre: RowId, outline: Option<&OutlineIndex>, failure: Option<Gap>) -> Known<OutlinePosition> {
    let Some(outline) = outline else {
        return Known::Unknown(failure.unwrap_or_else(|| {
            Gap::new(GapReason::ReadFailed, "the package outline was not read")
        }));
    };
    let RowId::Symbol(key) = centre else {
        return Known::unknown(GapReason::NotCaptured, "the declaration has no symbol row");
    };
    let Some(row) = outline.row(key) else {
        return Known::unknown(
            if outline.is_complete() {
                GapReason::NotCaptured
            } else {
                GapReason::Unavailable
            },
            "the declaration is not in the package outline the engine served",
        );
    };
    let ancestors = outline
        .ancestors(key)
        .into_iter()
        .filter_map(DeclRef::from_row)
        .collect::<Vec<_>>();
    let parent = row.parent.filter(|parent| outline.row(*parent).is_some());
    let sibling_rows: Vec<&Row> = match parent {
        Some(parent) => outline.children(parent).collect(),
        None => outline
            .children
            .get(&None)
            .into_iter()
            .flatten()
            .map(|index| &outline.rows[*index])
            .collect(),
    };
    let index = sibling_rows.iter().position(|sibling| sibling.id == centre);
    let siblings = sibling_rows
        .into_iter()
        .filter_map(DeclRef::from_row)
        .collect::<Vec<_>>();
    Known::Known(OutlinePosition {
        ancestors: ancestors.into(),
        siblings: siblings.into(),
        index,
    })
}

// ---------------------------------------------------------------------------
// Symbol page
// ---------------------------------------------------------------------------

/// Everything one symbol page is assembled from.
#[derive(Debug)]
pub struct SymbolInputs<'a> {
    /// Requested coordinate.
    pub coordinate: &'a SymbolRef,
    /// Document reply.
    pub document: &'a Document,
    /// Related neighbourhood, or why it is missing.
    pub related: Result<Neighbourhood<'a>, Gap>,
    /// References reply.
    pub references: Result<&'a SurfaceReply, &'a ClientError>,
    /// Package outline, or why it is missing.
    pub outline: Result<&'a OutlineIndex, Gap>,
}

/// Assembles one declaration page.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
pub fn symbol_page(inputs: &SymbolInputs<'_>) -> SymbolPage {
    let coordinate = inputs.coordinate;
    let outline = inputs.outline.as_ref().ok().copied();
    let neighbourhood = inputs.related.as_ref().ok().copied();
    // The page's own row: a structural row echoes the document key; a
    // compiler-backed row is keyed by its compiler identity and found by label.
    let own_row = neighbourhood
        .and_then(|hood| hood.rows.iter().find(|row| row.label == coordinate.as_str()))
        .or_else(|| outline.and_then(|outline| outline.row_by_label(coordinate.as_str())))
        .or_else(|| outline.and_then(|outline| outline.row(inputs.document.symbol)));
    let centre = own_row.map_or(RowId::Symbol(inputs.document.symbol), |row| row.id);
    let captured = inputs
        .document
        .location
        .captured()
        .map(|location| (location.path(), location.start_line()));
    let kind = own_row.and_then(|row| row.kind);
    let identity = DeclRef::from_label(
        coordinate.as_str(),
        match centre {
            RowId::Symbol(key) => Some(key),
            RowId::Package(_) | RowId::Object(_) => None,
        },
        kind,
        captured,
    )
    .unwrap_or_else(|| DeclRef {
        coordinate: coordinate.clone(),
        key: None,
        name: Arc::from(leaf_name(coordinate.as_str())),
        kind,
        family: crate::model::pages::KindFamily::of(kind),
        path: None,
        line: None,
        language: Language::Unknown,
        semantic: false,
    });
    let signature = signature_text(
        inputs.document.signature.as_deref(),
        identity.language,
        identity.key,
        outline,
    );
    let children: Vec<&Row> = match (outline, centre) {
        (Some(outline), RowId::Symbol(key)) if outline.row(key).is_some() => {
            outline.children(key).collect()
        }
        _ => neighbourhood
            .map(|hood| {
                hood.rows
                    .iter()
                    .filter(|row| row.id != centre && row.parent.map(RowId::Symbol) == Some(centre))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let members_known = if outline.is_some() || neighbourhood.is_some() {
        Known::Known(members(kind, &children, outline))
    } else {
        Known::Unknown(
            inputs
                .outline
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| Gap::new(GapReason::ReadFailed, "members were not read")),
        )
    };
    SymbolPage {
        package: coordinate.package().map_or_else(
            || {
                Known::unknown(
                    GapReason::NotCaptured,
                    "the coordinate does not spell its package",
                )
            },
            Known::Known,
        ),
        signature,
        docs: doc_fragments(&inputs.document.fragments, outline),
        site: source_site(&inputs.document.location, &inputs.document.excerpt),
        members: members_known,
        rose: rose(
            centre,
            kind,
            neighbourhood,
            inputs.related.as_ref().err().cloned(),
        ),
        references: references(inputs.references, outline),
        outline: outline_position(centre, outline, inputs.outline.as_ref().err().cloned()),
        identity,
    }
}

// ---------------------------------------------------------------------------
// Source view
// ---------------------------------------------------------------------------

/// Finds the byte offset of one one-based line.
fn line_offset(text: &str, line: u32) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }
    let mut seen = 1_u32;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            seen += 1;
            if seen == line {
                return Some(index + 1);
            }
        }
    }
    None
}

/// Checks a local file against the engine's excerpt at the declared line.
/// Returns the file text only when the excerpt is found starting on that line.
#[must_use]
pub fn verify_local_file(file: &str, excerpt: &str, line: u32) -> bool {
    let Some(start) = line_offset(file, line) else {
        return false;
    };
    let rest = &file[start..];
    let excerpt = excerpt.trim_end();
    if excerpt.is_empty() {
        return false;
    }
    // The excerpt may begin after indentation on its first line.
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let first_line = &rest[..line_end];
    first_line
        .find(excerpt.lines().next().unwrap_or_default().trim_start())
        .is_some_and(|column| rest[column..].starts_with(excerpt))
}

fn identifier_spans(
    text: &str,
    own: Option<SymbolKey>,
    outline: Option<&OutlineIndex>,
) -> Known<Arc<[IdentifierSpan]>> {
    let Some(outline) = outline else {
        return Known::unknown(GapReason::ReadFailed, "the package outline was not read");
    };
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') {
                index += 1;
            }
            let word = &text[start..index];
            if word.len() > 1
                && let Some(row) = outline.resolve_name(word, |kind| {
                    is_type_like(kind) && kind != Some(DeclarationKind::Type)
                })
                && own.is_none_or(|own| row.id != RowId::Symbol(own))
                && let (Some(span), Ok(target)) = (
                    u32::try_from(start)
                        .ok()
                        .zip(u32::try_from(index).ok())
                        .and_then(|(start, end)| ByteSpan::new(start, end)),
                    SymbolRef::new(&row.label),
                )
            {
                spans.push(IdentifierSpan {
                    span,
                    link: SymbolLink {
                        target,
                        provenance: Provenance::ByName,
                    },
                });
            }
        } else {
            index += 1;
        }
    }
    Known::Known(spans.into())
}

/// Assembles one source view. `local_file` is the whole file read from the
/// local project, when the package is local and the file exists.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
pub fn source_view(
    coordinate: &SymbolRef,
    document: &Document,
    local_file: Option<&str>,
    references: &Known<Arc<[ReferenceSite]>>,
    outline: Option<&OutlineIndex>,
) -> SourceView {
    let site = source_site(&document.location, &document.excerpt);
    let own_row = outline.and_then(|outline| outline.row_by_label(coordinate.as_str()));
    let captured = document
        .location
        .captured()
        .map(|location| (location.path(), location.start_line()));
    let symbol = own_row
        .and_then(DeclRef::from_row)
        .or_else(|| DeclRef::from_label(coordinate.as_str(), Some(document.symbol), None, captured))
        .unwrap_or_else(|| DeclRef {
            coordinate: coordinate.clone(),
            key: Some(document.symbol),
            name: Arc::from(leaf_name(coordinate.as_str())),
            kind: None,
            family: crate::model::pages::KindFamily::Namespace,
            path: None,
            line: None,
            language: Language::Unknown,
            semantic: false,
        });
    let file = site
        .location
        .clone()
        .map(|location| location.path);
    let excerpt = site.excerpt.known().cloned();
    let location = site.location.known().cloned();
    let verified = match (local_file, &excerpt, &location) {
        (Some(file_text), Some(excerpt), Some(location)) => {
            verify_local_file(file_text, &excerpt.text, location.line).then_some(file_text)
        }
        _ => None,
    };
    let text = match (verified, &excerpt, &location) {
        (Some(file_text), Some(excerpt), _) => Known::Known(SourceText {
            text: Arc::from(file_text),
            first_line: 1,
            origin: SourceOrigin::LocalFile,
            complete: excerpt.complete,
        }),
        (None, Some(excerpt), location) => Known::Known(SourceText {
            text: Arc::clone(&excerpt.text),
            first_line: location.as_ref().map_or(1, |location| location.line),
            origin: SourceOrigin::Excerpt,
            complete: excerpt.complete,
        }),
        (_, None, _) => Known::Unknown(
            site.excerpt
                .gap()
                .cloned()
                .unwrap_or_else(|| Gap::new(GapReason::NotCaptured, "no source text")),
        ),
    };
    let declaration = excerpt
        .as_ref()
        .and_then(|excerpt| excerpt.lines)
        .map_or_else(
            || Known::unknown(GapReason::NotCaptured, "the declaration's lines are not known"),
            Known::Known,
        );
    let identifiers = match &text {
        Known::Known(source) => identifier_spans(&source.text, symbol.key, outline),
        Known::Unknown(gap) => Known::Unknown(gap.clone()),
    };
    let (uses, uses_elsewhere) = match (references, &text, &file) {
        (Known::Known(sites), Known::Known(source), Known::Known(path)) => {
            let mut here = Vec::new();
            let mut elsewhere = Vec::new();
            for span in sites.iter().filter_map(|site| site.span.known()) {
                let in_text = source.origin == SourceOrigin::LocalFile
                    && span.file.as_ref() == path.as_ref()
                    && (span.bytes.end as usize) <= source.text.len();
                if in_text {
                    here.push(span.bytes);
                } else {
                    elsewhere.push(span.clone());
                }
            }
            let uses = if source.origin == SourceOrigin::LocalFile {
                Known::Known(here.into())
            } else {
                Known::unknown(
                    GapReason::NotServed,
                    "use spans are file byte offsets and the engine serves only the declaration excerpt",
                )
            };
            (uses, elsewhere)
        }
        (Known::Unknown(gap), _, _) => (Known::Unknown(gap.clone()), Vec::new()),
        _ => (
            Known::unknown(GapReason::NotCaptured, "no source text to place uses in"),
            Vec::new(),
        ),
    };
    SourceView {
        symbol,
        file,
        text,
        declaration,
        identifiers,
        uses,
        uses_elsewhere: uses_elsewhere.into(),
    }
}

// ---------------------------------------------------------------------------
// Package dossier
// ---------------------------------------------------------------------------

const fn standing(value: RegistryReleaseStanding) -> Standing {
    match value {
        RegistryReleaseStanding::Available => Standing::Available,
        RegistryReleaseStanding::Yanked => Standing::Yanked,
        RegistryReleaseStanding::Deprecated => Standing::Deprecated,
        RegistryReleaseStanding::Unlisted => Standing::Unlisted,
        RegistryReleaseStanding::Retracted => Standing::Retracted,
        RegistryReleaseStanding::Removed => Standing::Removed,
    }
}

const fn fact_gap_reason(availability: RegistryFactAvailability) -> GapReason {
    match availability {
        RegistryFactAvailability::NotRecorded => GapReason::NotRecorded,
        RegistryFactAvailability::Unsupported => GapReason::Unsupported,
        RegistryFactAvailability::Unavailable => GapReason::Unavailable,
        RegistryFactAvailability::Stale => GapReason::Stale,
        RegistryFactAvailability::Unknown => GapReason::Unknown,
    }
}

fn not_served(what: &str) -> Gap {
    Gap::new(
        GapReason::NotServed,
        format!("the engine serves no {what} for registry releases"),
    )
}

fn local_gap(what: &str) -> Gap {
    Gap::new(
        GapReason::LocalProject,
        format!("{what} is a registry fact; no registry records a local project"),
    )
}

/// Lowers one registry record into the dossier's record.
#[must_use]
pub fn registry_record(record: &RegistryPackageRecord) -> PackageRecord {
    let advisory = &record.advisory;
    let worst = advisory
        .advisories
        .iter()
        .map(|entry| entry.severity)
        .max();
    let decision = match &advisory.decision {
        backend_library::AcquisitionDecision::Allow => "allow",
        backend_library::AcquisitionDecision::Warn(_) => "warn",
        backend_library::AcquisitionDecision::Deny(_) => "deny",
    };
    PackageRecord {
        package: PackageRef::from_reference(record.coordinate.clone()),
        source: RecordSource::Registry,
        name: Arc::from(record.name.as_str()),
        version: Known::Known(Arc::from(record.version.as_str())),
        ecosystem: Known::Known(Arc::from(record.ecosystem.as_str())),
        standing: Known::Known(standing(record.standing)),
        downloads: match &record.downloads {
            RegistryDownloadCount::Exact(count) => Known::Known(Downloads::Exact(*count)),
            RegistryDownloadCount::Approximate(count) => {
                Known::Known(Downloads::Approximate(*count))
            }
            RegistryDownloadCount::Unavailable(availability) => Known::unknown(
                fact_gap_reason(*availability),
                "the registry feed published no usable download count",
            ),
        },
        bytes: Known::Known(record.bytes),
        advisory: Known::Known(AdvisorySummary {
            advisories: advisory.advisories.len(),
            worst,
            decision: Arc::from(decision),
            coverage: advisory.coverage,
            freshness: advisory.freshness,
        }),
        description: Known::Unknown(not_served("description")),
        license: Known::Unknown(not_served("license")),
    }
}

fn local_record(package: &PackageRef, local: &LocalPackage) -> PackageRecord {
    PackageRecord {
        package: package.clone(),
        source: RecordSource::LocalManifest,
        name: Arc::clone(&local.name),
        version: local.version.clone().map_or_else(
            || Known::unknown(GapReason::NotRecorded, "the manifest states no version"),
            Known::Known,
        ),
        ecosystem: Known::Known(Arc::from("cargo")),
        standing: Known::Unknown(local_gap("release standing")),
        downloads: Known::Unknown(local_gap("download count")),
        bytes: Known::Unknown(local_gap("archive size")),
        advisory: Known::Unknown(local_gap("advisory coverage")),
        description: local.description.clone().map_or_else(
            || Known::unknown(GapReason::NotRecorded, "the manifest states no description"),
            Known::Known,
        ),
        license: local.license.clone().map_or_else(
            || Known::unknown(GapReason::NotRecorded, "the manifest states no license"),
            Known::Known,
        ),
    }
}

/// Everything a dossier is assembled from. Each part carries its own failure.
#[derive(Debug)]
pub struct PackageInputs<'a> {
    /// The package asked for.
    pub package: &'a PackageRef,
    /// `package` surface reply.
    pub records: Result<&'a SurfaceReply, &'a ClientError>,
    /// `package-versions` surface reply.
    pub versions: Result<&'a SurfaceReply, &'a ClientError>,
    /// `dependencies` surface reply.
    pub dependencies: Result<&'a SurfaceReply, &'a ClientError>,
    /// `dependents` surface reply.
    pub dependents: Result<&'a SurfaceReply, &'a ClientError>,
    /// Package outline, or why it is missing.
    pub outline: Result<&'a OutlineIndex, Gap>,
    /// Local manifest facts, for a local project.
    pub local: Option<&'a LocalPackage>,
}

fn surface_gap(reply: Result<&SurfaceReply, &ClientError>, what: &str) -> Gap {
    match reply {
        Ok(other) => Gap::new(
            GapReason::ReadFailed,
            format!("{what} reply changed shape to {:?}", other.id()),
        ),
        Err(error) => client_gap(error),
    }
}

const fn scope(scope: backend_library::DependencyScope) -> DependencyScope {
    match scope {
        backend_library::DependencyScope::Runtime => DependencyScope::Runtime,
        backend_library::DependencyScope::Optional => DependencyScope::Optional,
        backend_library::DependencyScope::Development => DependencyScope::Development,
        backend_library::DependencyScope::Build => DependencyScope::Build,
        backend_library::DependencyScope::Peer => DependencyScope::Peer,
    }
}

/// Assembles one package dossier.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
pub fn package_dossier(inputs: &PackageInputs<'_>) -> PackageDossier {
    let package = inputs.package;
    let local = package.is_local();
    let registry_records = match inputs.records {
        Ok(SurfaceReply::Package(records)) => Ok(records.as_ref()),
        other => Err(surface_gap(other, "package")),
    };
    let record = match (inputs.local, &registry_records) {
        (Some(manifest), _) => Known::Known(local_record(package, manifest)),
        (None, Ok(records)) => records
            .iter()
            .find(|record| record.coordinate == *package.reference())
            .or_else(|| records.first())
            .map_or_else(
                || {
                    Known::Unknown(if local {
                        local_gap("a registry record")
                    } else {
                        Gap::new(
                            GapReason::NotRecorded,
                            "the local registry has no committed record for this release",
                        )
                    })
                },
                |record| Known::Known(registry_record(record)),
            ),
        (None, Err(gap)) => Known::Unknown(gap.clone()),
    };
    let current_version = record
        .known()
        .and_then(|record| record.version.known().cloned());
    let versions = match inputs.versions {
        Ok(SurfaceReply::PackageVersions(records)) if records.is_empty() && local => {
            Known::Unknown(local_gap("release history"))
        }
        Ok(SurfaceReply::PackageVersions(records)) => Known::Known(
            records
                .iter()
                .map(|record| VersionEntry {
                    package: PackageRef::from_reference(record.coordinate.clone()),
                    version: Arc::from(record.version.as_str()),
                    standing: standing(record.standing),
                    current: record.coordinate == *package.reference()
                        || current_version
                            .as_deref()
                            .is_some_and(|version| version == record.version.as_str()),
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        other => Known::Unknown(surface_gap(other, "package-versions")),
    };
    let dependencies = match (inputs.local, inputs.dependencies) {
        (_, Ok(SurfaceReply::Dependencies(backend_library::DependencyFacts::Known(rows)))) => {
            Known::Known(
                rows.iter()
                    .map(|row| Dependency {
                        name: Arc::from(row.target.name.as_str()),
                        requirement: Arc::from(row.target.requirement.as_str()),
                        scope: scope(row.scope),
                        optional: row.optional,
                        resolved: row
                            .target
                            .resolved
                            .clone()
                            .map(PackageRef::from_reference),
                    })
                    .collect::<Vec<_>>()
                    .into(),
            )
        }
        // A local project's own manifest names its dependencies even when
        // the registry graph has no record of the project.
        (Some(manifest), _) => Known::Known(
            manifest
                .dependencies
                .iter()
                .map(|dependency| Dependency {
                    name: Arc::clone(&dependency.name),
                    requirement: Arc::clone(&dependency.requirement),
                    scope: match dependency.kind {
                        DependencyKind::Normal => DependencyScope::Runtime,
                        DependencyKind::Development => DependencyScope::Development,
                        DependencyKind::Build => DependencyScope::Build,
                    },
                    optional: false,
                    resolved: None,
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        (None, Ok(SurfaceReply::Dependencies(backend_library::DependencyFacts::Unknown(text)))) => {
            Known::unknown(GapReason::NotRecorded, text.as_str())
        }
        (
            None,
            Ok(SurfaceReply::Dependencies(backend_library::DependencyFacts::Unavailable(text))),
        ) => Known::unknown(GapReason::Unavailable, text.as_str()),
        (None, other) => Known::Unknown(surface_gap(other, "dependencies")),
    };
    let dependents = match inputs.dependents {
        Ok(SurfaceReply::Dependents(backend_library::RegistryMetadata::Recorded(records))) => {
            Known::Known(records.iter().map(registry_record).collect::<Vec<_>>().into())
        }
        Ok(SurfaceReply::Dependents(backend_library::RegistryMetadata::NotRecorded(text))) => {
            if local {
                Known::Unknown(local_gap("reverse dependencies"))
            } else {
                Known::unknown(GapReason::NotRecorded, text.as_str())
            }
        }
        other => Known::Unknown(surface_gap(other, "dependents")),
    };
    let outline = match &inputs.outline {
        Ok(index) => Known::Known(index.tree()),
        Err(gap) => Known::Unknown(gap.clone()),
    };
    let readme = match inputs.local {
        Some(manifest) if !manifest.readme.is_empty() => Known::Known(Arc::clone(&manifest.readme)),
        Some(_) => Known::unknown(GapReason::NotRecorded, "the project has no README the manifest names"),
        None => Known::Unknown(not_served("README")),
    };
    PackageDossier {
        package: package.clone(),
        record,
        versions,
        dependencies,
        dependents,
        outline,
        readme,
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn match_reason(query: &str, decl: &DeclRef, row: &Row) -> MatchReason {
    let needle = query.to_lowercase();
    let name = decl.name.to_lowercase();
    if name == needle {
        return MatchReason::ExactName;
    }
    if name.contains(&needle) {
        return MatchReason::Name;
    }
    if row
        .signature
        .as_deref()
        .is_some_and(|signature| signature.to_lowercase().contains(&needle))
    {
        return MatchReason::Signature;
    }
    if summary_of(row).is_some_and(|summary| summary.to_lowercase().contains(&needle)) {
        return MatchReason::Docs;
    }
    MatchReason::Producer
}

/// Lowers one search snapshot into a results page.
#[must_use]
pub fn search_page(query: &str, snapshot: &ViewSnapshot, worker: usize) -> SearchPage {
    search_rows(
        query,
        snapshot.root.rows(),
        snapshot.root.coverage(),
        snapshot.next.map(backend_library::PageContinuation::from_cursor),
        worker,
    )
}

/// Lowers search rows, lane coverage, and a continuation into a results page.
#[must_use]
pub fn search_rows(
    query: &str,
    rows: &[Row],
    coverage: &[backend_library::Coverage],
    next: Option<backend_library::PageContinuation>,
    worker: usize,
) -> SearchPage {
    let rows = rows
        .iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let decl = DeclRef::from_row(row)?;
            let reason = match_reason(query, &decl, row);
            let signature = signature_text(row.signature.as_deref(), decl.language, decl.key, None);
            Some(SearchRow {
                rank,
                package: decl.coordinate.package().map(|package| Arc::from(package.as_str())),
                score: row.score.map_or_else(
                    || Known::unknown(GapReason::NotServed, "the engine published no score for this row"),
                    Known::Known,
                ),
                signature,
                snippet: summary_of(row),
                reason,
                decl,
            })
        })
        .collect::<Vec<_>>();
    SearchPage {
        query: Arc::from(query),
        coverage: CoverageLine::new(coverage, u64::try_from(rows.len()).ok()),
        rows: rows.into(),
        next: next.map(|cursor| SearchContinuation { cursor, worker }),
    }
}

// ---------------------------------------------------------------------------
// Orbit
// ---------------------------------------------------------------------------

/// Everything the Orbit model is assembled from.
#[derive(Debug)]
pub struct OrbitInputs<'a> {
    /// `packages` reply rows (the shelf).
    pub packages: Result<&'a [Row], &'a ClientError>,
    /// `projects` surface reply.
    pub projects: Result<&'a SurfaceReply, &'a ClientError>,
    /// `explore` surface reply.
    pub explore: Result<&'a SurfaceReply, &'a ClientError>,
    /// `tree` surface reply.
    pub tree: Result<&'a SurfaceReply, &'a ClientError>,
}

fn tree_subject(subject: &backend_library::TreeSubject) -> TreeSubject {
    match subject {
        backend_library::TreeSubject::Package(reference) => {
            TreeSubject::Package(PackageRef::from_reference(reference.clone()))
        }
        backend_library::TreeSubject::Declaration(label) => {
            TreeSubject::Declaration(Arc::from(label.as_str()))
        }
        backend_library::TreeSubject::Explore(query) => {
            TreeSubject::Explore(query.as_ref().map(|query| Arc::from(query.as_str())))
        }
        backend_library::TreeSubject::Search(query) => TreeSubject::Search(Arc::from(query.as_str())),
        backend_library::TreeSubject::Owner(owner) => TreeSubject::Owner(Arc::from(owner.as_str())),
    }
}

/// Assembles the Orbit model.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per model; splitting hides the field-by-field correspondence
pub fn orbit_model(inputs: &OrbitInputs<'_>) -> OrbitModel {
    let indexed = match inputs.packages {
        Ok(rows) => Known::Known(
            rows.iter()
                .filter(|row| matches!(row.id, RowId::Package(_)))
                .filter_map(|row| {
                    let package = PackageRef::parse(&row.label).ok()?;
                    Some(IndexedPackage {
                        name: Arc::from(package.display_name()),
                        readiness: match row.state {
                            backend_library::RowState::Ready => Readiness::Ready,
                            backend_library::RowState::Loading => Readiness::Indexing,
                            backend_library::RowState::Failed => Readiness::Failed,
                        },
                        package,
                    })
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        Err(error) => Known::Unknown(client_gap(error)),
    };
    let projects = match inputs.projects {
        Ok(SurfaceReply::Projects(records)) => Known::Known(
            records
                .iter()
                .map(|record| OrbitProject {
                    id: record.id.get(),
                    name: Arc::from(record.name.as_str()),
                    lockfile: record.lockfile.as_ref().map(|path| Arc::from(path.as_str())),
                    members: record
                        .members
                        .iter()
                        .cloned()
                        .map(PackageRef::from_reference)
                        .collect::<Vec<_>>()
                        .into(),
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        other => Known::Unknown(surface_gap(other, "projects")),
    };
    let explore = match inputs.explore {
        Ok(SurfaceReply::Explored(records)) => {
            Known::Known(records.iter().map(registry_record).collect::<Vec<_>>().into())
        }
        other => Known::Unknown(surface_gap(other, "explore")),
    };
    let tree = match inputs.tree {
        Ok(SurfaceReply::Tree(nodes)) => Known::Known(
            nodes
                .iter()
                .map(|node| TreeNode {
                    id: node.id.get(),
                    parent: node.parent.map(backend_library::TreeNodeId::get),
                    subject: tree_subject(&node.subject),
                    title: Arc::from(node.title.as_str()),
                    opener: match &node.opener {
                        backend_library::TreeOpener::Desktop => TreeOpener::Desktop,
                        backend_library::TreeOpener::Cli => TreeOpener::Cli,
                        backend_library::TreeOpener::Mcp(name) => {
                            TreeOpener::Mcp(Arc::from(name.as_str()))
                        }
                    },
                    active: node.active,
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        other => Known::Unknown(surface_gap(other, "tree")),
    };
    OrbitModel {
        indexed,
        projects,
        explore,
        tree,
    }
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// Lowers one owner health report.
#[must_use]
pub fn health_model(report: &HealthReport) -> HealthModel {
    let progress = report.progress();
    let mut ready = Vec::new();
    let mut missing = Vec::new();
    for status in report.capabilities().as_slice() {
        let family: Arc<str> = Arc::from(format!("{:?}", status.family()));
        match status.lifecycle() {
            backend_library::CapabilityLifecycle::Ready
            | backend_library::CapabilityLifecycle::Active
            | backend_library::CapabilityLifecycle::Resident
            | backend_library::CapabilityLifecycle::Installed => ready.push(family),
            other => missing.push(crate::model::pages::MissingCapability {
                family,
                state: Arc::from(format!("{other:?}")),
            }),
        }
    }
    HealthModel {
        lanes: CoverageLine::new(report.coverage(), Some(report.row_count())),
        rows: report.row_count(),
        ingest: IngestModel {
            files_discovered: progress.files_discovered(),
            files_indexed: progress.files_indexed(),
            files_unavailable: progress.files_unavailable(),
            declarations: progress.declarations(),
            languages: progress
                .languages()
                .iter()
                .map(|row| LanguageProgress {
                    language: Arc::from(format!("{:?}", row.language())),
                    files: row.files(),
                    declarations: row.declarations(),
                })
                .collect::<Vec<_>>()
                .into(),
            faults: progress
                .faults()
                .iter()
                .map(|row| FaultProgress {
                    reason: Arc::from(format!("{:?}", row.reason())),
                    files: row.files(),
                })
                .collect::<Vec<_>>()
                .into(),
        },
        ready_capabilities: ready.into(),
        missing_capabilities: missing.into(),
    }
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]
mod tests {
    //! Fixtures mirror replies probed from a live `backend-locald` (the
    //! structural `crates/present` corpus and the semantic `rich_project`
    //! fixture); every test asserts rendered field content, not counts.

    use super::*;
    use crate::model::local_package::{LocalDependency, LocalPackageSource, ReadmeBlock};
    use crate::model::pages::{KindFamily, Standing};
    use backend_library::{
        Basis, CommandFailure, ReferenceRecord, SemanticDeclarationIdentity, SemanticLinkEvidence,
        SemanticSourceSpan, SourceLocation as EngineLocation, object_version, symbol_key,
        view_state_root,
    };

    const PRESENT: &str = "/work/crates/present";
    const RICH: &str = "/work/rich_project";

    fn basis() -> Basis {
        Basis::new(view_state_root(&[]), object_version(b"fixture"))
    }

    fn key(label: &str) -> SymbolKey {
        symbol_key(label)
    }

    struct RowSpec<'a> {
        label: String,
        kind: DeclarationKind,
        signature: Option<&'a str>,
        parent: Option<&'a str>,
        doc: Option<&'a str>,
        site: Option<(&'a str, u32)>,
    }

    fn row(spec: &RowSpec<'_>) -> Row {
        let mut row = Row::new(RowId::Symbol(key(&spec.label)), basis(), spec.label.as_str())
            .with_kind(spec.kind);
        if let Some(signature) = spec.signature {
            row = row.with_signature(signature);
        }
        if let Some(parent) = spec.parent {
            row = row.with_parent(key(parent));
        }
        row = row.with_document(
            spec.doc
                .map(|doc| vec![Fragment::Text(doc.to_owned())])
                .unwrap_or_default(),
        );
        if let Some((path, line)) = spec.site {
            row = row.with_source(EngineLocation::new(path, line).expect("fixture location"));
        }
        row
    }

    fn present(tail: &str) -> String {
        format!("{PRESENT}::{tail}")
    }

    /// The structural outline of `page.rs` and `identity.rs`, as probed.
    fn present_rows() -> Vec<Row> {
        let module = present("page.rs");
        let page = present("page.rs:396::Page");
        let identity_module = present("identity.rs");
        let identity = present("identity.rs:339::Identity");
        let prose = present("page.rs:26::Prose");
        let specs = vec![
            RowSpec { label: module.clone(), kind: DeclarationKind::Module, signature: None, parent: None, doc: None, site: Some(("page.rs", 1)) },
            RowSpec { label: page.clone(), kind: DeclarationKind::Struct, signature: Some("pub struct Page"), parent: Some(&module), doc: Some("One complete declaration page."), site: Some(("page.rs", 396)) },
            RowSpec { label: present("page.rs:399::prose"), kind: DeclarationKind::Field, signature: Some("prose: Box<[Prose]>"), parent: Some(&page), doc: None, site: Some(("page.rs", 399)) },
            RowSpec { label: present("page.rs:410::new"), kind: DeclarationKind::Method, signature: Some("pub fn new(identity: Identity, kind: Option<DeclarationKind>, source: Source) -> Self"), parent: Some(&page), doc: Some("Assembles one page from already-typed parts."), site: Some(("page.rs", 410)) },
            RowSpec { label: present("page.rs:437::with_prose"), kind: DeclarationKind::Method, signature: Some("pub fn with_prose(mut self, prose: impl Into<Box<[Prose]>>) -> Self"), parent: Some(&page), doc: Some("Attaches the producer's captured documentation."), site: Some(("page.rs", 437)) },
            RowSpec { label: present("page.rs:463::identity"), kind: DeclarationKind::Method, signature: Some("pub const fn identity(&self) -> &Identity"), parent: Some(&page), doc: Some("Returns the page's identity."), site: Some(("page.rs", 463)) },
            RowSpec { label: present("page.rs:470::retitle"), kind: DeclarationKind::Method, signature: Some("pub fn retitle(&mut self, title: &str)"), parent: Some(&page), doc: None, site: Some(("page.rs", 470)) },
            RowSpec { label: prose.clone(), kind: DeclarationKind::Enum, signature: Some("pub enum Prose"), parent: Some(&module), doc: Some("One block of producer-captured documentation."), site: Some(("page.rs", 26)) },
            RowSpec { label: present("page.rs:28::Text"), kind: DeclarationKind::Variant, signature: Some("Text(String)"), parent: Some(&prose), doc: Some("A paragraph of readable text."), site: Some(("page.rs", 28)) },
            RowSpec { label: identity_module.clone(), kind: DeclarationKind::Module, signature: None, parent: None, doc: None, site: Some(("identity.rs", 1)) },
            RowSpec { label: identity.clone(), kind: DeclarationKind::Struct, signature: Some("pub struct Identity"), parent: Some(&identity_module), doc: Some("One parsed row identity."), site: Some(("identity.rs", 339)) },
        ];
        specs.iter().map(row).collect()
    }

    fn present_document(label: &str, signature: &str, doc: &str, site: (&str, u32), excerpt: &str) -> Document {
        let mut document = Document::new(
            key(label),
            view_state_root(&[]),
            vec![Fragment::Text(doc.to_owned())],
        );
        document.signature = Some(signature.to_owned());
        document.location = SourceAvailability::Captured(
            EngineLocation::new(site.0, site.1).expect("fixture location"),
        );
        document.excerpt =
            SourceExcerpt::captured(excerpt, SourceExcerptExtent::Complete).expect("excerpt");
        document
    }

    fn no_semantics() -> ClientError {
        ClientError::CommandFailed(CommandFailure::InvalidQuery(
            "references need a complete semantic publication; none is selected for this package"
                .to_owned(),
        ))
    }

    fn names(members: &[Member]) -> Vec<&str> {
        members.iter().map(|member| member.decl.name.as_ref()).collect()
    }

    #[test]
    fn a_structural_struct_page_carries_signature_docs_members_and_honest_gaps() {
        let rows = present_rows();
        let outline = OutlineIndex::new(rows.clone(), true);
        let page_label = present("page.rs:396::Page");
        let coordinate = SymbolRef::new(&page_label).expect("coordinate");
        let document = present_document(
            &page_label,
            "pub struct Page",
            "One complete declaration page.",
            ("page.rs", 396),
            "pub struct Page {\n    identity: Identity,\n    prose: Box<[Prose]>,\n}",
        );
        let related_rows = rows
            .iter()
            .filter(|row| {
                row.label == page_label
                    || row.label == present("page.rs")
                    || row.parent == Some(key(&page_label))
            })
            .cloned()
            .collect::<Vec<_>>();
        let failure = no_semantics();
        let page = symbol_page(&SymbolInputs {
            coordinate: &coordinate,
            document: &document,
            related: Ok(Neighbourhood {
                rows: &related_rows,
                relations: None,
                rich: None,
            }),
            references: Err(&failure),
            outline: Ok(&outline),
        });

        assert_eq!(page.identity.name.as_ref(), "Page");
        assert_eq!(page.identity.kind, Some(DeclarationKind::Struct));
        assert_eq!(page.identity.family, KindFamily::Type);
        assert_eq!(page.identity.path.as_deref(), Some("page.rs"));
        assert_eq!(page.identity.line, Some(396));
        assert_eq!(page.identity.language, Language::Rust);
        assert_eq!(
            page.package.known().map(PackageRef::as_str),
            Some(PRESENT),
            "the package comes from the coordinate's project root"
        );

        let signature = page.signature.known().expect("source signature");
        assert_eq!(signature.text.as_ref(), "pub struct Page");
        let name = signature
            .tokens
            .iter()
            .find(|token| token.class == TokenClass::Name)
            .expect("declared name token");
        assert_eq!(signature.token_text(name), "Page");

        assert_eq!(
            DocFragment::plain_text(&page.docs),
            "One complete declaration page."
        );
        let location = page.site.location.known().expect("captured location");
        assert_eq!((location.path.as_ref(), location.line), ("page.rs", 396));
        let excerpt = page.site.excerpt.known().expect("captured excerpt");
        assert!(excerpt.text.starts_with("pub struct Page {"));
        assert_eq!(excerpt.lines, Some(LineSpan { first: 396, last: 399 }));

        let members = page.members.known().expect("members");
        assert_eq!(names(&members.made_of), ["prose"]);
        let receiver_of = |name: &str| {
            members
                .does
                .iter()
                .find(|group| group.members.iter().any(|member| member.decl.name.as_ref() == name))
                .map(|group| group.receiver)
        };
        assert_eq!(receiver_of("new"), Some(Receiver::Makes));
        assert_eq!(receiver_of("identity"), Some(Receiver::Reads));
        assert_eq!(receiver_of("retitle"), Some(Receiver::Changes));
        assert_eq!(receiver_of("with_prose"), Some(Receiver::Consumes));
        let identity_member = members
            .all()
            .find(|member| member.decl.name.as_ref() == "identity")
            .expect("identity accessor");
        assert_eq!(identity_member.summary.as_deref(), Some("Returns the page's identity."));
        // `new(identity: Identity, …)` links the Identity type by name.
        let new_member = members
            .all()
            .find(|member| member.decl.name.as_ref() == "new")
            .expect("constructor");
        let new_signature = new_member.signature.known().expect("constructor signature");
        let link = new_signature
            .links()
            .find(|token| new_signature.token_text(token) == "Identity")
            .and_then(|token| token.link.as_ref())
            .expect("Identity is linked");
        assert_eq!(link.target.as_str(), present("identity.rs:339::Identity"));
        assert_eq!(link.provenance, Provenance::ByName);

        // Structural replies know containment and nothing typed.
        let down = page.rose.down.known().expect("containment");
        assert!(down.iter().any(|relation| relation.decl.name.as_ref() == "prose"));
        assert_eq!(
            page.rose.up.gap().map(|gap| gap.reason),
            Some(GapReason::NoSemanticPublication)
        );
        assert_eq!(
            page.rose.left.gap().map(|gap| gap.reason),
            Some(GapReason::NoSemanticPublication)
        );
        let references = page.references.gap().expect("references are unknown");
        assert_eq!(references.reason, GapReason::NoSemanticPublication);
        assert!(references.detail.contains("complete semantic publication"));

        let position = page.outline.known().expect("outline position");
        assert_eq!(
            position.ancestors.iter().map(|decl| decl.name.as_ref()).collect::<Vec<_>>(),
            ["page.rs"]
        );
        let siblings = position.siblings.iter().map(|decl| decl.name.as_ref()).collect::<Vec<_>>();
        assert_eq!(siblings, ["Prose", "Page"], "outline order is source order");
        assert_eq!(position.index, Some(1));
    }

    #[test]
    fn an_enum_page_lists_its_variants_as_made_of() {
        let rows = present_rows();
        let outline = OutlineIndex::new(rows, true);
        let children = outline.children(key(&present("page.rs:26::Prose"))).collect::<Vec<_>>();
        let members = members(Some(DeclarationKind::Enum), &children, Some(&outline));
        assert_eq!(names(&members.made_of), ["Text"]);
        assert_eq!(
            members.made_of[0].summary.as_deref(),
            Some("A paragraph of readable text.")
        );
    }

    fn rich(tail: &str) -> String {
        format!("{RICH}::semantic::{tail}")
    }

    /// The semantic rows for `impl Marker for Boxed {}`, as probed.
    fn rich_rows() -> (Vec<Row>, Vec<GraphRelation>) {
        let marker = rich("1bf97a0b::Marker");
        let block = rich("b578f79d::Boxed");
        let boxed = rich("18207de4::Boxed");
        let rows = vec![
            row(&RowSpec { label: marker.clone(), kind: DeclarationKind::Trait, signature: Some("pub trait Marker"), parent: None, doc: None, site: Some(("src/lib.rs", 26)) }),
            row(&RowSpec { label: block.clone(), kind: DeclarationKind::Type, signature: Some("nominal(entity(family=x\"18207DE4\",variant=x\"D58F\",name=x\"426F786564\"))"), parent: None, doc: None, site: None }),
            row(&RowSpec { label: boxed.clone(), kind: DeclarationKind::Struct, signature: Some("nominal(entity(family=x\"18207DE4\",variant=x\"D58F\",name=x\"426F786564\"))"), parent: None, doc: None, site: None }),
            row(&RowSpec { label: rich("43a81a00::value"), kind: DeclarationKind::Field, signature: Some("pub value: i32"), parent: Some(&boxed), doc: None, site: Some(("src/lib.rs", 12)) }),
        ];
        let relations = vec![
            GraphRelation::new(RowId::Symbol(key(&block)), RowId::Symbol(key(&marker)), SemanticLinkKind::TypeReference),
            GraphRelation::new(RowId::Symbol(key(&block)), RowId::Symbol(key(&boxed)), SemanticLinkKind::TypeReference),
        ];
        (rows, relations)
    }

    #[test]
    fn a_semantic_type_is_marker_through_its_impl_block_and_says_it_was_derived() {
        let (rows, relations) = rich_rows();
        let boxed = rich("18207de4::Boxed");
        let hood = Neighbourhood {
            rows: &rows,
            relations: Some(&relations),
            rich: None,
        };
        let rose = rose(RowId::Symbol(key(&boxed)), Some(DeclarationKind::Struct), Some(hood), None);
        let up = rose.up.known().expect("typed up");
        let marker = up.first().expect("Boxed is Marker");
        assert_eq!(marker.decl.name.as_ref(), "Marker");
        assert_eq!(marker.kind, RelationKind::Semantic(SemanticLinkKind::Implements));
        assert_eq!(
            marker.provenance,
            Provenance::Derived {
                via: Derivation::ImplBlock,
                confidence: SemanticConfidence::Compiler,
            }
        );
        assert_eq!(marker.arrival, Arrival::NotReported);
        assert_eq!(
            marker.via.as_ref().map(|via| via.coordinate.as_str()),
            Some(rich("b578f79d::Boxed").as_str())
        );
        // The impl block's own edge to the struct is an incoming type use.
        let left = rose.left.known().expect("typed left");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, RelationKind::Semantic(SemanticLinkKind::TypeReference));
        let down = rose.down.known().expect("containment");
        assert_eq!(down[0].decl.name.as_ref(), "value");

        // The struct's own signature is compiler-encoded, and says so.
        let signature = signature_text(rows[2].signature.as_deref(), Language::Rust, None, None);
        assert_eq!(signature.gap().map(|gap| gap.reason), Some(GapReason::Encoded));
    }

    #[test]
    fn a_trait_page_lists_impl_blocks_as_implementors_and_reference_spans_exactly() {
        let (rows, relations) = rich_rows();
        let marker = rich("1bf97a0b::Marker");
        let hood = Neighbourhood {
            rows: &rows,
            relations: Some(&relations),
            rich: None,
        };
        let rose = rose(RowId::Symbol(key(&marker)), Some(DeclarationKind::Trait), Some(hood), None);
        let implementors = rose.implemented_by.known().expect("implementors");
        assert_eq!(implementors.len(), 1);
        assert_eq!(implementors[0].decl.name.as_ref(), "Boxed");
        assert_eq!(implementors[0].decl.kind, Some(DeclarationKind::Type));
        assert!(matches!(
            implementors[0].provenance,
            Provenance::Derived { via: Derivation::ImplBlock, .. }
        ));
        assert!(rose.up.known().expect("typed up").is_empty(), "Marker has no supertrait");

        let reply = SurfaceReply::References {
            target: backend_library::ProductText::new(marker.clone()).expect("target"),
            references: Box::new([ReferenceRecord {
                site: backend_library::ProductText::new(rich("b578f79d::Boxed")).expect("site"),
                target: SemanticLinkTarget::Local {
                    declaration: SemanticDeclarationIdentity {
                        family: [1; 16],
                        variant: [2; 16],
                    },
                },
                relation: SemanticLinkKind::TypeReference,
                evidence: SemanticLinkEvidence {
                    confidence: SemanticConfidence::Compiler,
                    source: Some(SemanticSourceSpan {
                        file: backend_library::ProductText::new("src/lib.rs").expect("file"),
                        start: 313,
                        end: 319,
                    }),
                },
            }]),
        };
        let outline = OutlineIndex::new(rows, true);
        let sites = references(Ok(&reply), Some(&outline));
        let sites = sites.known().expect("reference sites");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].site.name.as_ref(), "Boxed");
        assert_eq!(sites[0].site.kind, Some(DeclarationKind::Type), "resolved through the outline");
        assert_eq!(sites[0].confidence, SemanticConfidence::Compiler);
        assert_eq!(sites[0].scope, ReferenceScope::Local);
        let span = sites[0].span.known().expect("captured span");
        assert_eq!(span.file.as_ref(), "src/lib.rs");
        assert_eq!((span.bytes.start, span.bytes.end), (313, 319));
    }

    #[test]
    fn a_registry_dossier_keeps_version_standing_downloads_and_producer_gaps() {
        let package = PackageRef::parse("pkg:cargo/beta@1.0.0").expect("package");
        let record = crate::runtime::tests::registry_record("beta", "1.0.0");
        let mut yanked = crate::runtime::tests::registry_record("beta", "0.9.0");
        yanked.standing = RegistryReleaseStanding::Yanked;
        yanked.downloads = RegistryDownloadCount::Unavailable(RegistryFactAvailability::NotRecorded);
        let records = SurfaceReply::Package(Box::new([record.clone()]));
        let versions = SurfaceReply::PackageVersions(Box::new([yanked, record]));
        let dependencies = SurfaceReply::Dependencies(backend_library::DependencyFacts::Known(Box::new([
            backend_library::PackageDependencyRecord::new(
                package.reference().clone(),
                backend_library::PackageDependencyTarget::new(
                    backend_library::RegistryEcosystem::Cargo,
                    "serde",
                    "^1.0",
                    None,
                )
                .expect("target"),
                backend_library::DependencyScope::Runtime,
                false,
                backend_library::DependencyEvidence {
                    authority: backend_library::DependencyAuthority::RegistryMetadata,
                    frontier: [7; 32],
                    provenance: [8; 32],
                },
            ),
        ])));
        let dependents = SurfaceReply::Dependents(backend_library::RegistryMetadata::NotRecorded(
            backend_library::ProductText::new("the configured feed does not record dependency metadata")
                .expect("reason"),
        ));
        let dossier = package_dossier(&PackageInputs {
            package: &package,
            records: Ok(&records),
            versions: Ok(&versions),
            dependencies: Ok(&dependencies),
            dependents: Ok(&dependents),
            outline: Err(Gap::new(GapReason::ReadFailed, "outline refused")),
            local: None,
        });
        let head = dossier.record.known().expect("record");
        assert_eq!(head.name.as_ref(), "beta");
        assert_eq!(head.version.known().map(AsRef::as_ref), Some("1.0.0"));
        assert_eq!(head.ecosystem.known().map(AsRef::as_ref), Some("cargo"));
        assert_eq!(head.standing.known(), Some(&Standing::Available));
        assert_eq!(head.downloads.known(), Some(&Downloads::Exact(42)));
        assert_eq!(head.bytes.known(), Some(&1_024));
        assert_eq!(head.description.gap().map(|gap| gap.reason), Some(GapReason::NotServed));
        let versions = dossier.versions.known().expect("versions");
        let summary = versions
            .iter()
            .map(|entry| (entry.version.as_ref(), entry.standing, entry.current))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            [("0.9.0", Standing::Yanked, false), ("1.0.0", Standing::Available, true)]
        );
        let dependencies = dossier.dependencies.known().expect("dependencies");
        assert_eq!(dependencies[0].name.as_ref(), "serde");
        assert_eq!(dependencies[0].requirement.as_ref(), "^1.0");
        assert_eq!(dependencies[0].scope, DependencyScope::Runtime);
        let dependents = dossier.dependents.gap().expect("dependents are not recorded");
        assert_eq!(dependents.reason, GapReason::NotRecorded);
        assert_eq!(dependents.detail.as_ref(), "the configured feed does not record dependency metadata");
        assert_eq!(dossier.readme.gap().map(|gap| gap.reason), Some(GapReason::NotServed));
        assert_eq!(dossier.outline.gap().map(|gap| gap.detail.as_ref()), Some("outline refused"));
    }

    #[test]
    fn a_local_dossier_reads_its_manifest_and_never_claims_registry_facts() {
        let package = PackageRef::parse(PRESENT).expect("local package");
        let local = LocalPackage {
            project: crate::core::LocalProjectId::new(PRESENT).expect("project"),
            source: LocalPackageSource::Cargo,
            name: Arc::from("backend-present"),
            version: Some(Arc::from("0.3.0")),
            description: None,
            license: Some(Arc::from("MIT")),
            rust_version: None,
            repository: None,
            homepage: None,
            documentation: None,
            keywords: Arc::from([]),
            categories: Arc::from([]),
            readme: Arc::from([ReadmeBlock::Paragraph(Arc::from("Presentation model."))]),
            dependencies: Arc::from([LocalDependency {
                name: Arc::from("backend-library"),
                requirement: Arc::from("*"),
                kind: DependencyKind::Normal,
                users: 1,
            }]),
            features: Arc::from([]),
            members: 1,
        };
        let records = SurfaceReply::Package(Box::new([]));
        let versions = SurfaceReply::PackageVersions(Box::new([]));
        let dependencies = SurfaceReply::Dependencies(backend_library::DependencyFacts::Unavailable(
            backend_library::ProductText::new("dependency facts are unavailable because the package is not recorded")
                .expect("reason"),
        ));
        let dependents = SurfaceReply::Dependents(backend_library::RegistryMetadata::NotRecorded(
            backend_library::ProductText::new("not recorded").expect("reason"),
        ));
        let outline = OutlineIndex::new(present_rows(), true);
        let dossier = package_dossier(&PackageInputs {
            package: &package,
            records: Ok(&records),
            versions: Ok(&versions),
            dependencies: Ok(&dependencies),
            dependents: Ok(&dependents),
            outline: Ok(&outline),
            local: Some(&local),
        });
        let head = dossier.record.known().expect("manifest record");
        assert_eq!(head.source, RecordSource::LocalManifest);
        assert_eq!(head.name.as_ref(), "backend-present");
        assert_eq!(head.version.known().map(AsRef::as_ref), Some("0.3.0"));
        assert_eq!(head.license.known().map(AsRef::as_ref), Some("MIT"));
        assert_eq!(head.downloads.gap().map(|gap| gap.reason), Some(GapReason::LocalProject));
        assert_eq!(dossier.versions.gap().map(|gap| gap.reason), Some(GapReason::LocalProject));
        assert_eq!(dossier.dependents.gap().map(|gap| gap.reason), Some(GapReason::LocalProject));
        assert_eq!(
            dossier.dependencies.known().map(|deps| deps[0].name.as_ref()),
            Some("backend-library")
        );
        let tree = dossier.outline.known().expect("mosaic tree");
        let modules = tree.roots.iter().map(|node| node.decl.name.as_ref()).collect::<Vec<_>>();
        assert_eq!(modules, ["identity.rs", "page.rs"]);
        let page_module = &tree.roots[1];
        let items = page_module
            .children
            .iter()
            .map(|node| (node.decl.name.as_ref(), node.decl.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            items,
            [("Prose", Some(DeclarationKind::Enum)), ("Page", Some(DeclarationKind::Struct))]
        );
        assert_eq!(tree.count(), 11);
        assert!(matches!(
            dossier.readme.known().map(AsRef::as_ref),
            Some([ReadmeBlock::Paragraph(text)]) if text.as_ref() == "Presentation model."
        ));
    }

    #[test]
    fn search_rows_keep_producer_order_and_say_why_each_matched() {
        let rows = vec![
            row(&RowSpec { label: present("record.rs:91::from_row"), kind: DeclarationKind::Method, signature: Some("pub fn from_row(row: &Row) -> Self"), parent: None, doc: Some("Lowers one engine row into a result record."), site: Some(("record.rs", 91)) }),
            row(&RowSpec { label: present("drive.rs:116::Engine"), kind: DeclarationKind::Trait, signature: Some("pub trait Engine"), parent: None, doc: Some("Whatever a surface talks to in order to read an admitted reply."), site: Some(("drive.rs", 116)) }),
            row(&RowSpec { label: present("drive.rs:200::answer"), kind: DeclarationKind::Function, signature: Some("pub fn answer(engine: &mut dyn Engine) -> Answer"), parent: None, doc: None, site: Some(("drive.rs", 200)) }),
        ];
        let page = search_rows("engine", &rows, &[backend_library::Coverage::Complete], None, 0);
        let summary = page
            .rows
            .iter()
            .map(|row| (row.rank, row.decl.name.as_ref(), row.reason))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            [
                (0, "from_row", MatchReason::Docs),
                (1, "Engine", MatchReason::ExactName),
                (2, "answer", MatchReason::Signature),
            ]
        );
        assert_eq!(page.rows[1].snippet.as_deref(), Some("Whatever a surface talks to in order to read an admitted reply."));
        assert_eq!(page.rows[1].package.as_deref(), Some(PRESENT));
        assert_eq!(page.rows[1].score.gap().map(|gap| gap.reason), Some(GapReason::NotServed));
        assert!(page.next.is_none());
    }

    #[test]
    fn a_local_source_view_is_the_whole_verified_file_with_linked_identifiers() {
        let rows = present_rows();
        let outline = OutlineIndex::new(rows, true);
        let label = present("page.rs:3::render");
        let file = "use crate::Page;\n\npub fn render(page: &Page) -> Identity {\n    page.identity().clone()\n}\n";
        let mut document = present_document(
            &label,
            "pub fn render(page: &Page) -> Identity",
            "Renders one page.",
            ("page.rs", 3),
            "pub fn render(page: &Page) -> Identity {\n    page.identity().clone()\n}",
        );
        document.symbol = key(&label);
        let coordinate = SymbolRef::new(&label).expect("coordinate");
        let uses = Known::Known(Arc::from([ReferenceSite {
            site: DeclRef::from_label(&label, None, None, None).expect("site"),
            relation: SemanticLinkKind::TypeReference,
            confidence: SemanticConfidence::Compiler,
            span: Known::Known(FileSpan {
                file: Arc::from("page.rs"),
                bytes: ByteSpan::new(11, 15).expect("span"),
            }),
            scope: ReferenceScope::Local,
        }]));
        let view = source_view(&coordinate, &document, Some(file), &uses, Some(&outline));
        let text = view.text.known().expect("source text");
        assert_eq!(text.origin, SourceOrigin::LocalFile);
        assert_eq!(text.first_line, 1);
        assert_eq!(text.text.as_ref(), file);
        assert_eq!(view.declaration.known(), Some(&LineSpan { first: 3, last: 5 }));
        assert_eq!(view.file.known().map(AsRef::as_ref), Some("page.rs"));
        let linked = view
            .identifiers
            .known()
            .expect("identifiers")
            .iter()
            .map(|span| (&text.text[span.span.range()], span.link.target.as_str().to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(
            linked,
            [
                ("Page", present("page.rs:396::Page")),
                ("Page", present("page.rs:396::Page")),
                ("Identity", present("identity.rs:339::Identity")),
            ]
        );
        let uses = view.uses.known().expect("uses in this file");
        assert_eq!(&text.text[uses[0].range()], "Page");

        // A file that no longer matches the excerpt is not trusted.
        let stale = "// edited since indexing\n";
        let view = source_view(&coordinate, &document, Some(stale), &Known::Known(Arc::from([])), Some(&outline));
        let text = view.text.known().expect("excerpt text");
        assert_eq!(text.origin, SourceOrigin::Excerpt);
        assert_eq!(text.first_line, 3);
        assert_eq!(view.uses.gap().map(|gap| gap.reason), Some(GapReason::NotServed));
    }

    #[test]
    fn health_keeps_lane_reasons_and_ingest_counts() {
        let root = view_state_root(&[]);
        let report = HealthReport::from_admitted_parts(
            backend_library::RevisionReceipt::new(root, backend_library::Cursor::at(root, 1), object_version(b"fixture")),
            basis(),
            Box::new([
                backend_library::Coverage::Complete,
                backend_library::Coverage::Unavailable {
                    lane: backend_library::Lane::Semantic,
                    reason: backend_library::Reason::Unconfigured,
                },
            ]),
            1_270,
            backend_library::CapabilityInventory::explicitly_unavailable(),
        )
        .with_progress(
            backend_library::IngestProgress::new(
                23,
                22,
                1,
                1_269,
                vec![backend_library::LanguageRows::new(backend_library::SourceLanguage::Rust, 22, 1_269)],
                Vec::new(),
            )
            .expect("progress"),
        );
        let health = health_model(&report);
        assert_eq!(health.rows, 1_270);
        assert_eq!(health.ingest.files_discovered, 23);
        assert_eq!(health.ingest.files_indexed, 22);
        assert_eq!(health.ingest.files_unavailable, 1);
        assert_eq!(health.ingest.declarations, 1_269);
        assert_eq!(health.ingest.languages[0].language.as_ref(), "Rust");
        assert_eq!(health.ingest.facets(), Some(12));
        let semantic = health.lanes.lanes()[3];
        assert_eq!(semantic.name(), "semantic");
        assert!(matches!(
            semantic.state(),
            backend_present::LaneState::Unavailable { reason: backend_library::Reason::Unconfigured }
        ));
        assert!(health.ready_capabilities.is_empty());
        assert!(!health.missing_capabilities.is_empty());
    }

    #[test]
    fn encoded_signatures_are_recognised_and_source_text_is_not() {
        for encoded in [
            "nominal(entity(family=x\"18\",variant=x\"D5\",name=x\"42\"))",
            "builtin(i32)",
            "reference(mutability=immutable,lifetime=none,target=builtin(i32))",
            "function(parameters=[],results=[],abi=none,variadic=none,unsafe=false)",
        ] {
            assert!(is_encoded_signature(encoded), "{encoded}");
        }
        for source in [
            "pub struct Page",
            "Page(Box<Page>)",
            "pub value: i32",
            "def build(self, path: str) -> None",
            "func (s *Server) Serve() error",
        ] {
            assert!(!is_encoded_signature(source), "{source}");
        }
    }
}
