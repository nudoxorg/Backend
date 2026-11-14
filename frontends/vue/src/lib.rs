//! Bounded native Vue SFC source regions. This frontend does not publish facts.
//!
//! Verter's Vue-derived tokenizer and syntax parser operate on original UTF-8
//! bytes. We snapshot finalized roots on each parser event because its final
//! descriptor replaces duplicate scripts and templates. The original host is
//! the only source authority; no generated or concatenated text is indexed.
use backend_compile::{InputContentVersion, VerifiedSourceExtent, VerifiedSourceText};
use backend_version::{ContentId, SourceFactDomain};
use verter_core::diagnostics::{Diagnostic, SyntaxPluginContext, SyntaxPluginOptions};
use verter_core::parser::Syntax;
use verter_core::tokenizer::{Event, byte::tokenize_sfc};
use verter_core::types::{NodeProp, NodeTag};

/// One parser-observed root attribute, with offsets into the exact host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    /// Attribute name bytes.
    pub name: std::ops::Range<usize>,
    /// Raw value bytes, excluding quotes, if present.
    pub value: Option<std::ops::Range<usize>>,
    /// Whether this is a Vue directive.
    pub directive: bool,
}
/// A root block and its original host-byte spans.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    /// Block tag name.
    pub name: std::ops::Range<usize>,
    /// Opening tag, including delimiters.
    pub open: std::ops::Range<usize>,
    /// Verbatim content between tags, when present.
    pub content: Option<std::ops::Range<usize>>,
    /// Closing tag, when present.
    pub close: Option<std::ops::Range<usize>>,
    /// Root attributes in source order.
    pub attributes: Vec<Attribute>,
}
/// Original spelling of a plain root attribute. Values are not entity-decoded.
#[derive(Debug, Clone)]
pub enum RawAttribute<'a> {
    /// No such static attribute exists.
    Missing,
    /// Attribute is present without a value.
    Boolean,
    /// Original host bytes between value delimiters.
    Value(VerifiedSourceExtent<'a>),
    /// Multiple attributes have this spelling; no value is selected.
    Duplicate,
}
/// Native root-block category; the raw attributes remain available separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// An ordinary script or script setup block.
    Script {
        /// Whether the static setup attribute was present.
        setup: bool,
    },
    /// A style block with Vue-specific transform flags.
    Style {
        /// Whether the static scoped attribute was present.
        scoped: bool,
        /// Whether the static module attribute was present.
        module: bool,
    },
    /// The Vue template source.
    Template,
    /// A framework or user-defined root block.
    Custom,
}
/// Explicit limits of this source-only frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// Full Vue container and framework semantics have not been established.
    HostSemanticsUnanalyzed,
    /// This block was inventoried but its embedded grammar has not been analyzed.
    UnanalyzedBlock {
        /// Index into the host-ordered block inventory.
        block: usize,
    },
    /// An external source needs separately captured identity and resolution.
    ExternalSource {
        /// Index into the host-ordered block inventory.
        block: usize,
    },
    /// Vue permits at most one template; Verter does not diagnose this here.
    DuplicateTemplate {
        /// Index into the host-ordered block inventory.
        block: usize,
    },
}
/// Immutable inventory bound to the exact verified host.
#[derive(Debug)]
pub struct Container<'a> {
    host: VerifiedSourceExtent<'a>,
    blocks: Vec<Block>,
    diagnostics: Vec<Diagnostic>,
    gaps: Vec<Gap>,
}
fn convert(
    open: &NodeTag,
    close: Option<&NodeTag>,
    content: Option<(u32, u32)>,
    attrs: &[NodeProp],
) -> Block {
    Block {
        name: (open.start as usize + 1)..open.name_end as usize,
        open: open.start as usize..open.end as usize,
        content: content.map(|(a, b)| a as usize..b as usize),
        close: close.map(|tag| tag.start as usize..tag.end as usize),
        attributes: attrs
            .iter()
            .map(|a| Attribute {
                name: a.start as usize..a.name_end as usize,
                value: a
                    .value_start
                    .zip(a.value_end)
                    .map(|(a, b)| a as usize..b as usize),
                directive: a.is_directive,
            })
            .collect(),
    }
}
/// Reasons a Vue style block cannot be passed to the plain CSS parser.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StyleRegionError {
    /// Selected block is not a style block.
    NotStyle,
    /// External or absent inline content.
    NoInlineContent,
    /// Ambiguous, encoded or unsupported style language.
    UnsupportedLanguage,
}
/// Bounds on parser input and event processing. Exhaustion is a refusal,
/// never a complete empty inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VueSourceError {
    /// Host exceeds the finite source budget.
    SourceLimit,
    /// Source is not UTF-8.
    NonUtf8,
    /// Native parser emitted too many events or root blocks.
    WorkLimit,
    /// Native parser emitted a range outside the exact host.
    InvalidNativeRange,
    /// Captured bytes do not match the selected host identities.
    HostIdentityMismatch,
}
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_EVENTS: usize = 1_000_000;
const MAX_BLOCKS: usize = 4096;

impl<'a> Container<'a> {
    /// The complete original host, bound to both canonical identity domains.
    pub fn host(&self) -> &VerifiedSourceExtent<'a> {
        &self.host
    }
    /// Root blocks in host order, including duplicated blocks.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
    /// Native parser diagnostics, including duplicate script diagnostics.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    /// Capabilities or host constructs not handled by this source frontend.
    pub fn gaps(&self) -> &[Gap] {
        &self.gaps
    }
    /// Classify a root block using the parser-observed host spans.
    pub fn kind(&self, block: usize) -> Option<BlockKind> {
        let block = self.blocks.get(block)?;
        let source = self.host.text();
        let has = |name: &str| {
            block
                .attributes
                .iter()
                .any(|a| !a.directive && source.get(a.name.clone()) == Some(name))
        };
        Some(match source.get(block.name.clone())? {
            "script" => BlockKind::Script {
                setup: has("setup"),
            },
            "style" => BlockKind::Style {
                scoped: has("scoped"),
                module: has("module"),
            },
            "template" => BlockKind::Template,
            _ => BlockKind::Custom,
        })
    }
    /// Look up an exact plain attribute spelling without decoding entities.
    pub fn raw_attribute(&self, block: usize, name: &str) -> Option<RawAttribute<'a>> {
        let source = self.host.text();
        let mut matching = self
            .blocks
            .get(block)?
            .attributes
            .iter()
            .filter(|a| !a.directive && source.get(a.name.clone()) == Some(name));
        let Some(first) = matching.next() else {
            return Some(RawAttribute::Missing);
        };
        if matching.next().is_some() {
            return Some(RawAttribute::Duplicate);
        }
        match &first.value {
            Some(range) => self
                .host
                .subregion(range.clone())
                .ok()
                .map(RawAttribute::Value),
            None => Some(RawAttribute::Boolean),
        }
    }
    /// Select only verbatim inline CSS; Sass, Less, unknown or ambiguous
    /// `lang` and external `src` are refused. Scoped/module transforms remain
    /// unmodeled even for a returned CSS region.
    pub fn inline_css(&self, block: usize) -> Result<VerifiedSourceExtent<'a>, StyleRegionError> {
        if !matches!(self.kind(block), Some(BlockKind::Style { .. })) {
            return Err(StyleRegionError::NotStyle);
        }
        match self.raw_attribute(block, "lang") {
            Some(RawAttribute::Missing) => {}
            Some(RawAttribute::Value(value)) if value.text() == "css" => {}
            _ => return Err(StyleRegionError::UnsupportedLanguage),
        }
        self.inline_content(block)
            .ok_or(StyleRegionError::NoInlineContent)
    }
    /// Return inline content only if no external source attribute is present.
    pub fn inline_content(&self, block: usize) -> Option<VerifiedSourceExtent<'a>> {
        if self.gaps.contains(&Gap::ExternalSource { block }) {
            return None;
        }
        self.content(block)
    }
    /// Return the verbatim block content with both host identity domains.
    /// External and empty/self-closing blocks can lack content.
    pub fn content(&self, block: usize) -> Option<VerifiedSourceExtent<'a>> {
        let range = self.blocks.get(block)?.content.clone()?;
        self.host.subregion(range).ok()
    }
}

/// Parse one exact host under finite work limits; no transformed content is used.
pub fn parse(bytes: &[u8]) -> Result<Container<'_>, VueSourceError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(VueSourceError::SourceLimit);
    }
    let verified = VerifiedSourceText::capture(bytes).map_err(|_| VueSourceError::NonUtf8)?;
    parse_verified(verified, bytes.len())
}

/// Verify a selected host in both content domains before parsing.
pub fn parse_admitted(
    bytes: &[u8],
    input: InputContentVersion,
    source: ContentId<SourceFactDomain>,
) -> Result<Container<'_>, VueSourceError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(VueSourceError::SourceLimit);
    }
    std::str::from_utf8(bytes).map_err(|_| VueSourceError::NonUtf8)?;
    let verified = VerifiedSourceText::admit(bytes, input, source)
        .map_err(|_| VueSourceError::HostIdentityMismatch)?;
    parse_verified(verified, bytes.len())
}

fn parse_verified(
    verified: VerifiedSourceText<'_>,
    len: usize,
) -> Result<Container<'_>, VueSourceError> {
    let host = verified
        .extent(0..len)
        .map_err(|_| VueSourceError::InvalidNativeRange)?;
    let source = host.text();
    let options = SyntaxPluginOptions::default();
    let ctx = SyntaxPluginContext {
        input: source,
        bytes: source.as_bytes(),
        options: &options,
        diagnostics: Vec::new(),
    };
    let mut syntax = Syntax::new(false);
    let mut blocks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut styles_seen = 0;
    let mut customs_seen = 0;
    let mut events = 0usize;
    let mut exceeded = false;
    tokenize_sfc(source.as_bytes(), |event| {
        events += 1;
        if events > MAX_EVENTS || exceeded {
            exceeded = true;
            return;
        }
        syntax.handle(&event, &ctx);
        if !matches!(
            event,
            Event::CloseTag { .. } | Event::SelfClosingTag { .. } | Event::End
        ) {
            return;
        }
        for node in [syntax.script(), syntax.script_setup()]
            .into_iter()
            .flatten()
        {
            if seen.insert(node.tag_open.start) {
                blocks.push(convert(
                    &node.tag_open,
                    node.tag_close.as_ref(),
                    node.content.map(|s| (s.start, s.end)),
                    &node.attributes,
                ));
            }
        }
        for node in &syntax.style_nodes()[styles_seen..] {
            blocks.push(convert(
                &node.tag_open,
                node.tag_close.as_ref(),
                node.content.map(|s| (s.start, s.end)),
                &node.attributes,
            ));
        }
        styles_seen = syntax.style_nodes().len();
        for node in &syntax.unknown_nodes()[customs_seen..] {
            blocks.push(convert(
                &node.tag_open,
                node.tag_close.as_ref(),
                node.content.map(|s| (s.start, s.end)),
                &node.attributes,
            ));
        }
        customs_seen = syntax.unknown_nodes().len();
        if let Some(ast) = syntax.template_ast() {
            let node = &ast.root;
            if seen.insert(node.tag_open.start) {
                blocks.push(convert(
                    &node.tag_open,
                    node.tag_close.as_ref(),
                    node.content.as_ref().map(|s| (s.start, s.end)),
                    &node.attributes,
                ));
            }
        }
        if blocks.len() > MAX_BLOCKS {
            exceeded = true;
        }
    });
    if exceeded {
        return Err(VueSourceError::WorkLimit);
    }
    blocks.sort_by_key(|block| block.open.start);
    let valid = |range: &std::ops::Range<usize>| {
        range.start <= range.end && source.get(range.clone()).is_some()
    };
    if blocks.iter().any(|b| {
        !valid(&b.name)
            || !valid(&b.open)
            || b.content.as_ref().is_some_and(|r| !valid(r))
            || b.close.as_ref().is_some_and(|r| !valid(r))
            || b.attributes
                .iter()
                .any(|a| !valid(&a.name) || a.value.as_ref().is_some_and(|r| !valid(r)))
    }) {
        return Err(VueSourceError::InvalidNativeRange);
    }
    // Check the native container relationships as well as the individual
    // ranges: a valid UTF-8 slice alone is not proof of block ownership.
    for block in &blocks {
        if block.name.start < block.open.start
            || block.name.end > block.open.end
            || block.attributes.iter().any(|a| {
                a.name.start < block.open.start
                    || a.name.end > block.open.end
                    || a.value
                        .as_ref()
                        .is_some_and(|v| v.start < block.open.start || v.end > block.open.end)
            })
            || block
                .content
                .as_ref()
                .is_some_and(|r| r.start < block.open.end)
            || block.close.as_ref().is_some_and(|close| {
                close.start < block.open.end
                    || block.content.as_ref().is_some_and(|r| r.end > close.start)
            })
        {
            return Err(VueSourceError::InvalidNativeRange);
        }
    }
    let mut gaps = vec![Gap::HostSemanticsUnanalyzed];
    let mut template_seen = false;
    for (index, block) in blocks.iter().enumerate() {
        gaps.push(Gap::UnanalyzedBlock { block: index });
        if &source[block.name.clone()] == "template" {
            if template_seen {
                gaps.push(Gap::DuplicateTemplate { block: index });
            }
            template_seen = true;
        }
        if block
            .attributes
            .iter()
            .any(|a| !a.directive && &source[a.name.clone()] == "src")
        {
            gaps.push(Gap::ExternalSource { block: index });
        }
    }
    Ok(Container {
        host,
        blocks,
        diagnostics: syntax.take_diagnostics(),
        gaps,
    })
}

mod script;
pub use script::{
    DeclarationKind, ScriptCall, ScriptCoverage, ScriptDeclaration, ScriptParse, ScriptParseError,
    ScriptSyntaxDiagnostic,
};

/// Cooperative native script replay controls shared across a query.
pub use tsz_common::{ExecutionCheckpoint, ProjectExecutionBudget};

mod inventory;
pub use inventory::{
    VerifiedVueCall, VerifiedVueDeclaration, VerifiedVueInventory, VueCapabilityCoverage,
    VueCoverage, VueDeclarationPage, VueHostInventory, VueInventoryRecord, VueInventoryRelation,
    VueInventoryRootSchema, VueUnavailable, vue_path_inventory_digest, vue_selection_key,
};

/// Bind the native Vue/TSZ implementation, exact vendored compiler sources,
/// dependency lock and Rust toolchain to a support fingerprint.
#[must_use]
pub fn support_fingerprint() -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-native-support.v1\0");
    for bytes in [
        include_bytes!("lib.rs").as_slice(),
        include_bytes!("script.rs").as_slice(),
        include_bytes!("inventory.rs").as_slice(),
        include_bytes!("../Cargo.toml").as_slice(),
        include_bytes!("../build.rs").as_slice(),
        env!("VUE_NATIVE_BUILD_BASIS").as_bytes(),
    ] {
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    *hash.finalize().as_bytes()
}
