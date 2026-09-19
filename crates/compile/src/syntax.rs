//! Shared, content-versioned syntax extraction for local language frontends.

use crate::{
    InputContentSchema, InputContentVersion, SyntaxProducerId, SyntaxProducerSchema, typed_of,
};
use std::{collections::BTreeSet, fmt, num::NonZeroU32, path::Path, sync::Arc};
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

pub(crate) use crate::syntax_kind::declaration_kind;

const MAX_DECLARATIONS: usize = 16_384;
const MAX_TEXT_BYTES: usize = 4_096;

/// Source-language family understood by the shared syntax frontend.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SourceLanguage {
    /// Rust source.
    Rust = 0,
    /// TypeScript or JavaScript source.
    TypeScript = 1,
    /// Python source.
    Python = 2,
    /// Go source.
    Go = 3,
    /// Java source.
    Java = 4,
    /// C# source.
    CSharp = 5,
    /// C-family source parsed through Clang.
    Clang = 6,
}

impl SourceLanguage {
    /// Every supported source-language family in canonical order.
    pub const ALL: [Self; 7] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::Clang,
    ];

    /// Returns the stable lowercase language name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Clang => "clang",
        }
    }

    /// Returns the stable one-byte wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        self as u8
    }

    /// Admits a stable one-byte wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Rust),
            1 => Some(Self::TypeScript),
            2 => Some(Self::Python),
            3 => Some(Self::Go),
            4 => Some(Self::Java),
            5 => Some(Self::CSharp),
            6 => Some(Self::Clang),
            _ => None,
        }
    }
}

/// Closed semantic declaration vocabulary shared by every source frontend.
///
/// Tree-sitter tags intentionally use a small set of language-independent
/// names.  Keeping that vocabulary typed at the compile boundary means a
/// view, wire adapter, or UI can group declarations without re-parsing
/// frontend strings.  The extra variants cover richer frontends while
/// `Unknown` keeps an older reader able to retain a bounded future tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum DeclarationKind {
    /// A source file or namespace/module declaration.
    Module = 1,
    /// A class declaration.
    Class = 2,
    /// A callable free function.
    Function = 3,
    /// A callable member declaration.
    Method = 4,
    /// An interface or protocol declaration.
    Interface = 5,
    /// A type declaration or alias.
    Type = 6,
    /// A macro or preprocessor declaration.
    Macro = 7,
    /// A compile-time constant.
    Constant = 8,
    /// A field/member data declaration.
    Field = 9,
    /// A property/accessor declaration.
    Property = 10,
    /// A constructor declaration.
    Constructor = 11,
    /// An enum declaration.
    Enum = 12,
    /// A struct declaration.
    Struct = 13,
    /// A trait declaration.
    Trait = 14,
    /// A union declaration.
    Union = 15,
    /// A variable declaration.
    Variable = 16,
    /// An import/use declaration.
    Import = 17,
    /// A bounded future or frontend-specific tag not yet understood here.
    Unknown = 255,
}

impl DeclarationKind {
    /// Returns the stable lowercase name used by products and JSON adapters.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Macro => "macro",
            Self::Constant => "constant",
            Self::Field => "field",
            Self::Property => "property",
            Self::Constructor => "constructor",
            Self::Enum => "enum",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Union => "union",
            Self::Variable => "variable",
            Self::Import => "import",
            Self::Unknown => "unknown",
        }
    }

    /// Returns the stable canonical wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        self as u8
    }

    /// Admits a canonical wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Module),
            2 => Some(Self::Class),
            3 => Some(Self::Function),
            4 => Some(Self::Method),
            5 => Some(Self::Interface),
            6 => Some(Self::Type),
            7 => Some(Self::Macro),
            8 => Some(Self::Constant),
            9 => Some(Self::Field),
            10 => Some(Self::Property),
            11 => Some(Self::Constructor),
            12 => Some(Self::Enum),
            13 => Some(Self::Struct),
            14 => Some(Self::Trait),
            15 => Some(Self::Union),
            16 => Some(Self::Variable),
            17 => Some(Self::Import),
            255 => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Normalizes one frontend tag into the closed vocabulary.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.trim() {
            "module" | "namespace" => Self::Module,
            "class" => Self::Class,
            "function" | "func" => Self::Function,
            "method" => Self::Method,
            "interface" | "protocol" => Self::Interface,
            "type" | "type_alias" | "type-alias" => Self::Type,
            "macro" | "preprocessor" => Self::Macro,
            "constant" | "const" => Self::Constant,
            "field" => Self::Field,
            "property" => Self::Property,
            "constructor" => Self::Constructor,
            "enum" => Self::Enum,
            "struct" => Self::Struct,
            "trait" => Self::Trait,
            "union" => Self::Union,
            "variable" | "var" => Self::Variable,
            "import" | "use" => Self::Import,
            _ => Self::Unknown,
        }
    }
}

impl From<&str> for DeclarationKind {
    fn from(value: &str) -> Self {
        Self::from_name(value)
    }
}

impl From<String> for DeclarationKind {
    fn from(value: String) -> Self {
        Self::from_name(&value)
    }
}

/// Exact source location retained with a declaration and view row.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceLocation {
    path: Arc<str>,
    start_line: NonZeroU32,
}

/// Completeness of a bounded declaration source excerpt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceExcerptExtent {
    /// The complete declaration text fits in the retained bound.
    Complete,
    /// The declaration text continues beyond the retained UTF-8 prefix.
    Truncated,
}

/// Explicit availability of bounded declaration source text.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceExcerpt {
    /// Source text was retained with an explicit completeness result.
    Captured {
        /// Bounded UTF-8 declaration text.
        text: Arc<str>,
        /// Whether the retained text is complete.
        extent: SourceExcerptExtent,
    },
    /// The producer did not capture declaration source text.
    NotCaptured,
    /// Source text exists but is not resident in this process.
    NotHydrated,
    /// This deployment has no source-text provider.
    Unconfigured,
}

impl SourceExcerpt {
    /// Maximum source bytes retained for one declaration.
    pub const MAX_BYTES: usize = 4_096;

    /// Admits already-bounded captured source text.
    ///
    /// # Errors
    /// Returns an error when the text is empty or exceeds [`Self::MAX_BYTES`].
    pub fn captured(text: &str, extent: SourceExcerptExtent) -> Result<Self, String> {
        if text.is_empty() || text.len() > Self::MAX_BYTES {
            return Err("source excerpt is empty or oversized".to_owned());
        }
        Ok(Self::Captured {
            text: Arc::from(text),
            extent,
        })
    }

    /// Captures a bounded UTF-8 prefix without allocating the unbounded input.
    #[must_use]
    pub fn capture_bounded(source: &str) -> Self {
        if source.is_empty() {
            return Self::NotCaptured;
        }
        let end = bounded_excerpt_end(source);
        Self::Captured {
            text: Arc::from(&source[..end]),
            extent: if end == source.len() {
                SourceExcerptExtent::Complete
            } else {
                SourceExcerptExtent::Truncated
            },
        }
    }

    /// Returns retained text when captured.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Captured { text, .. } => Some(text),
            Self::NotCaptured | Self::NotHydrated | Self::Unconfigured => None,
        }
    }

    /// Returns completeness when source text was captured.
    #[must_use]
    pub const fn extent(&self) -> Option<SourceExcerptExtent> {
        match self {
            Self::Captured { extent, .. } => Some(*extent),
            Self::NotCaptured | Self::NotHydrated | Self::Unconfigured => None,
        }
    }
}

fn bounded_excerpt_end(source: &str) -> usize {
    let mut end = source.len().min(SourceExcerpt::MAX_BYTES);
    while !source.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    end
}

impl SourceLocation {
    /// Maximum admitted source path bytes.
    pub const MAX_PATH_BYTES: usize = 4_096;

    /// Creates a checked one-based source location.
    ///
    /// # Errors
    /// Returns an error for an empty/oversized path or a zero line.
    pub fn new(path: impl Into<String>, start_line: u32) -> Result<Self, String> {
        let path = path.into();
        let Some(start_line) = NonZeroU32::new(start_line) else {
            return Err("source location line must be one-based".to_owned());
        };
        if path.is_empty() || path.len() > Self::MAX_PATH_BYTES {
            return Err("source location path is empty or oversized".to_owned());
        }
        Ok(Self {
            path: Arc::from(path),
            start_line,
        })
    }

    /// Creates a bounded location for callers that do not own a path yet.
    ///
    /// This compatibility constructor is only used by the legacy-shaped
    /// declaration constructor; all parser output supplies its real path.
    #[must_use]
    pub fn unknown(start_line: u32) -> Self {
        Self {
            path: Arc::from("<unknown>"),
            start_line: NonZeroU32::new(start_line).unwrap_or(NonZeroU32::MIN),
        }
    }

    /// Returns the canonical source path or coordinate.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the one-based declaration start line.
    #[must_use]
    pub const fn start_line(&self) -> u32 {
        self.start_line.get()
    }
}

/// One bounded declaration selected by a grammar's semantic tags query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDeclaration {
    name: String,
    kind: DeclarationKind,
    location: SourceLocation,
    signature: String,
    documentation: String,
    source_excerpt: SourceExcerpt,
}

impl SourceDeclaration {
    /// Maximum bytes in each retained text field.
    pub const MAX_TEXT_BYTES: usize = MAX_TEXT_BYTES;

    /// Constructs a checked declaration.
    ///
    /// # Errors
    /// Returns an error when a required field is empty or any text field is oversized.
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<DeclarationKind>,
        line: u32,
        signature: impl Into<String>,
        documentation: impl Into<String>,
    ) -> Result<Self, String> {
        Self::with_location(
            SourceLocation::unknown(line),
            name,
            kind,
            signature,
            documentation,
        )
    }

    /// Constructs a checked declaration at an exact source path and line.
    ///
    /// # Errors
    /// Returns an error when the location or any retained text is invalid or
    /// oversized.
    pub fn at_path(
        path: impl Into<String>,
        name: impl Into<String>,
        kind: impl Into<DeclarationKind>,
        line: u32,
        signature: impl Into<String>,
        documentation: impl Into<String>,
    ) -> Result<Self, String> {
        Self::with_location(
            SourceLocation::new(path, line)?,
            name,
            kind,
            signature,
            documentation,
        )
    }

    /// Constructs a checked declaration from a prevalidated source location.
    ///
    /// # Errors
    /// Returns an error when a required field is empty or any text field is
    /// oversized.
    pub fn with_location(
        location: SourceLocation,
        name: impl Into<String>,
        kind: impl Into<DeclarationKind>,
        signature: impl Into<String>,
        documentation: impl Into<String>,
    ) -> Result<Self, String> {
        let value = Self {
            name: name.into(),
            kind: kind.into(),
            location,
            signature: signature.into(),
            documentation: documentation.into(),
            source_excerpt: SourceExcerpt::NotCaptured,
        };
        if value.name.is_empty()
            || [
                value.name.len(),
                value.kind.name().len(),
                value.signature.len(),
                value.documentation.len(),
            ]
            .into_iter()
            .any(|length| length > Self::MAX_TEXT_BYTES)
        {
            return Err("source declaration is empty or oversized".to_owned());
        }
        Ok(value)
    }

    /// Returns the declared name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the language-independent semantic tag kind.
    #[must_use]
    pub const fn kind(&self) -> DeclarationKind {
        self.kind
    }

    /// Returns the stable display name for this declaration kind.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        self.kind.name()
    }

    /// Returns the exact source location.
    #[must_use]
    pub const fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns the one-based source line.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.location.start_line()
    }

    /// Attaches bounded source text retained by the parser.
    #[must_use]
    pub fn with_source_excerpt(mut self, source_excerpt: SourceExcerpt) -> Self {
        self.source_excerpt = source_excerpt;
        self
    }

    /// Returns explicit source-text availability and completeness.
    #[must_use]
    pub const fn source_excerpt(&self) -> &SourceExcerpt {
        &self.source_excerpt
    }

    /// Returns the compact source signature.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Returns the contiguous documentation immediately preceding the declaration.
    #[must_use]
    pub fn documentation(&self) -> &str {
        &self.documentation
    }
}

/// An admitted source analysis bound to both input bytes and producer contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnalysis {
    language: SourceLanguage,
    content: InputContentVersion,
    producer: SyntaxProducerId,
    declarations: Arc<[SourceDeclaration]>,
}

impl SourceAnalysis {
    /// Returns the selected language.
    #[must_use]
    pub const fn language(&self) -> SourceLanguage {
        self.language
    }

    /// Returns the typed identity of the exact source bytes.
    #[must_use]
    pub const fn content(&self) -> InputContentVersion {
        self.content
    }

    /// Returns the parser, grammar, and extraction-contract identity.
    #[must_use]
    pub const fn producer(&self) -> SyntaxProducerId {
        self.producer
    }

    /// Returns declarations in stable source order without copying them.
    #[must_use]
    pub fn declarations(&self) -> &Arc<[SourceDeclaration]> {
        &self.declarations
    }
}

/// One compiled grammar choice for a set of filename extensions.
pub struct GrammarVariant {
    extensions: &'static [&'static str],
    language: Language,
    tags: Query,
}

impl GrammarVariant {
    /// Compiles one grammar's canonical tags query once.
    ///
    /// # Errors
    /// Returns [`SyntaxError::Query`] when the grammar and query disagree.
    pub fn new(
        extensions: &'static [&'static str],
        language: Language,
        tags: &str,
    ) -> Result<Self, SyntaxError> {
        let query =
            Query::new(&language, tags).map_err(|error| SyntaxError::Query(error.to_string()))?;
        Ok(Self {
            extensions,
            language,
            tags: query,
        })
    }

    fn supports(&self, extension: &str) -> bool {
        self.extensions.contains(&extension)
    }
}

/// A reusable local frontend: one typed producer and one or more compiled grammars.
pub struct SyntaxFrontend {
    language: SourceLanguage,
    producer: SyntaxProducerId,
    variants: Box<[GrammarVariant]>,
}

impl SyntaxFrontend {
    /// Maximum aggregate source-excerpt bytes retained by one analysis.
    pub const MAX_EXCERPT_BYTES: usize = 1024 * 1024;

    /// Creates a frontend from an explicit, bump-on-change contract and grammar variants.
    ///
    /// # Errors
    /// Returns [`SyntaxError::NoGrammar`] when no variants were provided.
    pub fn new(
        language: SourceLanguage,
        contract: &[u8],
        variants: Vec<GrammarVariant>,
    ) -> Result<Self, SyntaxError> {
        if variants.is_empty() {
            return Err(SyntaxError::NoGrammar(language));
        }
        let mut identity = Vec::with_capacity(language.name().len() + contract.len() + 1);
        identity.extend_from_slice(language.name().as_bytes());
        identity.push(0);
        identity.extend_from_slice(contract);
        Ok(Self {
            language,
            producer: typed_of::<SyntaxProducerSchema>(&identity),
            variants: variants.into_boxed_slice(),
        })
    }

    /// Returns the frontend language.
    #[must_use]
    pub const fn language(&self) -> SourceLanguage {
        self.language
    }

    /// Returns the producer contract identity used for reuse fences.
    #[must_use]
    pub const fn producer(&self) -> SyntaxProducerId {
        self.producer
    }

    /// Returns whether this frontend owns a path's extension.
    #[must_use]
    pub fn supports_path(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                let extension = value.to_ascii_lowercase();
                self.variants
                    .iter()
                    .any(|variant| variant.supports(&extension))
            })
    }

    /// Parses exact source bytes and applies the grammar's semantic tags query.
    ///
    /// Each call owns its parser and cursor, so independent files execute in
    /// parallel without locks. Compiled queries and output declarations are
    /// shared across project versions.
    ///
    /// # Errors
    /// Returns a [`SyntaxError`] for invalid UTF-8, an unsupported extension,
    /// parser setup failure, or an exceeded declaration bound.
    pub fn analyze(&self, path: &Path, source: &[u8]) -> Result<SourceAnalysis, SyntaxError> {
        let text = std::str::from_utf8(source).map_err(|_| SyntaxError::NonUtf8)?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| SyntaxError::UnsupportedPath(path.to_path_buf()))?;
        let variant = self
            .variants
            .iter()
            .find(|variant| variant.supports(&extension))
            .ok_or_else(|| SyntaxError::UnsupportedPath(path.to_path_buf()))?;
        let mut parser = Parser::new();
        parser
            .set_language(&variant.language)
            .map_err(|error| SyntaxError::Parser(error.to_string()))?;
        let tree = parser.parse(source, None).ok_or(SyntaxError::Cancelled)?;
        if tree.root_node().has_error() {
            return Err(SyntaxError::MalformedSource(self.language));
        }
        let mut declarations = Vec::new();
        declarations.push(module_declaration(path, self.language)?);
        let names = variant.tags.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&variant.tags, tree.root_node(), source);
        let mut seen = BTreeSet::new();
        let mut excerpt_bytes = 0usize;
        while let Some(query_match) = matches.next() {
            let definition = query_match.captures().iter().find_map(|capture| {
                let capture_name = names.get(capture.index as usize)?;
                capture_name
                    .strip_prefix("definition.")
                    .map(|kind| (kind, capture.node))
            });
            let name = query_match.captures().iter().find_map(|capture| {
                (names.get(capture.index as usize).copied() == Some("name")).then_some(capture.node)
            });
            let (Some((kind, definition)), Some(name)) = (definition, name) else {
                continue;
            };
            let name = node_text(name, text).trim();
            let line = u32::try_from(definition.start_position().row.saturating_add(1))
                .unwrap_or(u32::MAX);
            if name.is_empty() || !seen.insert((line, kind, name.to_owned())) {
                continue;
            }
            let declaration_source = node_text(definition, text);
            excerpt_bytes = excerpt_bytes
                .checked_add(bounded_excerpt_end(declaration_source))
                .ok_or(SyntaxError::TooManySourceBytes)?;
            if excerpt_bytes > Self::MAX_EXCERPT_BYTES {
                return Err(SyntaxError::TooManySourceBytes);
            }
            declarations.push(
                SourceDeclaration::at_path(
                    path.display().to_string(),
                    bounded(name),
                    declaration_kind(self.language, kind, definition.kind()),
                    line,
                    declaration_signature(definition, text),
                    declaration_documentation(definition, text),
                )?
                .with_source_excerpt(SourceExcerpt::capture_bounded(declaration_source)),
            );
            if declarations.len() > MAX_DECLARATIONS {
                return Err(SyntaxError::TooManyDeclarations);
            }
        }
        declarations[1..].sort_by(|left, right| {
            (left.line(), left.kind(), left.name()).cmp(&(right.line(), right.kind(), right.name()))
        });
        Ok(SourceAnalysis {
            language: self.language,
            content: typed_of::<InputContentSchema>(source),
            producer: self.producer,
            declarations: Arc::from(declarations.into_boxed_slice()),
        })
    }
}

/// Exact local syntax extraction failures.
#[derive(Debug)]
pub enum SyntaxError {
    /// The source was not UTF-8.
    NonUtf8,
    /// No grammar variant owns this path.
    UnsupportedPath(std::path::PathBuf),
    /// A frontend was constructed without a grammar.
    NoGrammar(SourceLanguage),
    /// A grammar's semantic tags query was invalid.
    Query(String),
    /// The parser rejected a grammar ABI.
    Parser(String),
    /// Parsing was cancelled by the parser runtime.
    Cancelled,
    /// The grammar produced an error node, so declaration facts are unsafe to publish.
    MalformedSource(SourceLanguage),
    /// The source exceeded the bounded declaration budget.
    TooManyDeclarations,
    /// Retained source excerpts exceeded the aggregate analysis bound.
    TooManySourceBytes,
    /// A selected declaration violated the bounded source contract.
    Declaration(String),
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8 => formatter.write_str("source is not UTF-8"),
            Self::UnsupportedPath(path) => {
                write!(formatter, "unsupported source path {}", path.display())
            }
            Self::NoGrammar(language) => {
                write!(formatter, "{} frontend has no grammar", language.name())
            }
            Self::Query(error) => write!(formatter, "invalid semantic tags query: {error}"),
            Self::Parser(error) => write!(formatter, "parser setup failed: {error}"),
            Self::Cancelled => formatter.write_str("source parsing was cancelled"),
            Self::MalformedSource(language) => {
                write!(formatter, "{} source is malformed", language.name())
            }
            Self::TooManyDeclarations => formatter.write_str("source declaration set is oversized"),
            Self::TooManySourceBytes => formatter.write_str("source excerpt set is oversized"),
            Self::Declaration(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for SyntaxError {}

impl From<String> for SyntaxError {
    fn from(error: String) -> Self {
        Self::Declaration(error)
    }
}

fn module_declaration(
    path: &Path,
    language: SourceLanguage,
) -> Result<SourceDeclaration, SyntaxError> {
    let module = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("module");
    SourceDeclaration::at_path(
        path.display().to_string(),
        bounded(module),
        "module",
        1,
        format!("{} module {}", language.name(), bounded(module)),
        "",
    )
    .map_err(SyntaxError::Declaration)
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

fn signature_node(mut node: Node<'_>) -> Node<'_> {
    for _ in 0..2 {
        let Some(parent) = node.parent() else {
            break;
        };
        if !matches!(
            parent.kind(),
            "function_definition"
                | "declaration"
                | "export_statement"
                | "decorated_definition"
                | "template_declaration"
        ) {
            break;
        }
        node = parent;
    }
    node
}

fn declaration_signature(node: Node<'_>, source: &str) -> String {
    let text = node_text(signature_node(node), source).trim();
    let end = text
        .find('{')
        .or_else(|| text.find('\n'))
        .unwrap_or(text.len());
    bounded(text[..end].trim())
}

fn declaration_documentation(mut node: Node<'_>, source: &str) -> String {
    for _ in 0..3 {
        let mut comments = Vec::new();
        let mut previous = node.prev_named_sibling();
        while let Some(candidate) = previous {
            if candidate.kind() != "comment" {
                break;
            }
            comments.push(clean_comment(node_text(candidate, source)));
            previous = candidate.prev_named_sibling();
        }
        if !comments.is_empty() {
            comments.reverse();
            return bounded(&comments.join("\n"));
        }
        let Some(parent) = node.parent() else {
            break;
        };
        if !matches!(
            parent.kind(),
            "function_definition"
                | "declaration"
                | "export_statement"
                | "decorated_definition"
                | "template_declaration"
        ) {
            break;
        }
        node = parent;
    }
    String::new()
}

fn clean_comment(comment: &str) -> String {
    comment
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches('/')
                .trim_start_matches('!')
                .trim_start_matches('*')
                .trim_start_matches('#')
                .trim_end_matches("*/")
                .trim()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_TEXT_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
