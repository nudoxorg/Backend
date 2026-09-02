//! Documentation-browser state and the built-in catalog projected by the GPUI shell.
//!
//! The catalog describes the public capabilities that are already present in this workspace. It
//! is deliberately presentation data: business requests still cross [`interface_core`] and the
//! shell never claims that a lower-plane index exists when the service reports it unavailable.

use interface_core::InputText;

/// Number of packages represented by the first-party documentation catalog.
pub const PACKAGE_COUNT: usize = 5;
/// Maximum rows shown by the global documentation search surface.
pub const MAX_DOCUMENT_RESULTS: usize = 64;

/// A documentation item kind with a stable visual treatment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentKind {
    /// A public structure.
    Struct,
    /// A public enumeration.
    Enum,
    /// A public trait.
    Trait,
    /// A public function or method.
    Function,
    /// A public module.
    Module,
}

impl DocumentKind {
    /// Short reader-facing label used in dense rows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Function => "fn",
            Self::Module => "mod",
        }
    }
}

/// One documented field, method, variant, or associated value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentMember {
    /// Rendered source-like signature.
    pub signature: &'static str,
    /// Compact explanation shown beneath the signature.
    pub summary: &'static str,
}

/// One first-party public symbol rendered in the reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentItem {
    /// Stable package index.
    pub package: usize,
    /// Stable leaf name.
    pub name: &'static str,
    /// Fully-qualified public path.
    pub path: &'static str,
    /// Public declaration kind.
    pub kind: DocumentKind,
    /// Familiar source-like declaration.
    pub signature: &'static str,
    /// One-line purpose statement.
    pub summary: &'static str,
    /// Reader-sized documentation paragraphs.
    pub paragraphs: &'static [&'static str],
    /// Optional executable-style usage example.
    pub example: Option<&'static str>,
    /// Public members or variants.
    pub members: &'static [DocumentMember],
    /// Related symbols expressed as qualified paths.
    pub related: &'static [&'static str],
    /// Repository-relative source location.
    pub source: &'static str,
    /// Whether the item is promoted on the landing page.
    pub featured: bool,
}

/// Package metadata shown by the package browser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentPackage {
    /// Cargo package name.
    pub name: &'static str,
    /// Current workspace version.
    pub version: &'static str,
    /// Concise package purpose.
    pub summary: &'static str,
    /// First item index owned by this package.
    pub first_item: usize,
    /// Number of catalog items owned by this package.
    pub item_count: usize,
    /// Compact capability tags.
    pub tags: &'static [&'static str],
}

/// Restriction selected on the documentation search surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DocumentFilter {
    /// Search every kind.
    #[default]
    All,
    /// Search structures and enumerations.
    Types,
    /// Search traits and functions.
    APIs,
    /// Search modules only.
    Modules,
}

/// Which indexed population the global search surface returns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DocumentSearchScope {
    /// Packages and symbols in one ranked surface.
    #[default]
    Everything,
    /// Public symbols only.
    Symbols,
    /// Package records only.
    Packages,
}

impl DocumentSearchScope {
    /// Reader-facing chip label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Everything => "Everything",
            Self::Symbols => "Symbols",
            Self::Packages => "Packages",
        }
    }
}

impl DocumentFilter {
    /// Reader-facing chip label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Types => "Types",
            Self::APIs => "APIs",
            Self::Modules => "Modules",
        }
    }

    const fn accepts(self, kind: DocumentKind) -> bool {
        match self {
            Self::All => true,
            Self::Types => matches!(kind, DocumentKind::Struct | DocumentKind::Enum),
            Self::APIs => matches!(kind, DocumentKind::Trait | DocumentKind::Function),
            Self::Modules => matches!(kind, DocumentKind::Module),
        }
    }
}

/// One ranked search result referencing a stable catalog item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentSearchHit {
    /// Stable index into [`DOCUMENT_ITEMS`].
    pub item: usize,
    /// Deterministic lexical relevance score.
    pub score: u16,
}

/// One keyboard-selectable row in the unified package and symbol search surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentSearchRow {
    /// A package record from the local catalog.
    Package(usize),
    /// A ranked public symbol.
    Symbol(DocumentSearchHit),
}

/// Reader-local navigation and disclosure state.
#[derive(Debug, Eq, PartialEq)]
pub struct DocumentationState {
    /// Whether the package/sidebar column is contracted.
    pub sidebar_collapsed: bool,
    /// Whether the on-page outline is visible.
    pub outline_visible: bool,
    /// Stable selected catalog item.
    pub selected_item: usize,
    /// Per-package tree disclosure.
    pub expanded_packages: [bool; PACKAGE_COUNT],
    /// Active documentation query, if nonempty.
    pub query: Option<InputText>,
    /// Search filter chip.
    pub filter: DocumentFilter,
    /// Indexed population selected by the reader.
    pub scope: DocumentSearchScope,
    /// Selected row in the current ranked result list.
    pub selected_result: usize,
    /// Whether the lower source section is expanded.
    pub source_expanded: bool,
    /// Whether the backend operations drawer is expanded.
    pub operations_expanded: bool,
    /// Packages pinned into the library tree. Unpinned packages remain discoverable in search.
    pub library_packages: [bool; PACKAGE_COUNT],
}

impl Default for DocumentationState {
    fn default() -> Self {
        Self {
            sidebar_collapsed: false,
            outline_visible: true,
            selected_item: 0,
            expanded_packages: [true, false, false, false, false],
            query: None,
            filter: DocumentFilter::All,
            scope: DocumentSearchScope::Everything,
            selected_result: 0,
            source_expanded: false,
            operations_expanded: false,
            library_packages: [true, true, true, false, false],
        }
    }
}

impl DocumentationState {
    /// Returns the current query without exposing its fixed transport storage.
    #[must_use]
    pub fn query_text(&self) -> &str {
        self.query.as_ref().map_or("", |query| query)
    }

    /// Replaces the global documentation query and resets row selection.
    pub fn replace_query(&mut self, query: InputText) {
        self.query = (!query.as_ref().is_empty()).then_some(query);
        self.selected_result = 0;
    }

    /// Clears the global query without constructing an empty transport value.
    pub fn clear_query(&mut self) {
        self.query = None;
        self.selected_result = 0;
    }

    /// Selects a filter and resets row selection.
    pub fn select_filter(&mut self, filter: DocumentFilter) {
        self.filter = filter;
        self.selected_result = 0;
    }

    /// Selects the indexed population used by global search.
    pub fn select_scope(&mut self, scope: DocumentSearchScope) {
        self.scope = scope;
        self.selected_result = 0;
    }

    /// Pins or unpins one known package from the library tree.
    pub fn set_package_added(&mut self, package: usize, added: bool) {
        if let Some(installed) = self.library_packages.get_mut(package) {
            *installed = added;
            if added && let Some(expanded) = self.expanded_packages.get_mut(package) {
                *expanded = true;
            }
        }
    }

    /// Selects an item and ensures its owning package is disclosed.
    pub fn select_item(&mut self, item: usize) {
        if let Some(document) = DOCUMENT_ITEMS.get(item) {
            self.selected_item = item;
            if let Some(expanded) = self.expanded_packages.get_mut(document.package) {
                *expanded = true;
            }
            self.source_expanded = false;
        }
    }

    /// Toggles one package disclosure when the package exists.
    pub fn toggle_package(&mut self, package: usize) {
        if let Some(expanded) = self.expanded_packages.get_mut(package) {
            *expanded = !*expanded;
        }
    }

    /// Computes deterministic lexical results over names, paths, signatures, and summaries.
    #[must_use]
    pub fn search_hits(&self) -> Vec<DocumentSearchHit> {
        let query = self.query_text().trim();
        let mut hits = Vec::with_capacity(DOCUMENT_ITEMS.len());
        for (item, document) in DOCUMENT_ITEMS.iter().enumerate() {
            if !self.filter.accepts(document.kind) {
                continue;
            }
            let score = if query.is_empty() {
                if document.featured { 40 } else { 10 }
            } else {
                search_score(document, query)
            };
            if score != 0 {
                hits.push(DocumentSearchHit { item, score });
            }
        }
        hits.sort_by(|left, right| {
            right.score.cmp(&left.score).then_with(|| {
                DOCUMENT_ITEMS[left.item]
                    .path
                    .cmp(DOCUMENT_ITEMS[right.item].path)
            })
        });
        hits.truncate(MAX_DOCUMENT_RESULTS);
        hits
    }

    /// Computes ranked package-name and summary matches from the same visible query.
    #[must_use]
    pub fn package_hits(&self) -> Vec<usize> {
        let query = self.query_text().trim();
        let mut hits = Vec::with_capacity(DOCUMENT_PACKAGES.len());
        for (index, package) in DOCUMENT_PACKAGES.iter().enumerate() {
            if query.is_empty()
                || contains_ascii(package.name, query)
                || query.split_whitespace().all(|token| {
                    contains_ascii(package.name, token) || contains_ascii(package.summary, token)
                })
            {
                hits.push(index);
            }
        }
        hits.sort_by_key(|index| {
            let package = DOCUMENT_PACKAGES[*index];
            let exact = u8::from(eq_ascii(package.name, query));
            let prefix = u8::from(starts_ascii(package.name, query));
            (
                core::cmp::Reverse(exact),
                core::cmp::Reverse(prefix),
                package.name,
            )
        });
        hits
    }

    /// Returns the exact ordered rows shown by the unified search surface.
    #[must_use]
    pub fn search_rows(&self) -> Vec<DocumentSearchRow> {
        let mut rows = Vec::new();
        if self.scope != DocumentSearchScope::Symbols {
            rows.extend(
                self.package_hits()
                    .into_iter()
                    .map(DocumentSearchRow::Package),
            );
        }
        if self.scope != DocumentSearchScope::Packages {
            rows.extend(
                self.search_hits()
                    .into_iter()
                    .map(DocumentSearchRow::Symbol),
            );
        }
        rows.truncate(MAX_DOCUMENT_RESULTS);
        rows
    }

    /// Moves the ranked result selection, clamped to the current result set.
    pub fn move_result_selection(&mut self, forward: bool) {
        let count = self.search_rows().len();
        if count == 0 {
            self.selected_result = 0;
        } else if forward {
            self.selected_result = (self.selected_result + 1).min(count - 1);
        } else {
            self.selected_result = self.selected_result.saturating_sub(1);
        }
    }

    /// Returns the catalog item selected in the current result list.
    #[must_use]
    pub fn selected_search_item(&self) -> Option<usize> {
        self.search_rows()
            .get(self.selected_result)
            .and_then(|row| match row {
                DocumentSearchRow::Symbol(hit) => Some(hit.item),
                DocumentSearchRow::Package(_) => None,
            })
    }

    /// Returns the currently keyboard-selected unified search row.
    #[must_use]
    pub fn selected_search_row(&self) -> Option<DocumentSearchRow> {
        self.search_rows().get(self.selected_result).copied()
    }
}

fn search_score(item: &DocumentItem, query: &str) -> u16 {
    let mut score = 0_u16;
    for token in query.split_whitespace() {
        let token_score = if eq_ascii(item.name, token) {
            120
        } else if starts_ascii(item.name, token) {
            90
        } else if contains_ascii(item.name, token) {
            70
        } else if contains_ascii(item.path, token) {
            48
        } else if contains_ascii(item.signature, token) {
            28
        } else if contains_ascii(item.summary, token)
            || item
                .paragraphs
                .iter()
                .any(|paragraph| contains_ascii(paragraph, token))
        {
            12
        } else {
            return 0;
        };
        score = score.saturating_add(token_score);
    }
    score.saturating_add(u16::from(item.featured))
}

fn eq_ascii(value: &str, query: &str) -> bool {
    value.eq_ignore_ascii_case(query)
}

fn starts_ascii(value: &str, query: &str) -> bool {
    value
        .get(..query.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(query))
}

fn contains_ascii(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.as_bytes();
    value
        .as_bytes()
        .windows(query.len())
        .any(|window| window.eq_ignore_ascii_case(query))
}

const IR_MEMBERS: &[DocumentMember] = &[
    DocumentMember {
        signature: "pub fn item(&self, id: EntityId) -> Option<ItemView<'_>>",
        summary: "Borrows one canonical declaration by dense identity.",
    },
    DocumentMember {
        signature: "pub fn signature(&self, item: EntityId) -> Option<SignatureDisplay<'_>>",
        summary: "Streams a source-like signature without intermediate allocation.",
    },
    DocumentMember {
        signature: "pub fn display_docs(&self, item: EntityId) -> Option<DocsDisplay<'_>>",
        summary: "Streams interned documentation as Markdown.",
    },
    DocumentMember {
        signature: "pub fn embedding_text(&self, item: EntityId, profile: EmbeddingProfile) -> Option<EmbeddingDisplay<'_>>",
        summary: "Combines signature, docs, and graph context for tokenization.",
    },
];

const BUILDER_MEMBERS: &[DocumentMember] = &[
    DocumentMember {
        signature: "pub fn new() -> Self",
        summary: "Creates an empty semantic builder.",
    },
    DocumentMember {
        signature: "pub fn add_frontend_tree<T: FrontendTree>(&mut self, tree: &T) -> Result<EntityRange, BuildError>",
        summary: "Admits a language frontend tree through the shared semantic contract.",
    },
    DocumentMember {
        signature: "pub fn finish(self) -> Ir",
        summary: "Finishes the validated, column-oriented semantic image.",
    },
];

const ITEM_VIEW_MEMBERS: &[DocumentMember] = &[
    DocumentMember {
        signature: "pub fn name(self) -> &[u8]",
        summary: "Returns the canonical symbol spelling.",
    },
    DocumentMember {
        signature: "pub fn kind(self) -> ItemKind",
        summary: "Returns the language-neutral declaration kind.",
    },
    DocumentMember {
        signature: "pub fn docs(self) -> &[DocFragment]",
        summary: "Borrows the ordered documentation fragments.",
    },
    DocumentMember {
        signature: "pub fn links_from(self) -> LinkIter<'ir>",
        summary: "Walks typed outgoing semantic relationships.",
    },
];

const SERVICE_MEMBERS: &[DocumentMember] = &[
    DocumentMember {
        signature: "pub const fn with_compiler(compiler: Compiler) -> Self",
        summary: "Installs an explicitly owned compiler capability.",
    },
    DocumentMember {
        signature: "pub fn execute(&mut self, input: &ApplicationInput) -> ApplicationReply",
        summary: "Executes one typed transport-independent request.",
    },
    DocumentMember {
        signature: "pub fn poll_admitted_execution(&mut self, correlation: CorrelationId, operation: OperationKey, cx: &mut Context<'_>) -> Poll<ApplicationReply>",
        summary: "Drives an admitted effect from its registered wake.",
    },
];

const INPUT_VARIANTS: &[DocumentMember] = &[
    DocumentMember {
        signature: "Generate(GenerateRequest)",
        summary: "Compiles bounded source through an installed compiler.",
    },
    DocumentMember {
        signature: "Search { correlation, snapshot, query, limit }",
        summary: "Searches locally available immutable snapshot facts.",
    },
    DocumentMember {
        signature: "Graph { correlation, snapshot, limit }",
        summary: "Requests graph-neighborhood results.",
    },
    DocumentMember {
        signature: "Vector { correlation, snapshot, limit }",
        summary: "Requests vector-similarity results.",
    },
    DocumentMember {
        signature: "Health { correlation }",
        summary: "Reports honest local and unavailable capability facts.",
    },
];

const REPLY_VARIANTS: &[DocumentMember] = &[
    DocumentMember {
        signature: "Generated(GeneratedArtifact)",
        summary: "A compiler lowered and durably published a compact IR artifact.",
    },
    DocumentMember {
        signature: "DependencyUnavailable { capability: Capability }",
        summary: "A lower-plane capability is explicitly absent.",
    },
    DocumentMember {
        signature: "Health([CapabilityHealth; 6])",
        summary: "A fixed complete capability snapshot.",
    },
    DocumentMember {
        signature: "ExecutionStarted { operation, transition }",
        summary: "A bounded operation has one service-owned future.",
    },
];

const EMPTY_MEMBERS: &[DocumentMember] = &[];

/// First-party packages available in the documentation browser.
pub const DOCUMENT_PACKAGES: [DocumentPackage; PACKAGE_COUNT] = [
    DocumentPackage {
        name: "compiler-ir",
        version: "0.1.0",
        summary: "Canonical cross-language semantic IR, rendering, graph, and VCS facts.",
        first_item: 0,
        item_count: 6,
        tags: &["no_std", "semantic", "zero-copy"],
    },
    DocumentPackage {
        name: "interface-core",
        version: "0.1.0",
        summary: "Typed application requests, replies, diagnostics, and capability ownership.",
        first_item: 6,
        item_count: 5,
        tags: &["service", "typed", "bounded"],
    },
    DocumentPackage {
        name: "server-index-core",
        version: "0.1.0",
        summary: "Immutable exact and lexical indexes over globally addressable entities.",
        first_item: 11,
        item_count: 4,
        tags: &["index", "lexical", "immutable"],
    },
    DocumentPackage {
        name: "server-index-trustfall",
        version: "0.1.0",
        summary: "Synchronous typed graph traversal over validated semantic views.",
        first_item: 15,
        item_count: 2,
        tags: &["graph", "trustfall", "query"],
    },
    DocumentPackage {
        name: "heart-adaptive",
        version: "0.1.0",
        summary: "Pure local-first placement policy and finite capability execution.",
        first_item: 17,
        item_count: 3,
        tags: &["policy", "local-first", "recovery"],
    },
];

/// Complete built-in documentation catalog.
pub const DOCUMENT_ITEMS: [DocumentItem; 20] = [
    DocumentItem {
        package: 0,
        name: "Ir",
        path: "compiler_ir::Ir",
        kind: DocumentKind::Struct,
        signature: "pub struct Ir { /* private fields */ }",
        summary: "Validated, column-oriented semantic image shared by every language frontend.",
        paragraphs: &[
            "Ir is the central read model of the compiler plane. It stores declarations, semantic types, documentation fragments, source spans, language extensions, and typed graph links behind dense identifiers.",
            "The reader APIs borrow directly from the image. Signatures, documentation, and embedding text are formatting adapters, so callers can stream into a formatter without allocating a second representation.",
        ],
        example: Some(
            "let ir = builder.finish();\nfor id in ir.items() {\n    println!(\"{}\", ir.signature(id).unwrap());\n}",
        ),
        members: IR_MEMBERS,
        related: &[
            "compiler_ir::IrBuilder",
            "compiler_ir::ItemView",
            "compiler_ir::EmbeddingProfile",
        ],
        source: "compiler/ir/semantic.rs:3871",
        featured: true,
    },
    DocumentItem {
        package: 0,
        name: "IrBuilder",
        path: "compiler_ir::IrBuilder",
        kind: DocumentKind::Struct,
        signature: "pub struct IrBuilder { /* interners and semantic columns */ }",
        summary: "Builds one validated semantic image from language frontend facts.",
        paragraphs: &[
            "IrBuilder owns interning and admission for the shared semantic vocabulary. Frontends contribute borrowed trees; the builder validates identifiers, semantic types, documentation, links, and language-specific extension facts before they become visible.",
            "Use one builder for a coherent image. Failed admission retains a typed BuildError and never publishes a partial tree.",
        ],
        example: Some(
            "let mut builder = IrBuilder::new();\nlet entities = builder.add_frontend_tree(&tree)?;\nlet ir = builder.finish();",
        ),
        members: BUILDER_MEMBERS,
        related: &[
            "compiler_ir::FrontendTree",
            "compiler_ir::BuildError",
            "compiler_ir::Ir",
        ],
        source: "compiler/ir/semantic.rs:2528",
        featured: true,
    },
    DocumentItem {
        package: 0,
        name: "ItemView",
        path: "compiler_ir::ItemView",
        kind: DocumentKind::Struct,
        signature: "pub struct ItemView<'ir> { /* borrowed semantic row */ }",
        summary: "Borrowed public view of one declaration and all of its semantic lanes.",
        paragraphs: &[
            "ItemView keeps declaration identity attached to every borrowed fact. It exposes kind, visibility, semantic type, documentation, source span, version facts, and outgoing links without allowing those lanes to drift apart.",
        ],
        example: None,
        members: ITEM_VIEW_MEMBERS,
        related: &[
            "compiler_ir::ItemKind",
            "compiler_ir::DocFragment",
            "compiler_ir::Link",
        ],
        source: "compiler/ir/semantic.rs:4137",
        featured: true,
    },
    DocumentItem {
        package: 0,
        name: "SignatureDisplay",
        path: "compiler_ir::SignatureDisplay",
        kind: DocumentKind::Struct,
        signature: "pub struct SignatureDisplay<'ir> { /* formatter view */ }",
        summary: "Renders declarations in familiar docs.rs-style source syntax.",
        paragraphs: &[
            "SignatureDisplay implements Display over the condensed semantic IR. It renders language-neutral declaration kinds and recursively formats concrete, computed, literal, reference, tuple, object, and function types.",
            "Type recursion is bounded, dangling facts remain visible, and callers choose whether to stream directly or collect with ToString.",
        ],
        example: Some(
            "let signature = ir.signature(entity).ok_or(NotFound)?;\nwrite!(output, \"{signature}\")?;",
        ),
        members: EMPTY_MEMBERS,
        related: &["compiler_ir::TypeDisplay", "compiler_ir::DocsDisplay"],
        source: "compiler/ir/render.rs:18",
        featured: true,
    },
    DocumentItem {
        package: 0,
        name: "DocsDisplay",
        path: "compiler_ir::DocsDisplay",
        kind: DocumentKind::Struct,
        signature: "pub struct DocsDisplay<'ir> { /* formatter view */ }",
        summary: "Streams interned documentation as portable Markdown.",
        paragraphs: &[
            "DocsDisplay preserves text, inline code, local links, foreign ecosystem links, soft breaks, and paragraph breaks from the semantic documentation lane.",
            "Because it borrows the IR and implements Display, the same trusted representation can feed this GUI, static export, protocol adapters, and embedding tokenizers.",
        ],
        example: Some(
            "if let Some(docs) = ir.display_docs(entity) {\n    write!(markdown, \"{docs}\")?;\n}",
        ),
        members: EMPTY_MEMBERS,
        related: &["compiler_ir::DocFragment", "compiler_ir::LinkTarget"],
        source: "compiler/ir/render.rs:28",
        featured: false,
    },
    DocumentItem {
        package: 0,
        name: "EmbeddingProfile",
        path: "compiler_ir::EmbeddingProfile",
        kind: DocumentKind::Struct,
        signature: "pub struct EmbeddingProfile(u8);",
        summary: "Selects signature, documentation, and graph lanes for semantic tokenization.",
        paragraphs: &[
            "The profile makes semantic-search input explicit. SYMBOL includes the declaration signature, DOCUMENTED adds docs, and CONTEXTUAL also adds typed outgoing graph relationships.",
        ],
        example: Some("let text = ir.embedding_text(entity, EmbeddingProfile::CONTEXTUAL);"),
        members: &[
            DocumentMember {
                signature: "pub const SYMBOL: Self",
                summary: "Signature only.",
            },
            DocumentMember {
                signature: "pub const DOCUMENTED: Self",
                summary: "Signature plus documentation.",
            },
            DocumentMember {
                signature: "pub const CONTEXTUAL: Self",
                summary: "Signature, documentation, and graph context.",
            },
        ],
        related: &["compiler_ir::EmbeddingDisplay"],
        source: "compiler/ir/render.rs:34",
        featured: false,
    },
    DocumentItem {
        package: 1,
        name: "ApplicationService",
        path: "interface_core::ApplicationService",
        kind: DocumentKind::Struct,
        signature: "pub struct ApplicationService<Compiler = UnavailableCompiler> { /* owned capabilities */ }",
        summary: "The transport-independent owner of validation, compilation, health, and finite execution.",
        paragraphs: &[
            "ApplicationService is the single business boundary shared by direct callers, CLI, MCP, protocol, and GPUI. Every request is a closed ApplicationInput and every result is one source-preserving ApplicationReply.",
            "The service is generic over its compiler capability, owns at most one adaptive execution, and never reports success for an absent index, graph, vector, or remote provider.",
        ],
        example: Some(
            "let mut service = ApplicationService::new();\nlet reply = service.execute(&ApplicationInput::Health {\n    correlation: CorrelationId(1),\n});",
        ),
        members: SERVICE_MEMBERS,
        related: &[
            "interface_core::ApplicationInput",
            "interface_core::ApplicationReply",
            "interface_core::CompilerCapability",
        ],
        source: "interface/core/service.rs:34",
        featured: true,
    },
    DocumentItem {
        package: 1,
        name: "ApplicationInput",
        path: "interface_core::ApplicationInput",
        kind: DocumentKind::Enum,
        signature: "pub enum ApplicationInput { Generate, Search, Graph, Vector, Health, /* … */ }",
        summary: "Closed typed command vocabulary accepted by the application service.",
        paragraphs: &[
            "Every input carries an unchanged correlation identity and bounded typed operands. There is no string command decoder inside the service, which keeps the CLI, MCP, protocol, and GUI behavior aligned.",
        ],
        example: None,
        members: INPUT_VARIANTS,
        related: &[
            "interface_core::ApplicationService",
            "interface_core::CorrelationId",
        ],
        source: "interface/core/model.rs:65",
        featured: true,
    },
    DocumentItem {
        package: 1,
        name: "ApplicationReply",
        path: "interface_core::ApplicationReply",
        kind: DocumentKind::Struct,
        signature: "pub struct ApplicationReply { pub correlation: CorrelationId, pub outcome: ApplicationOutcome }",
        summary: "A correlated result with mutually exclusive semantic body or diagnostic.",
        paragraphs: &[
            "ApplicationReply retains exact structured facts all the way to presentation. A resolved reply owns a ReplyBody; a failed reply owns one Diagnostic. The two cannot be accidentally combined.",
        ],
        example: None,
        members: &[
            DocumentMember {
                signature: "pub correlation: CorrelationId",
                summary: "The caller identity copied unchanged.",
            },
            DocumentMember {
                signature: "pub outcome: ApplicationOutcome",
                summary: "Resolved semantic facts or one exact failure.",
            },
        ],
        related: &[
            "interface_core::ApplicationOutcome",
            "interface_core::ReplyBody",
            "interface_core::Diagnostic",
        ],
        source: "interface/core/model.rs:445",
        featured: false,
    },
    DocumentItem {
        package: 1,
        name: "ReplyBody",
        path: "interface_core::ReplyBody",
        kind: DocumentKind::Enum,
        signature: "pub enum ReplyBody { Generated, DependencyUnavailable, Health, Adaptive, ExecutionStarted, Execution }",
        summary: "Semantic service result vocabulary used by every interface adapter.",
        paragraphs: &[
            "ReplyBody carries real authority and capability facts. Its ApplicationDisposition is derived exclusively from the variant, preventing presentation adapters from inventing completion or coverage.",
        ],
        example: None,
        members: REPLY_VARIANTS,
        related: &[
            "interface_core::ApplicationDisposition",
            "interface_core::GeneratedArtifact",
        ],
        source: "interface/core/model.rs:408",
        featured: false,
    },
    DocumentItem {
        package: 1,
        name: "CompilerCapability",
        path: "interface_core::CompilerCapability",
        kind: DocumentKind::Trait,
        signature: "pub trait CompilerCapability { fn readiness(&self) -> CompilerReadiness; fn generate(&mut self, request: CompilerRequest<'_>) -> Result<GeneratedArtifact, CompilerTerminal>; }",
        summary: "Lean monomorphized seam for local compilation and durable publication.",
        paragraphs: &[
            "The trait lets hosts install a concrete compiler without dynamic dispatch or forcing heavy compiler dependencies into portable application adapters. Terminals retain source, recipe, and publication authority.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "interface_core::UnavailableCompiler",
            "interface_core::GeneratedArtifact",
        ],
        source: "interface/core/compiler.rs:94",
        featured: false,
    },
    DocumentItem {
        package: 2,
        name: "EntityDocumentId",
        path: "server_index_core::EntityDocumentId",
        kind: DocumentKind::Struct,
        signature: "pub struct EntityDocumentId { pub fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>, pub entity: EntityId }",
        summary: "Collision-free global address of a declaration inside an immutable IR fragment.",
        paragraphs: &[
            "Exact and lexical index planes share EntityDocumentId. The full content-addressed fragment identity plus canonical entity ordinal avoids lossy hash salts and deliberately changes when the underlying fragment changes.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "server_index_core::ExactRow",
            "server_index_core::LexicalRow",
        ],
        source: "server/index/core/document.rs:22",
        featured: true,
    },
    DocumentItem {
        package: 2,
        name: "IndexSnapshot",
        path: "server_index_core::IndexSnapshot",
        kind: DocumentKind::Struct,
        signature: "pub struct IndexSnapshot<'selection>(IndexSnapshotView<'selection>);",
        summary: "Validated immutable selection of exact and lexical segment authorities.",
        paragraphs: &[
            "IndexSnapshot binds a generation to ordered exact and lexical segment identities. Construction validates canonical ordering, duplicate exclusion, and generation authority before retrieval can borrow the view.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "server_index_core::ExactManifest",
            "server_index_core::LexicalManifest",
        ],
        source: "server/index/core/snapshot.rs:55",
        featured: true,
    },
    DocumentItem {
        package: 2,
        name: "ExactOperation",
        path: "server_index_core::ExactOperation",
        kind: DocumentKind::Struct,
        signature: "pub struct ExactOperation<'query> { /* bounded query and output */ }",
        summary: "Bounded exact lookup over verified immutable segments.",
        paragraphs: &[
            "Exact retrieval walks verified segment rows and returns typed degradation when coverage is incomplete. Output capacity and query ownership are explicit at construction.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "server_index_core::ExactResolution",
            "server_index_core::ExactTerminal",
        ],
        source: "server/index/core/exact.rs:101",
        featured: false,
    },
    DocumentItem {
        package: 2,
        name: "LexicalOperation",
        path: "server_index_core::LexicalOperation",
        kind: DocumentKind::Struct,
        signature: "pub struct LexicalOperation<'query> { /* query, top-k, cancellation */ }",
        summary: "Deterministic top-k lexical retrieval across snapshot segments.",
        paragraphs: &[
            "LexicalOperation retains the query and requested top-k bound. Results carry their immutable document identity and score, while partial coverage remains a typed terminal rather than disappearing.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "server_index_core::LexicalHit",
            "server_index_core::LexicalScore",
        ],
        source: "server/index/core/lexical.rs:232",
        featured: false,
    },
    DocumentItem {
        package: 3,
        name: "IrTrustfallGraph",
        path: "server_index_trustfall::IrTrustfallGraph",
        kind: DocumentKind::Struct,
        signature: "pub struct IrTrustfallGraph<'ir> { /* borrowed semantic IR */ }",
        summary: "Trustfall adapter that traverses the compiler IR directly.",
        paragraphs: &[
            "IrTrustfallGraph exposes typed semantic links to a fixed GraphQL query without copying the IR into an intermediate graph store. Output field validation and bounded cardinality are retained as structured errors.",
        ],
        example: Some(
            "let graph = IrTrustfallGraph::new(&ir);\nlet written = graph.neighbors(entity, &mut output)?;",
        ),
        members: EMPTY_MEMBERS,
        related: &["server_index_trustfall::IrTrustfallHit", "compiler_ir::Ir"],
        source: "server/index/trustfall/graph.rs:68",
        featured: true,
    },
    DocumentItem {
        package: 3,
        name: "TrustfallGraph",
        path: "server_index_trustfall::TrustfallGraph",
        kind: DocumentKind::Struct,
        signature: "pub struct TrustfallGraph<'view> { /* validated graph view */ }",
        summary: "Synchronous graph query surface over a pinned validated projection.",
        paragraphs: &[
            "The graph keeps snapshot and recipe authority attached to every hit. A parsed fixed schema and indexed query are reused, and malformed upstream output becomes a closed TrustfallGraphError.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "server_index_trustfall::TrustfallHit",
            "server_index_graph_vector::ValidatedGraphView",
        ],
        source: "server/index/trustfall/graph.rs:257",
        featured: false,
    },
    DocumentItem {
        package: 4,
        name: "PolicyInput",
        path: "heart_adaptive::PolicyInput",
        kind: DocumentKind::Struct,
        signature: "pub struct PolicyInput<'facts> { pub pin: Pin, pub local: &'facts [LocalFact], pub remote: &'facts [RemoteFact], /* … */ }",
        summary: "Complete immutable fact snapshot consumed by the pure placement policy.",
        paragraphs: &[
            "PolicyInput names one pinned generation and snapshot, all local and remote facts, demand, bundle residence, remote health, and exact resource credits. The policy performs no I/O and has no ambient lookup.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &[
            "heart_adaptive::PolicyDecision",
            "heart_adaptive::ResourceBudget",
        ],
        source: "heart/adaptive/lib.rs:235",
        featured: true,
    },
    DocumentItem {
        package: 4,
        name: "next_action",
        path: "heart_adaptive::next_action",
        kind: DocumentKind::Function,
        signature: "pub fn next_action(input: &PolicyInput<'_>) -> Result<PolicyDecision, PolicyError>",
        summary: "Selects the next safe local-first placement action from one fact snapshot.",
        paragraphs: &[
            "next_action is deterministic and effect-free. It validates the fact set, respects operation and retry credits, prefers available local capabilities, and returns a typed decision for an external executor to own.",
        ],
        example: Some(
            "match next_action(&facts)? {\n    PolicyDecision::Act(action) => executor.submit(action),\n    PolicyDecision::Stable => {}\n}",
        ),
        members: EMPTY_MEMBERS,
        related: &[
            "heart_adaptive::PolicyInput",
            "heart_adaptive::PlacementAction",
        ],
        source: "heart/adaptive/lib.rs",
        featured: true,
    },
    DocumentItem {
        package: 4,
        name: "ResourceBudget",
        path: "heart_adaptive::ResourceBudget",
        kind: DocumentKind::Struct,
        signature: "pub struct ResourceBudget { pub ram_free: ByteCount, pub nvme_free: ByteCount, pub operations: OperationBudget, /* … */ }",
        summary: "Exact physical, action, pressure, battery, and retry credits for one policy decision.",
        paragraphs: &[
            "ResourceBudget prevents placement from treating capacity as an unbounded boolean. Each decision observes immutable memory, storage, pressure, battery, operation, and retry facts.",
        ],
        example: None,
        members: EMPTY_MEMBERS,
        related: &["heart_adaptive::Pressure", "heart_adaptive::BatteryState"],
        source: "heart/adaptive/lib.rs:217",
        featured: false,
    },
];

#[cfg(test)]
mod tests {
    use super::{DOCUMENT_ITEMS, DocumentFilter, DocumentationState};
    use interface_core::InputText;

    #[test]
    fn ranked_search_prefers_an_exact_name_and_respects_kind_filters() {
        let mut state = DocumentationState::default();
        state.replace_query(InputText::try_from_str("Ir").expect("bounded query"));
        let hits = state.search_hits();
        assert_eq!(DOCUMENT_ITEMS[hits[0].item].name, "Ir");

        state.select_filter(DocumentFilter::APIs);
        assert!(state.search_hits().iter().all(|hit| matches!(
            DOCUMENT_ITEMS[hit.item].kind,
            super::DocumentKind::Trait | super::DocumentKind::Function
        )));
    }

    #[test]
    fn selecting_an_item_discloses_its_package() {
        let mut state = DocumentationState::default();
        state.select_item(16);
        assert_eq!(state.selected_item, 16);
        assert!(state.expanded_packages[3]);
    }
}
