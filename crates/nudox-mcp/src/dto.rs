//! JSON projections of the `nudox-engine::wire` vocabulary (§L2).
//!
//! # Why this module exists — read before "simplifying" it
//!
//! LR-2 requires that every MCP tool schema be `schemars`-derived from a named
//! Rust type. The natural way to satisfy that is
//! `#[derive(Serialize, JsonSchema)]` on the wire types themselves, in
//! `nudox-engine`. That is the correct end state and this module should be
//! deleted the day it happens (see the crate-level `TODO(engine)` block).
//!
//! It does not happen here because two language rules stand in the way:
//!
//! 1. **Orphan rule.** `HitRow` and `Serialize` are both foreign to this
//!    crate, so `impl Serialize for HitRow` is not expressible.
//! 2. **`#[non_exhaustive]`.** Every wire enum carries it (LR-12), which also
//!    rules out serde's `#[serde(remote = "…")]` escape hatch: the generated
//!    match has no wildcard arm and will not compile against a
//!    `#[non_exhaustive]` foreign enum.
//!
//! So the projection is explicit `From<&wire::T>` conversions into owned,
//! derive-friendly types. Three properties keep this from becoming a second
//! data model:
//!
//! * **One direction only.** Conversions go `wire → dto`. Nothing here is ever
//!   converted back into a wire type, so the wire type stays the single source
//!   of truth and the two cannot disagree about *meaning*.
//! * **No re-derivation.** Nothing here recomputes a signature, a kind label,
//!   or a doc section — LR-3/LR-4 keep that in `engine::chunk`. Every field is
//!   a copy of an already-computed wire field.
//! * **Forward-compatible by construction.** Each `match` on a
//!   `#[non_exhaustive]` wire enum ends in a `_ =>` arm that maps to an
//!   `unknown` JSON variant. A producer that adds a kind or a section widens
//!   the wire enum without breaking this crate — which is exactly the contract
//!   LR-12 asks for.
//!
//! # Encoding notes
//!
//! * `SymbolKey` is rendered as the `ecosystem:name#introhex` string that
//!   `nudox-graph`'s schema already documents as *the* key form, so a key
//!   copied out of a `graph_query` result can be pasted straight into
//!   `get_symbol`. This is LR-1's one key in its wire spelling — not a second
//!   id type.
//! * Enums are internally tagged with `"kind"` / `"type"` discriminators so an
//!   LLM client reading raw JSON can tell variants apart without positional
//!   guessing.

use std::time::{SystemTime, UNIX_EPOCH};

use nudox_engine::wire::{
    self, CalloutLevel, CrumbRef, FieldRow, HitRow, InlineRun, KindTag, LinkTarget, MemberRow,
    ProseBlock, Provenance, RenderSection, SectionKind, SectionPlan, SigToken, SizeHint,
    SymbolHead, SymbolKey, Visibility,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;

// ---------------------------------------------------------------------------
// SymbolKey  —  "ecosystem:name#introhex"
// ---------------------------------------------------------------------------

/// The wire spelling of [`SymbolKey`] (LR-1), as `ecosystem:name#introhex`.
///
/// Example: `cargo:serde#3f1a…` (the intro half is 64 lowercase hex chars).
/// This is byte-identical to the `key` property in `schema.graphql`, so keys
/// move between `graph_query`, `search_symbols` and `get_symbol` unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SymbolKeyDto(pub String);

impl SymbolKeyDto {
    /// Render a wire key into its canonical string form.
    pub fn from_wire(key: &SymbolKey) -> Self {
        Self(format!(
            "{}:{}#{}",
            key.package.ecosystem.as_str(),
            key.package.name.as_str(),
            key.intro.to_hex()
        ))
    }

    /// Parse the string form back into a wire key.
    ///
    /// Every failure is an [`McpError::MalformedKey`] carrying the input, so an
    /// agent that mangled a key sees what it actually sent.
    pub fn to_wire(&self) -> Result<SymbolKey, McpError> {
        let malformed = |reason: &'static str| McpError::MalformedKey {
            key: self.0.clone(),
            reason,
        };

        let (lineage, intro_hex) = self
            .0
            .split_once('#')
            .ok_or_else(|| malformed("expected 'ecosystem:name#introhex' — no '#' found"))?;
        let (ecosystem, name) = lineage
            .split_once(':')
            .ok_or_else(|| malformed("expected 'ecosystem:name' before '#' — no ':' found"))?;
        if ecosystem.is_empty() {
            return Err(malformed("ecosystem segment is empty"));
        }
        if name.is_empty() {
            return Err(malformed("package name segment is empty"));
        }
        let intro = parse_intro_hex(intro_hex)
            .ok_or_else(|| malformed("intro segment must be 64 lowercase hex characters"))?;

        // The only two `nudox-ir` items this crate touches; see the
        // `TODO(engine)` note beside the dependency in Cargo.toml. Constructing
        // an id newtype is not "reading the IR" — no `Entry` or `IrView` is
        // reachable from here.
        Ok(SymbolKey::new(
            wire::PackageLineageId::new(
                nudox_ir::change::EcosystemId::new(ecosystem),
                nudox_ir::change::PackageName::new(name),
            ),
            intro,
        ))
    }
}

/// Decode a 64-character hex string into an `IntroId`.
///
/// Rejects odd lengths, wrong lengths, and non-hex bytes; returns `None`
/// rather than a partially-decoded id, so a truncated key can never resolve to
/// a *different* symbol.
fn parse_intro_hex(s: &str) -> Option<wire::IntroId> {
    if s.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let hi = hex_nibble(s.as_bytes()[i * 2])?;
        let lo = hex_nibble(s.as_bytes()[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(wire::IntroId::from_raw(bytes))
}

/// One hex character to its nibble value.
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Small scalars
// ---------------------------------------------------------------------------

/// A symbol's kind label (`"Function"`, `"Record"`, …).
///
/// `KindTag::Unknown(u16)` — a kind emitted by a producer newer than this
/// binary — renders as `"unknown(<n>)"` rather than being dropped, so an agent
/// sees that something exists even when we cannot name it (LD-7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct KindDto(pub String);

impl KindDto {
    /// Project a wire kind tag.
    pub fn from_wire(kind: KindTag) -> Self {
        match kind {
            KindTag::Known(d) => Self(format!("{d:?}")),
            KindTag::Unknown(raw) => Self(format!("unknown({raw})")),
            _ => Self("unknown".to_owned()),
        }
    }
}

/// A symbol's access modifier.
///
/// The six variants are the union across every supported language, so a Java
/// `protected` and a Rust `pub(crate)` stay distinguishable rather than being
/// flattened into one "not public" bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityDto {
    /// Unrestricted; visible to all consumers.
    Public,
    /// Visible only within the declaring scope.
    Private,
    /// Visible within the declaring type and its subtypes.
    Protected,
    /// Visible within the assembly or crate (C#-style `internal`).
    Internal,
    /// Visible within the package or module (Java default, Go unexported).
    Package,
    /// Visible within the current Rust crate (`pub(crate)`).
    Crate,
    /// A visibility this binary does not understand.
    Unknown,
}

impl VisibilityDto {
    /// Project a wire visibility.
    pub fn from_wire(v: Visibility) -> Self {
        match v {
            Visibility::Public => Self::Public,
            Visibility::Private => Self::Private,
            Visibility::Protected => Self::Protected,
            Visibility::Internal => Self::Internal,
            Visibility::Package => Self::Package,
            Visibility::Crate => Self::Crate,
        }
    }
}

/// How confidently this symbol's IR was produced, and from where (§L2.1).
///
/// `trusted_local` means it was produced on this machine from source we can
/// see; anything else means some part of the answer came from a remote
/// generation or a stale cache. Agents making correctness-sensitive claims
/// should prefer `trusted_local` results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "trust", rename_all = "snake_case")]
pub enum ProvenanceDto {
    /// Produced on this machine from visible source.
    TrustedLocal,
    /// Fetched and cryptographically verified from a remote generation.
    SyncedLocal {
        /// The remote generation this was verified against.
        generation: u64,
    },
    /// Served remotely; not materialised locally.
    Remote {
        /// The remote generation serving this answer.
        generation: u64,
    },
    /// Last-known-good, served while offline.
    Stale {
        /// Unix seconds at which this answer was last known good.
        as_of_unix_secs: u64,
    },
    /// A provenance class this binary does not understand.
    Unknown,
}

impl ProvenanceDto {
    /// Project a wire provenance.
    pub fn from_wire(p: &Provenance) -> Self {
        match p {
            Provenance::TrustedLocal => Self::TrustedLocal,
            Provenance::SyncedLocal { generation } => {
                Self::SyncedLocal { generation: generation.0 }
            }
            Provenance::Remote { generation } => Self::Remote { generation: generation.0 },
            Provenance::Stale { as_of } => Self::Stale { as_of_unix_secs: unix_secs(*as_of) },
            _ => Self::Unknown,
        }
    }
}

/// Unix seconds, saturating at the epoch for pre-1970 clocks.
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Signature tokens
// ---------------------------------------------------------------------------

/// One token of a rendered signature (LR-4: produced once, by the chunker).
///
/// Tokens are returned rather than a flat string so an agent can distinguish a
/// type reference from an identifier, and can follow `target` to the symbol a
/// type resolves to. [`SignatureDto::text`] is available when the flat form is
/// all that is wanted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "token", rename_all = "snake_case")]
pub enum SigTokenDto {
    /// A language keyword (`fn`, `struct`, `impl`, …).
    Keyword {
        /// The keyword text.
        text: String,
    },
    /// An identifier: a symbol or parameter name.
    Ident {
        /// The identifier text.
        text: String,
    },
    /// A type reference.
    Type {
        /// The rendered type text.
        text: String,
        /// The symbol this type resolves to, when it resolves through the
        /// corpus. Feed it straight back into `get_symbol`.
        target: Option<SymbolKeyDto>,
    },
    /// Punctuation (`(`, `,`, `->`, …).
    Punct {
        /// The punctuation text.
        text: String,
    },
    /// A single whitespace separator.
    Space,
    /// A generic parameter name (`T`, `K`, …).
    Generic {
        /// The parameter name.
        text: String,
    },
    /// A lifetime name (`'a`, `'static`, …).
    Lifetime {
        /// The lifetime name.
        text: String,
    },
    /// A token class this binary does not understand.
    Unknown,
}

impl SigTokenDto {
    /// Project a wire signature token.
    pub fn from_wire(t: &SigToken) -> Self {
        match t {
            SigToken::Kw(s) => Self::Keyword { text: (*s).to_owned() },
            SigToken::Ident(s) => Self::Ident { text: s.to_string() },
            SigToken::Ty { text, target } => Self::Type {
                text: text.to_string(),
                target: target.as_ref().map(SymbolKeyDto::from_wire),
            },
            SigToken::Punct(s) => Self::Punct { text: (*s).to_owned() },
            SigToken::Ws => Self::Space,
            SigToken::Generic(s) => Self::Generic { text: s.to_string() },
            SigToken::Lifetime(s) => Self::Lifetime { text: s.to_string() },
            _ => Self::Unknown,
        }
    }

    /// The token's literal text, or `" "` for a space.
    fn text(&self) -> &str {
        match self {
            Self::Keyword { text }
            | Self::Ident { text }
            | Self::Type { text, .. }
            | Self::Punct { text }
            | Self::Generic { text }
            | Self::Lifetime { text } => text,
            Self::Space => " ",
            Self::Unknown => "",
        }
    }
}

/// A rendered signature: the tokens plus their flattened text.
///
/// Both forms are sent because they serve different consumers — `text` is what
/// an agent quotes back to a user, `tokens` is what it walks to find linkable
/// types. Neither is recomputed here; `text` is a concatenation of the tokens
/// the chunker already produced (LR-4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SignatureDto {
    /// The signature as a single line of text.
    pub text: String,
    /// The same signature, tokenised.
    pub tokens: Vec<SigTokenDto>,
}

impl SignatureDto {
    /// Project a wire token slice.
    pub fn from_wire(tokens: &[SigToken]) -> Self {
        let tokens: Vec<SigTokenDto> = tokens.iter().map(SigTokenDto::from_wire).collect();
        let text = tokens.iter().map(SigTokenDto::text).collect::<String>();
        Self { text, tokens }
    }
}

// ---------------------------------------------------------------------------
// Prose
// ---------------------------------------------------------------------------

/// The destination of a link inside rendered documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum LinkTargetDto {
    /// A cross-reference to another symbol in the corpus.
    Symbol {
        /// The referenced symbol; usable with `get_symbol`.
        key: SymbolKeyDto,
    },
    /// An external URL.
    Url {
        /// The URL.
        url: String,
    },
    /// A link class this binary does not understand.
    Unknown,
}

impl LinkTargetDto {
    /// Project a wire link target.
    fn from_wire(t: &LinkTarget) -> Self {
        match t {
            LinkTarget::Symbol(key) => Self::Symbol { key: SymbolKeyDto::from_wire(key) },
            LinkTarget::Url(url) => Self::Url { url: url.to_string() },
            _ => Self::Unknown,
        }
    }
}

/// One inline run of documentation text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "run", rename_all = "snake_case")]
pub enum InlineRunDto {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// An inline code span.
    Code {
        /// The code text.
        text: String,
    },
    /// Strong/bold text.
    Strong {
        /// The text.
        text: String,
    },
    /// Emphasised/italic text.
    Emphasis {
        /// The text.
        text: String,
    },
    /// A hyperlink.
    Link {
        /// The link's visible text.
        text: String,
        /// Where it points.
        target: LinkTargetDto,
    },
    /// A run class this binary does not understand.
    Unknown,
}

impl InlineRunDto {
    /// Project a wire inline run.
    fn from_wire(r: &InlineRun) -> Self {
        match r {
            InlineRun::Text(s) => Self::Text { text: s.to_string() },
            InlineRun::Code(s) => Self::Code { text: s.to_string() },
            InlineRun::Strong(s) => Self::Strong { text: s.to_string() },
            InlineRun::Em(s) => Self::Emphasis { text: s.to_string() },
            InlineRun::Link { text, target } => Self::Link {
                text: text.to_string(),
                target: LinkTargetDto::from_wire(target),
            },
            _ => Self::Unknown,
        }
    }
}

/// One block-level element of documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "block", rename_all = "snake_case")]
pub enum ProseBlockDto {
    /// A paragraph.
    Paragraph {
        /// Its inline runs, in order.
        runs: Vec<InlineRunDto>,
    },
    /// A heading below the section split level.
    Heading {
        /// Heading depth.
        level: u8,
        /// Its inline runs, in order.
        runs: Vec<InlineRunDto>,
    },
    /// A bulleted or numbered list.
    List {
        /// `true` for a numbered list.
        ordered: bool,
        /// One entry per item, each a run sequence.
        items: Vec<Vec<InlineRunDto>>,
    },
    /// A thematic break.
    Rule,
    /// A fenced code block.
    Code {
        /// The fence info string (`rust`, `python`, …); empty when unfenced.
        lang: String,
        /// The block's text.
        text: String,
        /// Number of lines in `text`.
        line_count: u32,
    },
    /// A block class this binary does not understand.
    Unknown,
}

impl ProseBlockDto {
    /// Project a wire prose block.
    fn from_wire(b: &ProseBlock) -> Self {
        match b {
            ProseBlock::Paragraph(runs) => {
                Self::Paragraph { runs: runs.iter().map(InlineRunDto::from_wire).collect() }
            }
            ProseBlock::Heading { level, runs } => Self::Heading {
                level: *level,
                runs: runs.iter().map(InlineRunDto::from_wire).collect(),
            },
            ProseBlock::List { ordered, items } => Self::List {
                ordered: *ordered,
                items: items
                    .iter()
                    .map(|item| item.iter().map(InlineRunDto::from_wire).collect())
                    .collect(),
            },
            ProseBlock::Rule => Self::Rule,
            ProseBlock::Code { lang, text, line_count } => Self::Code {
                lang: lang.0.to_string(),
                text: text.to_string(),
                line_count: *line_count,
            },
            _ => Self::Unknown,
        }
    }
}

/// The semantic level of a callout block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalloutLevelDto {
    /// Informational.
    Note,
    /// Non-critical warning.
    Warning,
    /// Critical or destructive.
    Danger,
    /// A helpful tip.
    Tip,
    /// A callout class this binary does not understand.
    Unknown,
}

impl CalloutLevelDto {
    /// Project a wire callout level.
    fn from_wire(l: CalloutLevel) -> Self {
        match l {
            CalloutLevel::Note => Self::Note,
            CalloutLevel::Warning => Self::Warning,
            CalloutLevel::Danger => Self::Danger,
            CalloutLevel::Tip => Self::Tip,
            _ => Self::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Member / field rows
// ---------------------------------------------------------------------------

/// A child symbol listed in a `members` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemberRowDto {
    /// The member's key; usable with `get_symbol`.
    pub key: SymbolKeyDto,
    /// Its short name.
    pub name: String,
    /// Its rendered signature.
    pub signature: SignatureDto,
    /// Its kind label.
    pub kind: KindDto,
    /// Its access modifier.
    pub visibility: VisibilityDto,
}

impl MemberRowDto {
    /// Project a wire member row.
    fn from_wire(m: &MemberRow) -> Self {
        Self {
            key: SymbolKeyDto::from_wire(&m.key),
            name: m.name.to_string(),
            signature: SignatureDto::from_wire(&m.sig),
            kind: KindDto::from_wire(m.kind),
            visibility: VisibilityDto::from_wire(m.visibility),
        }
    }
}

/// A field or enum variant listed in a `fields` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FieldRowDto {
    /// The field's key; usable with `get_symbol`.
    pub key: SymbolKeyDto,
    /// Its short name.
    pub name: String,
    /// Its rendered type.
    pub ty: SignatureDto,
    /// Its kind label (`Field` or `Variant`).
    pub kind: KindDto,
}

impl FieldRowDto {
    /// Project a wire field row.
    fn from_wire(f: &FieldRow) -> Self {
        Self {
            key: SymbolKeyDto::from_wire(&f.key),
            name: f.name.to_string(),
            ty: SignatureDto::from_wire(&f.ty_tokens),
            kind: KindDto::from_wire(f.kind),
        }
    }
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// One rendered section of a symbol's documentation page.
///
/// Sections arrive in the order given by [`SymbolHeadDto::section_plan`], which
/// is the same order a human reader sees in the GUI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "section", rename_all = "snake_case")]
pub enum RenderSectionDto {
    /// Free-form documentation prose.
    Prose {
        /// This section's id, matching its `section_plan` entry.
        id: u32,
        /// The block content.
        blocks: Vec<ProseBlockDto>,
    },
    /// A standalone code block.
    CodeBlock {
        /// This section's id.
        id: u32,
        /// The fence info string.
        lang: String,
        /// The code text.
        text: String,
        /// Number of lines in `text`.
        line_count: u32,
    },
    /// The symbol's module / trait / impl children.
    Members {
        /// This section's id.
        id: u32,
        /// One row per child.
        entries: Vec<MemberRowDto>,
    },
    /// The symbol's fields and enum variants.
    Fields {
        /// This section's id.
        id: u32,
        /// One row per field or variant.
        entries: Vec<FieldRowDto>,
    },
    /// A documentation section headed `Example` or `Examples`.
    Examples {
        /// This section's id.
        id: u32,
        /// The block content.
        blocks: Vec<ProseBlockDto>,
    },
    /// A `[!NOTE]`-style callout.
    Callout {
        /// This section's id.
        id: u32,
        /// Its severity.
        level: CalloutLevelDto,
        /// The block content.
        blocks: Vec<ProseBlockDto>,
    },
    /// A section kind this binary does not understand.
    Unknown {
        /// This section's id.
        id: u32,
        /// The raw tag the producer emitted.
        tag: String,
    },
}

impl RenderSectionDto {
    /// Project a wire section.
    pub fn from_wire(s: &RenderSection) -> Self {
        let id = s.section_id().0;
        match s {
            RenderSection::Prose { blocks, .. } => {
                Self::Prose { id, blocks: blocks.iter().map(ProseBlockDto::from_wire).collect() }
            }
            RenderSection::CodeBlock { lang, text, line_count, .. } => Self::CodeBlock {
                id,
                lang: lang.0.to_string(),
                text: text.to_string(),
                line_count: *line_count,
            },
            RenderSection::Members { entries, .. } => {
                Self::Members { id, entries: entries.iter().map(MemberRowDto::from_wire).collect() }
            }
            RenderSection::Fields { entries, .. } => {
                Self::Fields { id, entries: entries.iter().map(FieldRowDto::from_wire).collect() }
            }
            RenderSection::Examples { blocks, .. } => Self::Examples {
                id,
                blocks: blocks.iter().map(ProseBlockDto::from_wire).collect(),
            },
            RenderSection::Callout { level, blocks, .. } => Self::Callout {
                id,
                level: CalloutLevelDto::from_wire(*level),
                blocks: blocks.iter().map(ProseBlockDto::from_wire).collect(),
            },
            RenderSection::Unknown { kind_tag, .. } => {
                Self::Unknown { id, tag: kind_tag.to_string() }
            }
            _ => Self::Unknown { id, tag: "unrecognised".to_owned() },
        }
    }
}

/// What kind of content a planned section will hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKindDto {
    /// Free-form prose.
    Prose,
    /// A standalone code block.
    CodeBlock,
    /// Module / trait / impl children.
    Members,
    /// Fields and enum variants.
    Fields,
    /// An `Examples` section.
    Examples,
    /// A callout blockquote.
    Callout,
    /// A section kind this binary does not understand.
    Unknown,
}

impl SectionKindDto {
    /// Project a wire section kind.
    fn from_wire(k: SectionKind) -> Self {
        match k {
            SectionKind::Prose => Self::Prose,
            SectionKind::CodeBlock => Self::CodeBlock,
            SectionKind::Members => Self::Members,
            SectionKind::Fields => Self::Fields,
            SectionKind::Examples => Self::Examples,
            SectionKind::Callout => Self::Callout,
            _ => Self::Unknown,
        }
    }
}

/// An entry in the section plan: what will appear, and roughly how large.
///
/// The size hint is the GUI's skeleton geometry. It is included because it is a
/// cheap, already-computed signal of how much content a section holds, which
/// lets an agent decide whether a symbol is worth reading in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SectionPlanDto {
    /// The id of the section this entry describes.
    pub id: u32,
    /// What kind of section it will be.
    pub kind: SectionKindDto,
    /// Estimated lines of text, when the section is text-shaped.
    pub estimated_lines: Option<u32>,
    /// Estimated row count, when the section is a table of members or fields.
    pub estimated_rows: Option<u32>,
}

impl SectionPlanDto {
    /// Project a wire section plan entry.
    fn from_wire(p: &SectionPlan) -> Self {
        let (estimated_lines, estimated_rows) = match p.size_hint {
            SizeHint::Lines(n) => (Some(n), None),
            SizeHint::Rows(n) => (None, Some(n)),
            _ => (None, None),
        };
        Self {
            id: p.id.0,
            kind: SectionKindDto::from_wire(p.kind),
            estimated_lines,
            estimated_rows,
        }
    }
}

// ---------------------------------------------------------------------------
// Breadcrumb + head
// ---------------------------------------------------------------------------

/// One ancestor in a symbol's path, root-first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrumbDto {
    /// The ancestor's key; usable with `get_symbol`.
    pub key: SymbolKeyDto,
    /// Its display label.
    pub label: String,
}

impl CrumbDto {
    /// Project a wire crumb.
    fn from_wire(c: &CrumbRef) -> Self {
        Self { key: SymbolKeyDto::from_wire(&c.key), label: c.label.to_string() }
    }
}

/// A symbol's header: identity, signature, and what its page contains.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolHeadDto {
    /// This symbol's key.
    pub key: SymbolKeyDto,
    /// Its ancestor chain, root-first.
    pub breadcrumb: Vec<CrumbDto>,
    /// Its rendered signature.
    pub signature: SignatureDto,
    /// Its kind label.
    pub kind: KindDto,
    /// Its access modifier.
    pub visibility: VisibilityDto,
    /// How trustworthy this answer is.
    pub provenance: ProvenanceDto,
    /// The deprecation notice, when the symbol carries one.
    pub deprecation: Option<String>,
    /// What sections the page contains, in order, with size estimates.
    pub section_plan: Vec<SectionPlanDto>,
}

impl SymbolHeadDto {
    /// Project a wire symbol head.
    pub fn from_wire(h: &SymbolHead) -> Self {
        Self {
            key: SymbolKeyDto::from_wire(&h.key),
            breadcrumb: h.breadcrumb.iter().map(CrumbDto::from_wire).collect(),
            signature: SignatureDto::from_wire(&h.signature),
            kind: KindDto::from_wire(h.kind),
            visibility: VisibilityDto::from_wire(h.visibility),
            provenance: ProvenanceDto::from_wire(&h.provenance),
            deprecation: h.deprecation.as_ref().map(|d| d.to_string()),
            section_plan: h.section_plan.iter().map(SectionPlanDto::from_wire).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Search hit
// ---------------------------------------------------------------------------

/// One search result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HitRowDto {
    /// The matched symbol's key. Pass this to `get_symbol` or `find_usages`.
    pub key: SymbolKeyDto,
    /// The name to display, path-qualified when needed to disambiguate.
    pub display_name: String,
    /// An abbreviated signature preview.
    pub signature: SignatureDto,
    /// The symbol's kind label.
    pub kind: KindDto,
    /// How trustworthy this answer is.
    pub provenance: ProvenanceDto,
    /// Relevance score; higher is more relevant. Results are sorted by it.
    pub score: f32,
}

impl HitRowDto {
    /// Project a wire hit row.
    pub fn from_wire(h: &HitRow) -> Self {
        Self {
            key: SymbolKeyDto::from_wire(&h.key),
            display_name: h.display_name.to_string(),
            signature: SignatureDto::from_wire(&h.sig_preview),
            kind: KindDto::from_wire(h.kind),
            provenance: ProvenanceDto::from_wire(&h.provenance),
            score: h.score,
        }
    }

    /// The `ecosystem:name` lineage prefix of this hit's key.
    ///
    /// Used by `search_symbols`'s optional `packages` filter, which the engine's
    /// `SearchQuery` does not express — see the `TODO(engine)` note in
    /// `crate::tools`.
    pub fn lineage(&self) -> Option<&str> {
        self.key.0.split_once('#').map(|(lineage, _)| lineage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed key that exercises every segment of the grammar.
    fn sample_key() -> String {
        format!("cargo:serde#{}", "ab".repeat(32))
    }

    #[test]
    fn symbol_key_round_trips_through_the_wire_type() {
        let dto = SymbolKeyDto(sample_key());
        let wire = dto.to_wire().expect("sample key parses");
        let back = SymbolKeyDto::from_wire(&wire);
        assert_eq!(dto, back, "wire -> string -> wire must be lossless");
    }

    #[test]
    fn symbol_key_rejects_every_malformed_shape() {
        let cases = [
            ("", "empty"),
            ("cargo:serde", "no '#'"),
            ("serde#abcd", "no ':'"),
            (":serde#abcd", "empty ecosystem"),
            ("cargo:#abcd", "empty name"),
            ("cargo:serde#", "empty intro"),
            ("cargo:serde#zz", "non-hex intro"),
        ];
        for (input, why) in cases {
            assert!(
                SymbolKeyDto(input.to_owned()).to_wire().is_err(),
                "should have rejected {input:?} ({why})"
            );
        }
        // A 63-char intro must not be accepted by zero-padding.
        let short = format!("cargo:serde#{}", "a".repeat(63));
        assert!(SymbolKeyDto(short).to_wire().is_err(), "truncated intro must not resolve");
    }

    #[test]
    fn signature_text_is_the_concatenation_of_its_tokens() {
        let sig = SignatureDto {
            text: String::new(),
            tokens: vec![
                SigTokenDto::Keyword { text: "fn".into() },
                SigTokenDto::Space,
                SigTokenDto::Ident { text: "parse".into() },
            ],
        };
        let text: String = sig.tokens.iter().map(SigTokenDto::text).collect();
        assert_eq!(text, "fn parse");
    }

    #[test]
    fn lineage_is_the_prefix_before_the_hash() {
        let hit_key = SymbolKeyDto(sample_key());
        let hit = HitRowDto {
            key: hit_key,
            display_name: "Deserializer".into(),
            signature: SignatureDto { text: String::new(), tokens: vec![] },
            kind: KindDto("Trait".into()),
            provenance: ProvenanceDto::TrustedLocal,
            score: 1.0,
        };
        assert_eq!(hit.lineage(), Some("cargo:serde"));
    }
}
