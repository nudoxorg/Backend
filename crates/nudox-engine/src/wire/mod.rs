//! The wire protocol: the complete type vocabulary crossing the engine↔GUI seam.
//!
//! # Why this module exists
//!
//! `lindsey` (the GPUI shell) must remain entirely ignorant of `nudox-ir`,
//! `nudox-store`, and `nudox-graph`.  Every datum it renders crosses exactly
//! one boundary — this module — so that the dependency law (§L0) is
//! structurally enforced rather than merely aspirational.
//!
//! # §L2 implementation notes
//!
//! * `SymbolKey` is a re-export of `nudox_ir::change::StableRef` (LR-1).
//!   No parallel id type is invented here.
//! * `SharedStr` wraps `triomphe::Arc<str>`.  It is deliberately GUI-free:
//!   `lindsey` adds its own `From<SharedStr> for gpui::SharedString` impl
//!   without this crate ever importing gpui.
//! * Every enum that crosses a version boundary is `#[non_exhaustive]` and
//!   carries an `Unknown`/`Other` variant (LR-12, LD-7).
//! * All payload-bearing events use `Arc<[T]>` / `SharedStr` so the drain
//!   loop moves pointers, never buffers (GUI-PLAN §2.2.5).

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Serialize;
use triomphe::Arc as TArc;

// LR-1 says `SymbolKey` is the one key everywhere — which means the pieces
// needed to *build* one must travel with it. Without `EcosystemId` and
// `PackageName` a consumer can receive a key but never construct one, and would
// be pushed into inventing a parallel id type, which is exactly what LR-1
// forbids.
pub use nudox_ir::change::{
    EcosystemId, IntroId, PackageLineageId, PackageName, StableRef as SymbolKey,
};
pub use nudox_ir::entry::Visibility;
pub use nudox_ir::kind::KindDiscriminant;

// ---------------------------------------------------------------------------
// Serialisation helpers
// ---------------------------------------------------------------------------

/// Serialise a `SymbolKey` as the canonical `"ecosystem:name#introhex"` string.
pub fn serialize_symbol_key<S: serde::Serializer>(
    key: &SymbolKey,
    s: S,
) -> Result<S::Ok, S::Error> {
    let repr = format!(
        "{}:{}#{}",
        key.package.ecosystem.as_str(),
        key.package.name.as_str(),
        key.intro.to_hex()
    );
    s.serialize_str(&repr)
}

/// Serialise an `Option<SymbolKey>` as an optional canonical string.
pub fn serialize_symbol_key_opt<S: serde::Serializer>(
    key: &Option<SymbolKey>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match key {
        Some(k) => serialize_symbol_key(k, s),
        None => s.serialize_none(),
    }
}

/// Serialise a `PackageLineageId` as `"ecosystem:name"`.
pub fn serialize_lineage_id<S: serde::Serializer>(
    id: &PackageLineageId,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{}:{}", id.ecosystem.as_str(), id.name.as_str()))
}

/// Serialise a `SystemTime` as unix seconds (u64).
pub fn serialize_unix_secs<S: serde::Serializer>(
    t: &SystemTime,
    s: S,
) -> Result<S::Ok, S::Error> {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    s.serialize_u64(secs)
}

// ---------------------------------------------------------------------------
// SharedStr
// ---------------------------------------------------------------------------

/// A cheaply-cloneable, immutable string that does not depend on gpui.
///
/// Backed by `triomphe::Arc<str>` (one word smaller than `std::sync::Arc<str>`
/// — no weak-count slot).  `lindsey` converts to `gpui::SharedString` behind a
/// local `From` impl so this crate stays GUI-free.
///
/// All wire payloads use `SharedStr` instead of `String` so that the drain
/// loop never allocates on the GUI thread — it clones `Arc` handles, not
/// heap buffers.
#[derive(Clone)]
pub struct SharedStr(TArc<str>);

impl SharedStr {
    /// Wrap an already-constructed `triomphe::Arc<str>`.
    #[inline]
    pub fn from_arc(arc: TArc<str>) -> Self {
        Self(arc)
    }

    /// Borrow the underlying arc.
    #[inline]
    pub fn as_arc(&self) -> &TArc<str> {
        &self.0
    }
}

impl From<&str> for SharedStr {
    fn from(s: &str) -> Self {
        Self(TArc::from(s))
    }
}

impl From<String> for SharedStr {
    fn from(s: String) -> Self {
        Self(TArc::from(s.as_str()))
    }
}

impl Deref for SharedStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.0)
    }
}

impl PartialEq for SharedStr {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}

impl Eq for SharedStr {}

impl Hash for SharedStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Serialize for SharedStr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl JsonSchema for SharedStr {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SharedStr")
    }
    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }
}

// ---------------------------------------------------------------------------
// Newtype ids  (§L7.2: newtype every id)
// ---------------------------------------------------------------------------

/// A monotonically increasing token that identifies one *logical query
/// generation*.  Every event in a stream carries the `Gen` it answers; stores
/// drop events whose gen ≠ current (GUI-PLAN §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct Gen(pub u64);

/// A stable, opaque id for a remote corpus generation (used in `Provenance`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct GenerationId(pub u64);

/// A per-stream section id, assigned sequentially by the chunker.
///
/// `SectionId(0)` is reserved (never emitted); sections begin at 1.
/// The GUI uses these to correlate `Highlight` events with already-painted
/// sections, and to key the `ListState` geometry (§9.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct SectionId(pub u32);

/// Discriminates the three search result sections (Name / Type / Semantic).
///
/// Kept as a `u8` newtype rather than an enum so forward-compat variants can be
/// added without a breaking match arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct SearchSectionId(pub u8);

// ---------------------------------------------------------------------------
// Provenance  (§L2.1)
// ---------------------------------------------------------------------------

/// How confidently this symbol's IR was produced and from where.
///
/// Every symbol hit, tab, and package row carries a provenance badge (LD-8).
/// The GUI maps each variant to a trust-chrome color token (§10.4) — the
/// variant *is* the state; no boolean flags.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provenance {
    /// Produced on this machine from source we can see (§10.4 `trust.local`).
    TrustedLocal,
    /// Fetched and verified from a remote generation (`trust.synced`).
    SyncedLocal { generation: GenerationId },
    /// Served remotely, not yet materialised (`trust.remote`).
    Remote { generation: GenerationId },
    /// Last-known-good, served while offline (`trust.stale`).
    Stale {
        #[serde(serialize_with = "serialize_unix_secs")]
        #[schemars(with = "u64")]
        as_of: SystemTime,
    },
}

// ---------------------------------------------------------------------------
// KindTag  (§L2.2 substitution)
// ---------------------------------------------------------------------------

/// A forward-compatible kind label.
///
/// `Known` wraps the 13 frozen `KindDiscriminant` variants from `nudox-ir`.
/// `Unknown` carries the raw wire `u16` so future producers can add kinds
/// without breaking existing readers — they render as a visible chip, never
/// panic (LD-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KindTag {
    /// A kind the current binary understands.
    Known(KindDiscriminant),
    /// A kind added by a future producer; renders as an opaque chip.
    Unknown(u16),
}

impl KindTag {
    /// Decode a raw wire discriminant, falling back to `Unknown`.
    pub fn from_u16(v: u16) -> Self {
        match KindDiscriminant::from_u16(v) {
            Some(d) => Self::Known(d),
            None => Self::Unknown(v),
        }
    }
}

impl From<KindDiscriminant> for KindTag {
    fn from(d: KindDiscriminant) -> Self {
        Self::Known(d)
    }
}

impl Serialize for KindTag {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        match self {
            KindTag::Known(d) => {
                let mut st = s.serialize_struct("KindTag", 2)?;
                st.serialize_field("kind", "known")?;
                // `KindDiscriminant` is a fieldless enum deriving `serde::Serialize`,
                // so serde emits the variant name (e.g. "Module", "Function") as a
                // plain string. Going through `Serialize` rather than `Debug` keeps
                // the wire label on a format contract instead of on `Debug` output,
                // and avoids allocating a `String` per token.
                st.serialize_field("name", d)?;
                st.end()
            }
            KindTag::Unknown(raw) => {
                let mut st = s.serialize_struct("KindTag", 2)?;
                st.serialize_field("kind", "unknown")?;
                st.serialize_field("raw", raw)?;
                st.end()
            }
        }
    }
}

impl JsonSchema for KindTag {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("KindTag")
    }
    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "known" },
                        "name": { "type": "string" }
                    },
                    "required": ["kind", "name"]
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "unknown" },
                        "raw": { "type": "integer" }
                    },
                    "required": ["kind", "raw"]
                }
            ]
        })
    }
}

// ---------------------------------------------------------------------------
// Signature tokens  (§9.2 `SigToken`, LR-4)
// ---------------------------------------------------------------------------

/// A single typed token in a rendered signature.
///
/// `Ty` tokens are clickable in the GUI: `target` is `Some` exactly when the
/// underlying `Type::Nominal(RawRef)` resolves through the corpus (LR-4).
/// `Kw` and `Punct` carry `&'static str` to avoid any per-token allocation
/// on the hot path.
///
/// `Deserialize` is intentionally absent: `Kw` and `Punct` hold `&'static str`
/// which cannot be reconstructed from owned bytes at runtime.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SigToken {
    /// A language keyword (`fn`, `struct`, `impl`, …).
    Kw(&'static str),
    /// An identifier (symbol name, parameter name, …).
    Ident(SharedStr),
    /// A type reference, optionally linked to another symbol.
    Ty { text: SharedStr, target: Option<SymbolKey> },
    /// Punctuation (`(`, `)`, `,`, `->`, `<`, `>`, …).
    Punct(&'static str),
    /// A single whitespace separator — keeps rendering logic whitespace-aware.
    Ws,
    /// A generic parameter name (`T`, `K`, `V`, …).
    Generic(SharedStr),
    /// A lifetime name (`'a`, `'static`, …).
    Lifetime(SharedStr),
}

impl Serialize for SigToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        match self {
            SigToken::Kw(kw) => {
                let mut st = s.serialize_struct("SigToken", 2)?;
                st.serialize_field("kind", "kw")?;
                st.serialize_field("text", kw)?;
                st.end()
            }
            SigToken::Ident(id) => {
                let mut st = s.serialize_struct("SigToken", 2)?;
                st.serialize_field("kind", "ident")?;
                st.serialize_field("text", id)?;
                st.end()
            }
            SigToken::Ty { text, target } => {
                let mut st = s.serialize_struct("SigToken", 3)?;
                st.serialize_field("kind", "ty")?;
                st.serialize_field("text", text)?;
                let target_str: Option<String> = target.as_ref().map(|k| {
                    format!(
                        "{}:{}#{}",
                        k.package.ecosystem.as_str(),
                        k.package.name.as_str(),
                        k.intro.to_hex()
                    )
                });
                st.serialize_field("target", &target_str)?;
                st.end()
            }
            SigToken::Punct(p) => {
                let mut st = s.serialize_struct("SigToken", 2)?;
                st.serialize_field("kind", "punct")?;
                st.serialize_field("text", p)?;
                st.end()
            }
            SigToken::Ws => {
                let mut st = s.serialize_struct("SigToken", 1)?;
                st.serialize_field("kind", "ws")?;
                st.end()
            }
            SigToken::Generic(g) => {
                let mut st = s.serialize_struct("SigToken", 2)?;
                st.serialize_field("kind", "generic")?;
                st.serialize_field("text", g)?;
                st.end()
            }
            SigToken::Lifetime(l) => {
                let mut st = s.serialize_struct("SigToken", 2)?;
                st.serialize_field("kind", "lifetime")?;
                st.serialize_field("text", l)?;
                st.end()
            }
        }
    }
}

impl JsonSchema for SigToken {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SigToken")
    }
    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["kw", "ident", "ty", "punct", "ws", "generic", "lifetime"]
                }
            },
            "required": ["kind"]
        })
    }
}

// ---------------------------------------------------------------------------
// Breadcrumb
// ---------------------------------------------------------------------------

/// One crumb in the breadcrumb trail shown above a symbol page.
///
/// Each crumb is clickable: the GUI navigates to `key` when activated.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CrumbRef {
    /// The stable identity of this ancestor.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// The display label (typically the ancestor's short name).
    pub label: SharedStr,
}

// ---------------------------------------------------------------------------
// Section planning  (§9.2, §9.4)
// ---------------------------------------------------------------------------

/// The kind of content a planned section will contain.
///
/// Derived from the entry before any markdown is parsed so that the skeleton
/// geometry (§9.4) can be laid out before content arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SectionKind {
    /// Free-form prose (parsed markdown).
    Prose,
    /// A standalone fenced code block.
    CodeBlock,
    /// The entry's direct module/trait/impl children.
    Members,
    /// The entry's field and variant children.
    Fields,
    /// A prose section whose H2 heading matches `^Examples?$`.
    Examples,
    /// A blockquote with a `[!NOTE]`-style lead.
    Callout,
    /// A section kind added by a future producer.
    Unknown,
}

/// A geometry hint for skeleton pre-layout (§9.4).
///
/// `Lines(n)` → `n × line_height`; `Rows(n)` → `n × row_height`; `Unknown`
/// → 3-line block.  The numbers are advisory; the real section replaces the
/// skeleton at the same height class, so scroll position never teleports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SizeHint {
    /// Estimated line count (prose, code blocks).
    Lines(u32),
    /// Estimated row count (members, fields).
    Rows(u32),
    /// No estimate available.
    Unknown,
}

/// The planned presence and geometry of one streamed section.
///
/// Sent to the GUI in `SymbolHead::section_plan` before any `Section` events
/// arrive, so it can pre-lay the skeleton.  The chunker computes this from the
/// same walk that produces the actual sections (one-walk guarantee, §9.4).
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SectionPlan {
    /// Stable id that links this plan entry to its `RenderSection`.
    pub id: SectionId,
    /// What kind of section is expected.
    pub kind: SectionKind,
    /// Geometry hint for skeleton pre-layout.
    pub size_hint: SizeHint,
}

// ---------------------------------------------------------------------------
// Callout
// ---------------------------------------------------------------------------

/// The semantic level of a callout block (GitHub `[!NOTE]` / `[!WARNING]` …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CalloutLevel {
    /// Informational note.
    Note,
    /// Non-critical warning.
    Warning,
    /// Critical / destructive danger.
    Danger,
    /// Helpful tip.
    Tip,
    /// Unknown callout kind; render with a neutral style.
    Unknown,
}

// ---------------------------------------------------------------------------
// Language id
// ---------------------------------------------------------------------------

/// The info string on a fenced code block (`rust`, `python`, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LangId(pub SharedStr);

// ---------------------------------------------------------------------------
// Inline content
// ---------------------------------------------------------------------------

/// The target of a hyperlink inside rendered prose.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkTarget {
    /// A cross-reference to another symbol (renders as `page.handoff`).
    Symbol {
        #[serde(serialize_with = "serialize_symbol_key")]
        #[schemars(with = "String")]
        key: SymbolKey,
    },
    /// An external URL (opens in the system browser).
    Url { url: SharedStr },
}

/// A single run of inline text inside a [`ProseBlock`].
///
/// Kept flat (no nesting) so the GUI can render each run in a single pass
/// without a recursive element tree.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InlineRun {
    /// Plain text.
    Text { text: SharedStr },
    /// Inline code span (`` `foo` ``).
    Code { text: SharedStr },
    /// Strong/bold text.
    Strong { text: SharedStr },
    /// Emphasis/italic text.
    Em { text: SharedStr },
    /// A hyperlink.
    Link { text: SharedStr, target: LinkTarget },
}

// ---------------------------------------------------------------------------
// Prose blocks
// ---------------------------------------------------------------------------

/// A block-level element inside a [`RenderSection::Prose`] or similar.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProseBlock {
    /// A paragraph of inline runs.
    Paragraph { runs: Vec<InlineRun> },
    /// A heading inside a section (H1 only — H2+ splits into a new section).
    Heading { level: u8, runs: Vec<InlineRun> },
    /// An ordered or unordered list.
    List { ordered: bool, items: Vec<Vec<InlineRun>> },
    /// A thematic break (`---`).
    Rule,
    /// A fenced code block.
    Code { lang: LangId, text: SharedStr, line_count: u32 },
}

// ---------------------------------------------------------------------------
// Member / field rows
// ---------------------------------------------------------------------------

/// A summary row for one member inside a `Members` section.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct MemberRow {
    /// Stable identity of this member.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// Short display name.
    pub name: SharedStr,
    /// Pre-rendered signature tokens (LR-4: produced once by the chunker).
    pub sig: Vec<SigToken>,
    /// Kind tag for the badge.
    pub kind: KindTag,
    /// Visibility badge.
    #[schemars(with = "String")]
    pub visibility: Visibility,
}

/// A summary row for one field inside a `Fields` section.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct FieldRow {
    /// Stable identity of this field or variant.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// Short display name.
    pub name: SharedStr,
    /// Pre-rendered type tokens.
    pub ty_tokens: Vec<SigToken>,
    /// Kind tag (Field vs. Variant).
    pub kind: KindTag,
}

// ---------------------------------------------------------------------------
// RenderSection  (§9.2)
// ---------------------------------------------------------------------------

/// One fully-rendered section of a symbol's documentation page.
///
/// Emitted in `section_plan` order via `DocEvent::Section`.  The section id
/// links back to the corresponding `SectionPlan` so the GUI can swap the
/// skeleton for live content.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RenderSection {
    /// Free-form prose (parsed markdown).
    Prose { id: SectionId, blocks: Vec<ProseBlock> },
    /// A standalone fenced code block.
    CodeBlock { id: SectionId, lang: LangId, text: SharedStr, line_count: u32 },
    /// The entry's module / trait / impl children.
    Members { id: SectionId, entries: Arc<[MemberRow]> },
    /// The entry's field and variant children.
    Fields { id: SectionId, entries: Arc<[FieldRow]> },
    /// A prose section whose heading matches `^Examples?$`.
    Examples { id: SectionId, blocks: Vec<ProseBlock> },
    /// A callout blockquote (`[!NOTE]`, `[!WARNING]`, …).
    Callout { id: SectionId, level: CalloutLevel, blocks: Vec<ProseBlock> },
    /// A section kind this binary does not understand; renders as a chip.
    Unknown { id: SectionId, kind_tag: SharedStr },
}

impl RenderSection {
    /// The section id that links this section to its [`SectionPlan`].
    ///
    /// Used by the GUI to replace the correct skeleton slot and by `Highlight`
    /// events to target the right code block.
    pub fn section_id(&self) -> SectionId {
        match self {
            Self::Prose { id, .. }
            | Self::CodeBlock { id, .. }
            | Self::Members { id, .. }
            | Self::Fields { id, .. }
            | Self::Examples { id, .. }
            | Self::Callout { id, .. }
            | Self::Unknown { id, .. } => *id,
        }
    }
}

// ---------------------------------------------------------------------------
// Highlight spans  (async upgrade, §9.3)
// ---------------------------------------------------------------------------

/// A syntax-highlight span inside a `CodeBlock` section.
///
/// Emitted after the section via `DocEvent::Highlight` so the code block
/// appears immediately as monochrome text and upgrades in place — zero
/// geometry change (§9.4.2, `highlight.sweep`).
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct HighlightSpan {
    /// Byte offset of the start of this span in the code block's text.
    pub start: u32,
    /// Byte offset one past the end.
    pub end: u32,
    /// CSS-class-like token class (`keyword`, `string`, `comment`, …).
    pub class: SharedStr,
}

// ---------------------------------------------------------------------------
// Symbol head  (§9.2)
// ---------------------------------------------------------------------------

/// The header of a symbol page — always the first `DocEvent`.
///
/// Sent before any section arrives so the GUI can paint the breadcrumb,
/// signature, and skeleton immediately (< 50 ms local, §9.1).
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SymbolHead {
    /// Stable identity of this symbol (LR-1).
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// Ancestor chain, root-first, each crumb clickable.
    pub breadcrumb: Vec<CrumbRef>,
    /// Pre-rendered signature tokens (LR-4).
    pub signature: Vec<SigToken>,
    /// Kind tag for the badge and icon.
    pub kind: KindTag,
    /// Visibility for the access modifier label.
    #[schemars(with = "String")]
    pub visibility: Visibility,
    /// Trust provenance for the badge chrome (LD-8).
    pub provenance: Provenance,
    /// Deprecation message, if any.
    pub deprecation: Option<SharedStr>,
    /// What will stream, in order, plus geometry hints for skeleton pre-layout
    /// (§9.4).  Computed by the same walk that emits the sections — not
    /// estimated — so the skeleton is *derived* geometry, not a guess.
    pub section_plan: Vec<SectionPlan>,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// A single search result row — fully render-ready on arrival.
///
/// The GUI renders this with zero string work in `render()` (§1.1.4):
/// `sig_preview` is a pre-tokenised signature; `display_name` is already a
/// `SharedStr`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct HitRow {
    /// Stable identity for navigation.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// Display name (may include path prefix for disambiguation).
    pub display_name: SharedStr,
    /// Abbreviated signature preview (§12.3, LR-4).
    pub sig_preview: Vec<SigToken>,
    /// Kind tag for the badge.
    pub kind: KindTag,
    /// Trust provenance badge.
    pub provenance: Provenance,
    /// Relevance score (higher = more relevant).
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Refs / Impls pages
// ---------------------------------------------------------------------------

/// One reference in a `RefsPage`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct RefRow {
    /// The symbol that holds this reference.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub target: SymbolKey,
    /// Display path of the referencing symbol.
    pub path: SharedStr,
    /// Kind label for the precision badge.
    pub kind_tag: SharedStr,
}

/// A paged list of cross-references to this symbol.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct RefsPage {
    /// The references in this page.
    pub refs: Arc<[RefRow]>,
    /// Total reference count across all pages.
    pub total: u64,
}

/// One implementation in an `ImplsPage`.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ImplRow {
    /// The impl entry.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// Short display label (e.g. `impl Display for Point`).
    pub label: SharedStr,
}

/// A paged list of trait implementations for this symbol.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ImplsPage {
    /// The impls in this page.
    pub impls: Arc<[ImplRow]>,
    /// Total impl count across all pages.
    pub total: u64,
}

// ---------------------------------------------------------------------------
// QueryRow
// ---------------------------------------------------------------------------

/// One result row from a Trustfall graph query.
///
/// Column order is established by the preceding `QueryEvent::Columns` event.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct QueryRow {
    /// Cell values in column order.
    pub cells: Arc<[SharedStr]>,
}

// ---------------------------------------------------------------------------
// Error type  (§L7.4)
// ---------------------------------------------------------------------------

/// Engine-level errors that reach the GUI so `ErrorState` can branch on them
/// (LD-16).
///
/// `#[non_exhaustive]` so future error variants can be added without breaking
/// existing `match` arms in `lindsey`.
///
/// `Clone` is load-bearing rather than incidental: this error travels *inside*
/// the event enums (`DocEvent::Failed`, `SearchEvent::Failed`,
/// `QueryEvent::Failed`), which are `Clone` because one arriving event can land
/// in several places at once — a failed symbol load updates the tab, the nav
/// entry and the toast queue. Every variant therefore carries owned, cloneable
/// data and never a `#[source]` chain to a non-`Clone` cause; where an
/// underlying error exists it is flattened to a `String` at the boundary.
#[derive(Clone, Debug, thiserror::Error, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EngineError {
    #[error("package {package} is not loaded")]
    PackageNotLoaded {
        /// The lineage id that was not found in the loaded corpus.
        /// Serialised as `"ecosystem:name"` via `serialize_lineage_id`.
        #[serde(serialize_with = "serialize_lineage_id")]
        #[schemars(with = "String")]
        package: PackageLineageId,
    },

    #[error("symbol not found")]
    SymbolNotFound,

    #[error("chunker error: {message}")]
    Chunk {
        /// The underlying error message from the chunker.
        message: String,
    },

    #[error("stream cancelled")]
    Cancelled,
}

// ---------------------------------------------------------------------------
// Event enums  (§L2.3, §9.3)
// ---------------------------------------------------------------------------

/// The streamed documentation protocol for one symbol page.
///
/// Protocol invariants (property-tested in §9.3):
/// 1. `Head` precedes everything; `Done`/`Failed` terminate.
/// 2. `Section`s arrive in `section_plan` order.
/// 3. `Highlight` only references already-sent sections.
/// 4. Applying any prefix of a valid stream yields a valid page.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DocEvent {
    /// Symbol metadata + skeleton geometry — exactly once, always first.
    Head(Box<SymbolHead>),
    /// One section of content, in `section_plan` order.
    Section(RenderSection),
    /// Async syntax-highlight upgrade for a previously-sent code section.
    Highlight { section: SectionId, spans: Arc<[HighlightSpan]> },
    /// One page of cross-references (may interleave with `Section`s).
    Refs { page: RefsPage, done: bool },
    /// One page of trait implementations.
    Impls { page: ImplsPage, done: bool },
    /// Terminal — stream is complete.
    Done,
    /// Terminal — stream failed.
    Failed(EngineError),
}

/// Events for a search result stream.
///
/// Local name/type hits (`Section`) MUST be emitted before any semantic work
/// starts (LR-10) so that the first frame never waits for embeddings.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum SearchEvent {
    /// Initial results for a section.
    Section { generation: Gen, section: SearchSectionId, rows: Arc<[HitRow]> },
    /// Incremental merge/update for an existing section.
    Merge { generation: Gen, section: SearchSectionId, rows: Arc<[HitRow]> },
    /// Latency measurement for one section.
    Latency { generation: Gen, section: SearchSectionId, elapsed: Duration },
    /// Terminal — all sections are complete.
    Done { generation: Gen },
    /// Terminal — stream failed.
    Failed { generation: Gen, error: EngineError },
}

/// Events for a Trustfall graph query stream.
///
/// `Columns` precedes any `Rows` (invariant enforced by the query executor).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum QueryEvent {
    /// Column names, sent once before any rows.
    Columns { generation: Gen, columns: Arc<[SharedStr]> },
    /// A batch of result rows.
    Rows { generation: Gen, rows: Arc<[QueryRow]> },
    /// Terminal — all rows are complete.
    Done { generation: Gen, total: u64 },
    /// Terminal — query failed.
    Failed { generation: Gen, error: EngineError },
}

// ---------------------------------------------------------------------------
// Conversions from store provenance
// ---------------------------------------------------------------------------

impl From<nudox_store::package::Provenance> for Provenance {
    fn from(p: nudox_store::package::Provenance) -> Self {
        // `nudox_store::package::Provenance` is #[non_exhaustive]; match both
        // known variants and treat anything future as TrustedLocal (LR-10:
        // local is the truth, this is not the exception).
        match p {
            nudox_store::package::Provenance::TrustedLocal => Provenance::TrustedLocal,
            nudox_store::package::Provenance::SnapshotLocal => Provenance::TrustedLocal,
            _ => Provenance::TrustedLocal,
        }
    }
}
