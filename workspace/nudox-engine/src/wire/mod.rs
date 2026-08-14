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

// The typed record of an intra-doc link whose spelling we corrected. It lives
// in its own file because it is a *closed* vocabulary that several unrelated
// consumers (the GUI legend, the corpus audit) must match exhaustively; see
// its module docs for why a silent repair is the defect this exists to kill.
pub mod repair;
pub use repair::{LinkOrigin, LinkRepair, LinkRepairKind};

// Where a declaration was written. Its own module because it is a closed
// vocabulary two unrelated wire types (`SymbolHead`, `ImplRow`) both carry, and
// because the reason a location is *missing* is itself protocol — see its
// module docs.
pub mod source;
pub use source::{LineCol, SourceLocation, UnlocatedReason};

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
pub fn serialize_unix_secs<S: serde::Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
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
    Ty {
        text: SharedStr,
        target: Option<SymbolKey>,
    },
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
    ///
    /// `origin` is **not** optional and has no `Default`: you cannot emit a
    /// link into this protocol without answering whether its spelling was the
    /// author's or ours. That is the whole mechanism by which a repair cannot
    /// be silent — see [`repair`] for the reasoning.
    Link {
        text: SharedStr,
        target: LinkTarget,
        origin: LinkOrigin,
    },
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
    List {
        ordered: bool,
        items: Vec<Vec<InlineRun>>,
    },
    /// A thematic break (`---`).
    Rule,
    /// A fenced code block.
    Code {
        lang: LangId,
        text: SharedStr,
        line_count: u32,
    },
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
    Prose {
        id: SectionId,
        blocks: Vec<ProseBlock>,
    },
    /// A standalone fenced code block.
    CodeBlock {
        id: SectionId,
        lang: LangId,
        text: SharedStr,
        line_count: u32,
    },
    /// The entry's module / trait / impl children.
    Members {
        id: SectionId,
        entries: Arc<[MemberRow]>,
    },
    /// The entry's field and variant children.
    Fields {
        id: SectionId,
        entries: Arc<[FieldRow]>,
    },
    /// A prose section whose heading matches `^Examples?$`.
    Examples {
        id: SectionId,
        blocks: Vec<ProseBlock>,
    },
    /// A callout blockquote (`[!NOTE]`, `[!WARNING]`, …).
    Callout {
        id: SectionId,
        level: CalloutLevel,
        blocks: Vec<ProseBlock>,
    },
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

    // ── Cfg gating ─────────────────────────────────────────────────────────────
    //
    // Some symbols only exist under a particular feature flag or target — a
    // reader who cannot see that is one `cargo build` away from a confusing
    // "unresolved item" error. docs.rs renders this as a prominent "Available
    // on crate feature `x` only" badge on the item page; a symbol page that
    // renders nothing here is telling the reader less than the tool we claim
    // to beat.
    /// The `#[cfg(...)]` predicate gating this symbol, rendered exactly as
    /// Rust surface syntax (`cfg(feature = "std")`, `cfg(target_os =
    /// "windows")`, …) so a reader who knows Rust can read it directly, with
    /// no producer-specific vocabulary to learn.
    ///
    /// `None` means the symbol is unconditionally compiled in, *or* the
    /// producer did not analyse cfg attributes for this symbol — the two are
    /// indistinguishable on the wire today, mirroring `Symbol::cfg` in
    /// `nudox-ir`.
    pub cfg: Option<SharedStr>,

    /// What will stream, in order, plus geometry hints for skeleton pre-layout
    /// (§9.4).  Computed by the same walk that emits the sections — not
    /// estimated — so the skeleton is *derived* geometry, not a guess.
    pub section_plan: Vec<SectionPlan>,

    /// Where this symbol was written.
    ///
    /// Replaces the `source_path: Option<SharedStr>` /
    /// `source_span: Option<[u32; 2]>` pair, which could not express a
    /// navigable location and could not say why when it had none
    /// (`docs/LIMITATIONS.md` L31, L42.2). Only
    /// [`SourceLocation::Declared`](source::SourceLocation::Declared) may be
    /// rendered as a link; see
    /// [`SourceLocation::jump_target`](source::SourceLocation::jump_target).
    pub source: SourceLocation,
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
    /// `true` when `ImplFlags::blanket` is set — i.e. the impl applies to all
    /// types satisfying a bound (`impl<T: Bound> Trait for T`).
    ///
    /// Blanket impls are rendered in a separate collapsed subheading because
    /// they describe the ecosystem's structure, not the specific type being
    /// viewed (docs.rs pattern).
    pub is_blanket: bool,
    /// The trait being implemented, rendered as a plain string.
    ///
    /// `None` for inherent impls (`impl Foo { … }`).  Used to group variadic
    /// arity families: a 16-tuple Handler family all share the same
    /// `trait_label`.
    pub trait_label: Option<SharedStr>,
    /// Number of generic type arguments on the *self type* at the outermost
    /// `Type::Apply` level.
    ///
    /// `0` for a bare nominal self type (`impl Foo`), `N` for `impl Foo<T1,
    /// …, TN>`.  Groups with ≥3 consecutive values of this field collapse to a
    /// single arity-range summary row in the GUI.
    pub self_generic_count: u32,

    /// Where the `impl` block was written.
    ///
    /// `ImplRow` previously carried `{key, label, is_blanket, trait_label,
    /// self_generic_count}` and no location at all, so a per-impl source link
    /// had nothing to point at (`docs/LIMITATIONS.md` L42.3) and the GUI's only
    /// recourse was to navigate to the impl's own symbol page and read the
    /// head's location from there — two steps for what docs.rs does in one.
    ///
    /// This is the location of the `impl … { }` header, not of any member
    /// inside it: an impl block is the thing the row names, and the members
    /// have their own entries.
    pub source: SourceLocation,
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
// Versions  (multi-generation corpus, §L2.4)
// ---------------------------------------------------------------------------

/// One loaded generation of a package.
///
/// The corpus can hold several versions of the same `PackageLineageId` — that
/// is the whole point of `PackageLineageId` being version-free (it names the
/// *lineage*, not one release). `VersionRow` is the GUI-facing projection of
/// one of those generations: enough to fill a dropdown item and no more.
///
/// `symbol_count` is the declaration-table size of *that* generation, not of
/// the lineage. Two rows for the same package routinely disagree, and the
/// difference is itself informative (a version that suddenly halves its symbol
/// count is usually a producer failure, not an API purge).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VersionRow {
    /// The version string exactly as the caller supplied it in
    /// [`crate::PackageVersionSpec::version`].
    ///
    /// Deliberately *not* normalised. The engine parses a copy of it for
    /// ordering (see `crate::versions`), but what the GUI shows is the string
    /// the user typed, because a dropdown that silently rewrites `1.2` to
    /// `1.2.0` is a dropdown the user cannot match against their manifest.
    pub version: SharedStr,
    /// True for exactly one row: the *selected* generation.
    ///
    /// Every version-free path in the engine — `open_symbol`, `search`,
    /// `query` — resolves against this generation, because `Corpus` is keyed
    /// by lineage alone and therefore has room for exactly one resident
    /// `PackageView` per package. Switching it is
    /// [`crate::EngineHandle::select_version`].
    ///
    /// "Selected" rather than "resident" because the two can disagree for the
    /// brief window between a `select_version` call and its
    /// [`VersionEvent::Switched`]: the selection is recorded synchronously, the
    /// corpus write is not. `select_version` documents why that ordering is the
    /// right one for a caller driving a dropdown.
    pub is_current: bool,
    /// Number of entries in this generation's declaration table.
    pub symbol_count: u64,
}

/// Every loaded generation of one package, newest first.
///
/// Returned by [`crate::EngineHandle::versions`]. Newest-first because that is
/// the order a dropdown wants: the release the user most likely means is the
/// one under the cursor when the list opens.
///
/// An empty `versions` list means the package is not loaded at all — it is not
/// an error, and it is distinguishable from "loaded, one version" by `len()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VersionList {
    /// The lineage these versions belong to.
    #[serde(serialize_with = "serialize_lineage_id")]
    #[schemars(with = "String")]
    pub package: PackageLineageId,
    /// The loaded generations, newest first.  See `crate::versions` for the
    /// exact ordering rule (semver-shaped where parseable, load order to break
    /// ties, unparseable strings sorted below all parseable ones).
    pub versions: Arc<[VersionRow]>,
}

impl VersionList {
    /// The generation the corpus currently serves, if any package is loaded.
    pub fn current(&self) -> Option<&VersionRow> {
        self.versions.iter().find(|v| v.is_current)
    }

    /// The number of loaded generations.
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// True when no generation of this package is loaded.
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// True if `version` names a loaded generation.
    pub fn contains(&self, version: &str) -> bool {
        self.versions.iter().any(|v| &*v.version == version)
    }
}

/// The outcome of a [`crate::EngineHandle::select_version`] request.
///
/// Exactly one of these is ever sent on the returned receiver, which then
/// closes. It is an event rather than a return value because repointing the
/// corpus takes the corpus write lock, which is `async`; the GUI cannot block
/// on that from a render pass.
///
/// The `generation` is the `Gen` the caller passed in, echoed back so a GUI
/// that has already moved on can drop a stale switch without applying it
/// (§9.3).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum VersionEvent {
    /// The corpus now serves `version` for `package`.
    ///
    /// Every open stream targeting this package is now answering against the
    /// *old* generation and must be re-issued under a fresh `Gen`.
    Switched {
        /// The generation this switch answers.
        generation: Gen,
        /// The package whose resident generation changed.
        package: PackageLineageId,
        /// The version now resident in the corpus.
        version: SharedStr,
    },
    /// The requested version is not loaded; the corpus is unchanged.
    ///
    /// Not an error — the engine only holds what it was asked to load, and a
    /// GUI may legitimately ask for a version it saw in a manifest but never
    /// requested.
    NotLoaded {
        /// The generation this answer belongs to.
        generation: Gen,
        /// The package that was asked about.
        package: PackageLineageId,
        /// The version that was requested and is absent.
        version: SharedStr,
    },
}

// ---------------------------------------------------------------------------
// Timeline  (§L2.5 — a symbol's history, keyed on IntroId)
// ---------------------------------------------------------------------------

/// What happened to a symbol in one version of its package.
///
/// # Why `Present` exists alongside `Introduced`
///
/// This distinction is the whole honesty budget of the feature. A timeline is
/// computed by walking the versions the corpus *happens to hold*, which is
/// almost never the package's full release history. If a symbol appears in the
/// oldest loaded version, the engine has no evidence about whether it was born
/// there or has existed for twenty releases — so it reports [`Self::Present`],
/// which claims only what is observed.
///
/// [`Self::Introduced`] is reserved for the case where the engine has positive
/// evidence: the symbol is *absent* from at least one older loaded version and
/// present here. That is a real introduction, and it is the only case in which
/// the word is used.
///
/// The single-version corpus therefore produces exactly one row, and that row
/// says `Present` — never `Introduced`, never an empty list.
///
/// # Why renames are their own variant
///
/// `IntroId` is assigned once, at first insertion, and a rename never changes
/// it (K18). So a rename is visible here as "same `IntroId`, different
/// `Symbol::name`" — the one classification that would be impossible with any
/// name-keyed history scheme, and the reason the timeline is keyed on
/// `IntroId` rather than on a path string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimelineChange {
    /// The symbol is present in the oldest loaded version.
    ///
    /// No claim is made about when it was introduced — see the type docs.
    Present,
    /// The symbol is absent from an older loaded version and present here.
    ///
    /// This is an observed introduction, not an inferred one.
    Introduced,
    /// Same `IntroId`, different name — the symbol was renamed (K18).
    Renamed {
        /// The name it carried in the previous version it appeared in.
        from: SharedStr,
    },
    /// Newly carries a deprecation marker.
    Deprecated,
    /// A previously-present deprecation marker was removed.
    Undeprecated,
    /// The rendered signature differs from the previous version it appeared in.
    SignatureChanged,
    /// The signature is identical but the visibility modifier changed.
    VisibilityChanged,
    /// Only the doc comment changed.
    DocsChanged,
    /// Nothing the engine can observe changed.
    Unchanged,
    /// Present in an earlier loaded version, absent here.
    ///
    /// The row still exists because "it is gone" is the most load-bearing fact
    /// a timeline can carry.
    Removed,
}

/// One point in a symbol's history: what it looked like in one version.
///
/// `signature` is produced by the same `chunk::signature::tokens` that builds
/// [`SymbolHead::signature`] (LR-4: signatures are rendered exactly once, in
/// one place), so a timeline row and a symbol page cannot disagree about what
/// a declaration looks like.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct TimelineRow {
    /// The package version this row describes.
    pub version: SharedStr,
    /// The headline classification for this version.
    ///
    /// At most one classification is reported per row even when several apply
    /// (a rename that also changes the signature, say). The priority order is
    /// `Removed` > `Introduced`/`Present` > `Renamed` > `Deprecated` >
    /// `Undeprecated` > `SignatureChanged` > `VisibilityChanged` >
    /// `DocsChanged` > `Unchanged`, i.e. rarest-and-most-consequential first.
    /// The other fields on this row remain authoritative regardless of which
    /// label won, so a GUI that wants to show "renamed *and* deprecated" can
    /// diff `name` and `deprecated` against the adjacent row itself.
    pub change: TimelineChange,
    /// The symbol's name in this version.  Empty for [`TimelineChange::Removed`].
    pub name: SharedStr,
    /// The rendered signature in this version.
    ///
    /// Empty for [`TimelineChange::Removed`] — there is no declaration left to
    /// render, and an empty token list is the honest representation of that.
    pub sig: Vec<SigToken>,
    /// Whether the symbol carries a deprecation marker in this version.
    ///
    /// This is *state*, not change: it stays `true` for every version after a
    /// deprecation lands, while `change` reports `Deprecated` only on the
    /// version where it first appeared.
    pub deprecated: bool,
    /// True for the version the corpus currently serves (`VersionRow::is_current`).
    pub is_current: bool,
}

/// A symbol's history across every loaded version of its package.
///
/// # What this is keyed on
///
/// `IntroId`. Two lowerings of the same package at different versions share
/// `IntroId`s for every symbol that persisted, so "the versions in which this
/// `IntroId` appears" *is* the symbol's timeline — no side table, no parallel
/// history structure, no name matching.
///
/// # What the one-version case looks like
///
/// Exactly one row, classified [`TimelineChange::Present`]. The GUI should
/// render it as "present in 0.8.9", not "introduced in 0.8.9": with a single
/// generation loaded the engine has no evidence about introduction.
///
/// `rows` is never empty for a symbol that exists — if the package is loaded
/// at all, the symbol appears in at least the generation the page was opened
/// against.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Timeline {
    /// The symbol this timeline describes.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// One row per loaded version in which the symbol was present, plus a
    /// trailing [`TimelineChange::Removed`] row for each version after it
    /// disappeared.  Newest first, matching [`VersionList::versions`].
    pub rows: Arc<[TimelineRow]>,
    /// How many generations of this package were examined to build `rows`.
    ///
    /// `rows.len()` can be smaller (a symbol added in 0.3 has no rows for 0.1
    /// or 0.2). Carrying the denominator lets the GUI say "3 of 5 versions"
    /// instead of implying the corpus only ever held three.
    pub versions_examined: u32,
}

// ---------------------------------------------------------------------------
// PackageDiff
// ---------------------------------------------------------------------------

/// Which disambiguator tier minted a key, as it crosses the wire.
///
/// A faithful projection of `nudox_ir::package::KeyTier` plus the one state
/// that type deliberately cannot represent: `Unrecorded`, meaning the
/// `PackageView` carries no `SealReport` and nothing is known. The IR type has
/// no such variant because at the point sealing runs the answer is always
/// known; the absence appears only downstream, and flattening it into
/// `Structural` here would tell a caller its key is sound on a corpus that
/// never looked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyTierLabel {
    /// Minted from the declaration's own content. Moves only when that
    /// content moves.
    Structural,
    /// Minted from the declaration's byte offsets. An edit *above* the
    /// declaration moves it.
    Span,
    /// Minted from the declaration's index among colliding siblings. A
    /// producer reordering its output moves it.
    Ordinal,
    /// This package's view carries no seal report; nothing is known.
    Unrecorded,
}

impl KeyTierLabel {
    /// Whether a key at this tier is a function of the declaration's content
    /// alone. `None` for [`KeyTierLabel::Unrecorded`] — the question is
    /// unanswered, not answered "no".
    pub fn is_content_derived(self) -> Option<bool> {
        match self {
            KeyTierLabel::Structural => Some(true),
            KeyTierLabel::Span | KeyTierLabel::Ordinal => Some(false),
            KeyTierLabel::Unrecorded => None,
        }
    }
}

/// What happened to one declaration between two generations of a package.
///
/// # Why `Removed` is not simply "absent from the newer generation"
///
/// The diff joins on `IntroId`, and an `IntroId` is not stable for every
/// declaration: two of the sealer's disambiguator tiers embed a byte span or
/// an ordinal, so a declaration that changed neither its name, its path, nor
/// its signature can still be minted a different key in the next release.
/// Measured on the provisioned corpus: 32,339 order-dependent groups across 49
/// of 79 packages, and 24 declarations across log/memchr/jackson-databind/
/// lodash that provably changed their published key between real releases.
///
/// A set-difference diff reports every one of those as a removal *and* an
/// addition — two false rows apiece, and the false removal is the damaging
/// one, because "this API was deleted" is exactly the kind of claim a reader
/// acts on. This enum exists so that case has its own name
/// ([`Self::Rekeyed`]), and so that the case we cannot resolve has one too
/// ([`Self::Indeterminate`]) instead of borrowing `Removed`'s.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiffVerdict {
    /// Present in the newer generation, and no declaration in the older one
    /// occupies its `(kind, path)`.
    Added,

    /// Present in the older generation, absent from the newer one, **and** its
    /// key was content-derived — so its absence is evidence about the
    /// declaration rather than about the key.
    ///
    /// This is the only verdict that asserts a deletion, and it is reachable
    /// only at [`KeyTierLabel::Structural`].
    Removed,

    /// One declaration, two keys. Present in both generations at the same
    /// `(kind, path)`, but the `IntroId` moved.
    ///
    /// Read `from_tier`/`to_tier` to know what the move means:
    ///
    /// * either side `Span` or `Ordinal` — the key may have moved with **no
    ///   change to the declaration at all**. This is the case that makes a
    ///   naive diff lie.
    /// * both `Structural` — the key moved because the declaration's own
    ///   identity-bearing content moved: an overload's signature, or an impl's
    ///   `(trait, self, generics, wheres)` skeleton. A real change, correctly
    ///   reported as one declaration rather than two.
    Rekeyed {
        /// The key it had in the older generation. Stop caching this one.
        #[serde(serialize_with = "serialize_symbol_key")]
        #[schemars(with = "String")]
        from_key: SymbolKey,
        /// The key it has in the newer generation.
        #[serde(serialize_with = "serialize_symbol_key")]
        #[schemars(with = "String")]
        to_key: SymbolKey,
        /// The tier that minted `from_key`.
        from_tier: KeyTierLabel,
        /// The tier that minted `to_key`.
        to_tier: KeyTierLabel,
    },

    /// Present in both generations under the same key, with one observable
    /// axis differing.
    ///
    /// The axis is a [`TimelineChange`] produced by the *same* classifier
    /// `get_symbol`'s timeline uses, so the two surfaces cannot describe one
    /// declaration differently.
    Changed {
        /// Which axis moved.
        change: TimelineChange,
    },

    /// Present in the older generation, absent from the newer one, and we
    /// **cannot say** whether it was deleted.
    ///
    /// Either its key was not content-derived (so its disappearance is equally
    /// consistent with an unrelated edit having moved it) and no unambiguous
    /// re-pairing was found, or the package carries no seal report at all so
    /// no tier is known.
    ///
    /// Returning this instead of `Removed` is the whole reason this diff is
    /// worth having: a diff that silently reports a key-churned declaration as
    /// deleted is worse than no diff, because the reader has no way to
    /// discount it.
    Indeterminate {
        /// The tier of the key that vanished — the evidence for why this is
        /// unanswerable.
        tier: KeyTierLabel,
    },
}

/// One declaration's row in a [`PackageDiff`].
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct DiffRow {
    /// The declaration's key in whichever generation it exists in — the newer
    /// one when it exists there, otherwise the older one.
    ///
    /// For [`DiffVerdict::Rekeyed`] this is `to_key`, and `from_key` is on the
    /// verdict: a caller navigating the diff wants the key that resolves
    /// *now*, and the stale one is the thing it should evict.
    #[serde(serialize_with = "serialize_symbol_key")]
    #[schemars(with = "String")]
    pub key: SymbolKey,
    /// The declaration's fully-qualified path, root-first and dot-joined.
    ///
    /// The join axis for [`DiffVerdict::Rekeyed`], and the only identity in
    /// this row a human can read.
    pub path: SharedStr,
    /// Its unqualified name in the generation `key` refers to.
    pub name: SharedStr,
    /// Its kind label (`Function`, `Record`, `Impl`, …).
    pub kind: SharedStr,
    /// What happened.
    pub verdict: DiffVerdict,
}

/// Two generations of one package, compared declaration by declaration.
///
/// # Why this is a computed answer and not a graph vertex
///
/// `nudox-graph` resolves against `nudox_store::corpus::Corpus`, which holds
/// exactly one resident `PackageView` per lineage — deliberately, because
/// `PackageLineageId` is version-free and that is what makes `IntroId`
/// continuity mean anything. The other generations live in the engine's
/// `VersionRegistry`, which sits *above* the graph in the dependency law
/// (`nudox-engine` → `nudox-graph`). A lineage-diff vertex would therefore
/// have required either inverting that edge or making the corpus
/// multi-resident — and multi-residency would change what every existing query
/// means, because "the world as it currently is" would stop having one answer.
///
/// Computing it here costs nothing extra in memory: both generations are
/// already resident in the registry. That is what the registry is for.
///
/// # Counts are derived, never accumulated
///
/// Every count below is computed from `rows` (doctrine §8). A tally kept
/// beside the rows it describes can drift from them; one computed from them
/// cannot.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PackageDiff {
    /// The lineage being diffed.
    #[serde(serialize_with = "serialize_lineage_id")]
    #[schemars(with = "String")]
    pub package: PackageLineageId,
    /// The older generation's version string.
    pub from_version: SharedStr,
    /// The newer generation's version string.
    pub to_version: SharedStr,
    /// One row per declaration that is not identical across the two
    /// generations, ordered by `(path, name, key)` so two runs are comparable.
    ///
    /// Declarations that exist in both and differ on no observable axis are
    /// **not** rows; they are counted in `unchanged`.
    pub rows: Arc<[DiffRow]>,
    /// How many declarations exist in both generations under the same key with
    /// nothing observable changed.
    pub unchanged: u32,
    /// How many declarations the older generation held.
    pub from_symbol_count: u32,
    /// How many declarations the newer generation holds.
    pub to_symbol_count: u32,
}

impl PackageDiff {
    /// How many rows satisfy `predicate`, derived from `rows`.
    ///
    /// Takes a predicate rather than publishing five parallel count fields,
    /// because five fields is five things that can disagree with the rows they
    /// summarise.
    pub fn count(&self, predicate: impl Fn(&DiffVerdict) -> bool) -> usize {
        self.rows.iter().filter(|r| predicate(&r.verdict)).count()
    }

    /// How many rows this diff could not resolve — the honesty number.
    ///
    /// A non-zero value is not a failure; it is the count of declarations
    /// whose disappearance the key scheme cannot explain. It rises with the
    /// number of `Span`/`Ordinal` keys the producer was forced to mint, so it
    /// is also a producer-quality signal.
    pub fn indeterminate(&self) -> usize {
        self.count(|v| matches!(v, DiffVerdict::Indeterminate { .. }))
    }
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

/// The disambiguator tier that minted a `SymbolKey`'s `IntroId`.
///
/// Mirrors `Symbol.keyTier` in `schema.graphql` field-for-field (same three
/// names) so an agent that has already learned this vocabulary from a
/// `graph_query` result recognises it here rather than parsing a fourth
/// spelling. `Unrecorded` has no variant here — see [`KeyStaleness::tier`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum KeyTierName {
    /// Minted from the declaration's own content. The common case.
    Structural,
    /// Minted from byte offsets; an unrelated edit above the declaration
    /// moves it.
    Span,
    /// Minted from the declaration's index among colliding siblings; a
    /// producer reordering its output moves it.
    Ordinal,
}

impl From<nudox_ir::package::KeyTier> for KeyTierName {
    fn from(tier: nudox_ir::package::KeyTier) -> Self {
        match tier {
            nudox_ir::package::KeyTier::Structural => Self::Structural,
            nudox_ir::package::KeyTier::Span => Self::Span,
            nudox_ir::package::KeyTier::Ordinal => Self::Ordinal,
        }
    }
}

impl fmt::Display for KeyTierName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Structural => f.write_str("Structural"),
            Self::Span => f.write_str("Span"),
            Self::Ordinal => f.write_str("Ordinal"),
        }
    }
}

/// Evidence that a [`SymbolKey`] which failed to resolve in the *current*
/// generation is not necessarily deleted — it resolved in some other loaded
/// generation of the same lineage.
///
/// # The defect this closes
///
/// A key that stops resolving after `select_version` was previously
/// indistinguishable from a deleted symbol: [`EngineError::SymbolNotFound`]
/// carried no fields at all. `graph_query` could already answer "how fragile
/// is this key" via `Symbol.keyTier` *while the key still resolved*, but by
/// the time a lookup actually failed there was no vertex left to ask. This
/// type is the same answer, reached the only way still possible after the
/// fact: by checking whether the key resolves under a *different* loaded
/// generation of the same package and reporting the tier it was minted at
/// there.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct KeyStaleness {
    /// A loaded generation (not necessarily the current one) of the same
    /// package lineage in which this exact key *does* resolve. Pass this to
    /// `select_version` to see it, or to `diff_versions` alongside the
    /// current version to see what it became.
    pub seen_in_version: SharedStr,
    /// The disambiguator tier that minted the key in `seen_in_version`, or
    /// `None` when that generation carries no seal report (`Unrecorded` —
    /// see `KeyProvenance`). `Some(KeyTierName::Structural)` here is still
    /// informative: it means the key is not supposed to move, so its absence
    /// from the current generation is more likely a real deletion or rename
    /// than a disambiguator collision — `diff_versions` is the next call to
    /// make either way.
    pub tier: Option<KeyTierName>,
}

/// A `(line, column)` position inside a `graph_query` query string, both
/// 1-based to match every editor's own numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct QueryErrorPosition {
    /// 1-based line number.
    pub line: u64,
    /// 1-based column number.
    pub column: u64,
}

impl fmt::Display for QueryErrorPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

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
        /// `Some(reason)` when a load for this exact lineage was attempted
        /// and failed — distinguishing "we tried and it broke" from "we
        /// never saw this name at all". `None` covers both "never
        /// requested" and "does not exist"; the engine has no external
        /// registry to tell those two apart, so it does not pretend to.
        /// Call `list_packages` either way to see what *is* loaded.
        attempted: Option<SharedStr>,
    },

    #[error("symbol not found")]
    SymbolNotFound {
        /// Set when this key resolves under a different loaded generation
        /// of the same package — see [`KeyStaleness`]. `None` means either
        /// the key never existed, the package has only one generation
        /// loaded, or no other loaded generation has ever seen it either.
        possibly_stale: Option<KeyStaleness>,
    },

    #[error("chunker error: {message}")]
    Chunk {
        /// The underlying error message from the chunker.
        message: String,
    },

    /// The Trustfall query text failed to parse, failed schema validation, or
    /// failed while executing (a malformed `$variable` binding, an edge
    /// parameter of the wrong type, etc).
    ///
    /// Distinct from [`Self::Chunk`]: a graph query is caller-supplied text
    /// that can be syntactically or semantically wrong, and calling that a
    /// "chunker error" (the previous behaviour — every query-plane failure
    /// was reported through `Chunk`, whose own message names an entirely
    /// different subsystem, the doc-page renderer) sent an agent looking at
    /// the wrong half of the codebase for something it did not break.
    #[error("graph query failed: {message}")]
    GraphQueryFailed {
        /// The parser/validator/executor's own message.
        message: String,
        /// Where in `query` the problem was found, when the underlying
        /// parser reported one. Most syntax errors do; schema-validation
        /// errors (an unknown field name, an incompatible filter) and
        /// execution-time errors (a resolver rejecting `$variable`'s value)
        /// do not, because by then there is no single token to blame.
        position: Option<QueryErrorPosition>,
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
/// 5. `Timeline` arrives at most once, after `Head` and before the first
///    `Section` (see the variant docs for why it is not a `Section`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum DocEvent {
    /// Symbol metadata + skeleton geometry — exactly once, always first.
    Head(Box<SymbolHead>),
    /// The symbol's history across the loaded versions of its package.
    ///
    /// # Why this is not a `RenderSection`
    ///
    /// Sections are the vertical flow of the page and are enumerated in
    /// `SymbolHead::section_plan` so the GUI can pre-lay their skeleton (§9.4).
    /// The timeline is a *tab*, not a band in that flow — giving it a
    /// `SectionId` would reserve geometry in a column it never occupies and
    /// would make `section_plan` disagree with what actually renders there.
    ///
    /// # Ordering
    ///
    /// Emitted immediately after `Head`, before any `Section`. Invariant 2
    /// (sections arrive in `section_plan` order) is about `Section` events
    /// relative to one another and is untouched by this. Emitting early rather
    /// than at the end means the Timeline tab is already populated the first
    /// time the user clicks it, instead of being the one tab that makes them
    /// wait for the body of a page they are not reading.
    ///
    /// Sent unconditionally whenever the symbol resolves — including when only
    /// one version of the package is loaded, in which case it carries exactly
    /// one row. See [`Timeline`] for what that row claims and does not claim.
    Timeline(Timeline),
    /// One section of content, in `section_plan` order.
    Section(RenderSection),
    /// Async syntax-highlight upgrade for a previously-sent code section.
    Highlight {
        section: SectionId,
        spans: Arc<[HighlightSpan]>,
    },
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
    Section {
        generation: Gen,
        section: SearchSectionId,
        rows: Arc<[HitRow]>,
    },
    /// Incremental merge/update for an existing section.
    Merge {
        generation: Gen,
        section: SearchSectionId,
        rows: Arc<[HitRow]>,
    },
    /// Latency measurement for one section.
    Latency {
        generation: Gen,
        section: SearchSectionId,
        elapsed: Duration,
    },
    /// What one section's rows *mean* — complete, partial, or not run at all.
    ///
    /// # Why an empty `Section` was not enough
    ///
    /// `Section { rows: [] }` is a claim: it says "we searched and found
    /// nothing". For the semantic section that claim was false in two distinct
    /// ways — the index may still be building over the corpus, or there may be
    /// no embedder in this build at all — and a consumer had no way to tell
    /// either from a genuine zero-hit answer. The GUI's `SectionStatus` flips
    /// to `Ready` on any `Section` event, so this was not a rendering bug to be
    /// fixed above the wire; the information was simply absent from it.
    ///
    /// Emitted **before** the section's `Section` event, so a consumer that
    /// renders on first paint already knows how to caption the rows it is about
    /// to receive. Sent for every section on every query, including
    /// [`SectionState::Complete`] for the local ones — a state that is only
    /// sent when it is interesting is a state a consumer has to infer the
    /// absence of.
    SectionState {
        generation: Gen,
        section: SearchSectionId,
        state: crate::semantic::SectionState,
    },
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
    Columns {
        generation: Gen,
        columns: Arc<[SharedStr]>,
    },
    /// A batch of result rows.
    Rows {
        generation: Gen,
        rows: Arc<[QueryRow]>,
    },
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
