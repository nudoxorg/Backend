//! `SearchModel` — render-ready state types for the omni-search overlay.
//!
//! These types live in the store layer (§12: stores hold render-ready state).
//! The view imports them from here; the store constructs and owns them.
//! Moving them out of `views/omni_search.rs` closes the backwards dependency
//! that forced wire→UI conversion to live in the view.

use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, SharedString};

use crate::theme::ext::Provenance;
use crate::theme::kind::LocalKindDiscriminant;
use crate::ui::signature_line::{SigToken as UiSigToken, SymbolKey as UiSymbolKey};
use heart::surface::{Located, Residence, SigToken as HeartSigToken, SymbolHit};
use heart::{Scored, StableReference, SymbolKind};
use nudox_engine::wire::{Provenance as WireProvenance, SymbolKey};

// ─────────────────────────────────────────────────────────────────────────────
// Section constants
// ─────────────────────────────────────────────────────────────────────────────

/// The three fused result sections (§15).
///
/// Frozen at three: `cmd-1/2/3` jump to them by ordinal, and `SearchResults`
/// in §12.3 is `[SectionData; 3]`.
pub const SECTION_COUNT: usize = 3;

// ─────────────────────────────────────────────────────────────────────────────
// Section enum
// ─────────────────────────────────────────────────────────────────────────────

/// Which of the three §15 sections a row or cursor belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// Lexical name matches — local, fastest.
    Name,
    /// Type-signature matches — local.
    Type,
    /// Embedding / semantic matches — may arrive 20–200 ms later.
    Semantic,
}

impl Section {
    /// All three, in display order.
    pub const ALL: [Section; SECTION_COUNT] = [Section::Name, Section::Type, Section::Semantic];

    /// Zero-based display index, used to key the fixed-size arrays.
    pub fn index(self) -> usize {
        match self {
            Section::Name => 0,
            Section::Type => 1,
            Section::Semantic => 2,
        }
    }

    /// Recover a section from its display index.
    pub fn from_index(ix: usize) -> Option<Section> {
        Section::ALL.get(ix).copied()
    }

    /// The sticky caption header label. `SectionHeader` upper-cases it.
    pub fn caption(self) -> &'static str {
        match self {
            Section::Name => "Name",
            Section::Type => "Type",
            Section::Semantic => "Semantic",
        }
    }

    /// Stable `ElementId` namespace for this section's row entrances (LD-19).
    pub fn entrance_namespace(self) -> &'static str {
        match self {
            Section::Name => "search.row.enter.name",
            Section::Type => "search.row.enter.type",
            Section::Semantic => "search.row.enter.semantic",
        }
    }

    /// Stable `ElementId` for this section's list (LD-19).
    pub fn list_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.list.name",
            Section::Type => "search.results.list.type",
            Section::Semantic => "search.results.list.semantic",
        }
    }

    /// Stable `ElementId` namespace for this section's rows (LD-19).
    pub fn row_id(self) -> &'static str {
        match self {
            Section::Name => "search.results.row.name",
            Section::Type => "search.results.row.type",
            Section::Semantic => "search.results.row.semantic",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cursor
// ─────────────────────────────────────────────────────────────────────────────

/// A position in the fused result set: which section, and which row inside it.
///
/// Deliberately *not* a flat index. §15 requires that selection movement never
/// wraps silently across sections, and a flat index makes that invariant
/// invisible — you cannot tell from `7` whether the next step leaves a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Display index of the section (see [`Section::index`]).
    pub section: usize,
    /// Row index within that section.
    pub row: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchMode
// ─────────────────────────────────────────────────────────────────────────────

/// The mode chip selection on the input row (§15 layout, §12.3 `mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Route the query by shape: `->` implies type search, prose implies semantic.
    #[default]
    Auto,
    /// Force lexical name search.
    Name,
    /// Force type-signature search.
    Type,
    /// Force semantic search.
    Semantic,
}

impl SearchMode {
    /// All four, in chip order.
    pub const ALL: [SearchMode; 4] = [
        SearchMode::Auto,
        SearchMode::Name,
        SearchMode::Type,
        SearchMode::Semantic,
    ];

    /// Chip label. `&'static str`, so rendering a chip allocates nothing.
    pub fn label(self) -> &'static str {
        match self {
            SearchMode::Auto => "Auto",
            SearchMode::Name => "Name",
            SearchMode::Type => "Type",
            SearchMode::Semantic => "Semantic",
        }
    }

    /// Whether `section` is shown while this mode is selected.
    ///
    /// # Why the mode is a *presentation* filter and not a query parameter
    ///
    /// It would be better if it were a query parameter, and it is not one:
    /// `nudox_engine::SearchQuery` carries `{ text, kinds, limit }` and no mode
    /// field, so `SearchStore`'s bridge has nowhere to put it (see the `_mode`
    /// argument on `SearchEngine::search`). The engine always fans out to all
    /// three sections and always answers the semantic one with an empty batch.
    ///
    /// Given that, the chips had exactly two possible honest meanings: do
    /// nothing at all (what they did — four chips, drawn since the first
    /// screenshot run, that no frame ever showed doing anything), or scope
    /// which of the three fused sections the reader is looking at. This is the
    /// second. It is real, it is visible, and it does not pretend the engine
    /// was asked a different question than it was.
    pub fn shows(self, section: Section) -> bool {
        match self {
            SearchMode::Auto => true,
            SearchMode::Name => section == Section::Name,
            SearchMode::Type => section == Section::Type,
            SearchMode::Semantic => section == Section::Semantic,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ScopeChip
// ─────────────────────────────────────────────────────────────────────────────

/// One scope chip (dep-set / kind / language / package filter, §12.3 `scope`).
///
/// The label is pre-computed by the store; this view never formats one.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeChip {
    /// Pre-computed label (e.g. `"this project"`, `"rust"`).
    pub label: SharedString,
    /// Whether the filter is currently applied.
    pub active: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// PreparedRow — the render-ready hit
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// RowQualifier — the reason a row shows (or does not show) a module path
// ─────────────────────────────────────────────────────────────────────────────

/// Why a rendered hit row shows the module path it shows.
///
/// # Why this is a field and not a rendering decision
///
/// Seven consecutive search rows once rendered as exactly `memchr` over
/// `mod memchr`, with identical badges, because the row template drew only the
/// leaf name and the leaf name was all the row carried
/// (GUI-WORKORDER-2 F1 / docs/LIMITATIONS.md L20). Adding "also draw the path" to
/// the template would have fixed that frame and left the *type* able to
/// express an ambiguous row, so the next section, the next mode, and the next
/// view would each have to remember. Instead a row cannot be built at all
/// except through [`PreparedRow::prepare`], which sees the whole delivered
/// batch and therefore is the only thing in the program that *can* answer
/// "is this leaf name enough?".
///
/// AGENTS-DOCTRINE §3: the illegal state — a row that does not know whether it
/// is ambiguous — is now unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowQualifier {
    /// The leaf name is unique among the rows delivered with it, so the leaf
    /// already identifies this row and a path would be noise.
    UniqueLeaf,
    /// The leaf collides with at least one sibling row.
    ///
    /// `tail` is the part of the module path that actually differs between the
    /// namesakes, already terminated with the producer's own separator.
    /// `elided_head` records that a prefix every namesake shared was dropped —
    /// the disambiguating segment is the valuable one, so `…/avx2/` beats a
    /// truncated `memchr::arch::x8…`. Empty `tail` means the collision is with
    /// a row that has no path segments at all (a package root, or a producer
    /// that recorded no path); the row then relies on the *absence* of a path
    /// tier to distinguish it, which is still a visible difference.
    Disambiguated {
        /// The differing tail of the path, with its trailing separator.
        tail: SharedString,
        /// Whether a shared head was elided (renders as a leading `…`).
        elided_head: bool,
    },
}

impl RowQualifier {
    /// The extra text the row draws below its signature, if any.
    ///
    /// Pre-formatted at prepare time; the render path only clones the
    /// `SharedString` (§1.1.4).
    pub fn text(&self) -> Option<SharedString> {
        match self {
            RowQualifier::UniqueLeaf => None,
            RowQualifier::Disambiguated { tail, .. } if tail.is_empty() => None,
            RowQualifier::Disambiguated { tail, elided_head } => Some(if *elided_head {
                SharedString::from(format!("…{tail}"))
            } else {
                tail.clone()
            }),
        }
    }
}

/// A [`HitRow`] converted once, at ingest, into exactly what `render` draws.
///
/// The wire type is render-*ready* in the sense that it needs no I/O, but it is
/// not render-*able* without three mappings that each allocate: the wire
/// signature token is a different type from the one [`SignatureLine`] consumes,
/// the `KindTag` must resolve to a local kind for its colour, and the display
/// name carries its module path inline. Doing any of that per frame would
/// defeat GPUI's shaped-text cache, so it happens here instead — once per hit,
/// on the generation that produced it.
///
/// # Construction
///
/// There is deliberately no public single-row constructor: see
/// [`RowQualifier`]. [`PreparedRow::prepare`] is the only way in.
#[derive(Clone, Debug)]
pub struct PreparedRow {
    /// Stable identity, emitted on open.
    ///
    /// `None` when the source that produced this hit could not supply a
    /// [`heart::query::StableReference`] (`heart::surface::SymbolHit::reference`
    /// is itself `Option` for exactly this reason — see that type's own doc
    /// comment). A row with no key is rendered normally but is **not
    /// openable**: fabricating a key from whatever bytes happen to be lying
    /// around (a durable id, a hash of the display name, …) would let a click
    /// resolve to the wrong symbol, or to no symbol at all, silently. A row
    /// that visibly cannot be clicked is the honest state; a row that silently
    /// fails on click is not.
    pub key: Option<SymbolKey>,
    /// The symbol's own name, without its path prefix.
    pub leaf: SharedString,
    /// The full module path prefix, with its trailing separator, or empty when
    /// the producer's display name carried none.
    ///
    /// This is the *whole* path, kept because the leading segment is the
    /// package name that the cross-package label reads. What the row draws as
    /// its disambiguating tier is [`PreparedRow::qualifier`], which may be
    /// shorter.
    pub path: SharedString,
    /// The owning package — the leading segment of [`PreparedRow::path`] — or
    /// empty when the producer's display name carried no path at all.
    ///
    /// # Why this is a field
    ///
    /// The row template used to slice it out of `path` itself, under a comment
    /// that named the fix and deferred it: "this is one `SharedString`
    /// allocation per visible row per render frame. The proper fix is to add
    /// `package: SharedString` to `PreparedRow`". This is that field.
    ///
    /// It belongs here for the same reason [`RowQualifier`] does: `path` is
    /// what the *wire* said, and everything the row draws should be decided
    /// once, on the generation that produced it, rather than re-derived by a
    /// template that runs on every frame of every scroll (§1.1.4).
    pub package: SharedString,
    /// Whether — and with what — this row is distinguished from its namesakes.
    pub qualifier: RowQualifier,
    /// Signature preview, already in the component's token vocabulary.
    pub sig: Vec<UiSigToken>,
    /// Resolved kind, or `None` for a discriminant this binary does not know
    /// (LD-7: renders as a visible chip, never a panic).
    pub kind: Option<LocalKindDiscriminant>,
    /// Label for the unknown-kind chip. Empty when `kind` is `Some`.
    pub unknown_kind_label: SharedString,
    /// Trust chrome selector (LD-8).
    pub provenance: Provenance,
    /// The engine's relevance for this hit, straight off the wire `score` in
    /// `(0, 1]`.
    ///
    /// Kept because the semantic section renders it as a *similarity meter*: a
    /// reader trusting a match found by meaning rather than by spelling needs to
    /// see how strong it is, and the value the ranking already sorted on is the
    /// honest source for that (§8 "counted, derived from the thing, not
    /// alongside it"). Carried for every section, but drawn only for the
    /// semantic one — for name/type rows `score` is a text-relevance product,
    /// not a similarity, so a meter there would invite a comparison the number
    /// does not support.
    pub relevance: f32,
}

impl PreparedRow {
    /// Convert one federated hit — `heart::surface::Located<heart::Scored<
    /// heart::surface::SymbolHit>>`, exactly what a `Frame::Item` carries —
    /// before the batch-wide disambiguation pass.
    ///
    /// Private on purpose — a row built alone cannot know whether its leaf
    /// name is unique, so it would have to guess, and guessing is what F1 was.
    ///
    /// # One ingest for both planes
    ///
    /// The local engine and a remote server both answer `Serve<Symbols>` with
    /// the same item type, `Scored<SymbolHit>` (`nudox_engine::surface`'s
    /// adapter for the former, `heart::client::remote::RemoteClient`'s blanket
    /// impl for the latter). There is therefore exactly one lowering from wire
    /// hit to render row, not one per plane — see `docs/LOCAL-REMOTE-CONTRACT.md`
    /// §2. `Located::residence` (not a field on `SymbolHit` itself, since
    /// residence is a property of *how the item arrived*, not of the symbol) is
    /// what [`provenance_of_residence`] maps onto LD-8's trust chrome.
    fn ingest(located: &Located<Scored<SymbolHit>>) -> PreparedRow {
        let symbol = &located.value.value;
        let (path, leaf) = split_qualified_name(&symbol.path);
        // `SymbolKind` is a closed enum; `Other` (and only `Other`) has no local
        // discriminant, so it degrades to a visible "other" chip rather than a
        // panic (LD-7).
        let kind = local_kind_of(symbol.kind);
        let unknown_kind_label = if kind.is_some() {
            SharedString::default()
        } else {
            SharedString::from("other")
        };
        // The package is the first segment of the path prefix
        // (`"tokio::runtime::"` → `"tokio"`). Empty path ⇒ empty package,
        // which the template reads as "nothing to disambiguate, draw no
        // label".
        //
        // Both separators, via the same rule as `path_segments` and
        // `split_qualified_name`. The template this replaces knew only about
        // `::`, so a Java, Go, C# or Python hit — every producer whose display
        // names are dot-separated — rendered *no* package label in a
        // cross-package search, silently. That is a defect, not a deliberate
        // narrowing, and it survived because the knowledge of how a producer
        // separates path segments lived in three places and only two of them
        // were kept current.
        let package = package_of(&path);
        let sig = symbol
            .signature
            .as_ref()
            .map(|s| s.tokens().iter().map(prepare_sig_token).collect())
            .unwrap_or_default();

        PreparedRow {
            // `None` when the source could not supply a `StableReference` —
            // see this field's own doc comment. Never fabricated.
            key: symbol.reference.as_ref().and_then(symbol_key_of_reference),
            leaf,
            path,
            package,
            // Overwritten by `prepare`, which is the only caller.
            qualifier: RowQualifier::UniqueLeaf,
            sig,
            kind,
            unknown_kind_label,
            provenance: provenance_of_residence(&located.residence),
            relevance: located.value.score.into_inner(),
        }
    }

    /// Convert a whole batch of federated hits, and make them tell each other
    /// apart.
    ///
    /// This is the call `SearchStore` makes each time its federated `Answer`
    /// delivers a batch, so that the disambiguation cost lands on the arriving
    /// generation rather than on every frame. Local and remote hits go through
    /// this one function — see [`PreparedRow::ingest`]'s own doc comment.
    ///
    /// # The invariant
    ///
    /// **No two rows in the returned batch render the same text**, whenever
    /// the engine gave us anything at all to tell them apart with. See
    /// [`PreparedRow::render_identity`] for the exact text compared, and
    /// `tests/screenshots.rs` for the assertion over the real corpus.
    pub fn prepare(hits: &[Located<Scored<SymbolHit>>]) -> Arc<[PreparedRow]> {
        let mut rows: Vec<PreparedRow> = hits.iter().map(PreparedRow::ingest).collect();
        let names: Vec<&str> = hits.iter().map(|h| &*h.value.value.path).collect();
        disambiguate(&mut rows, &names);
        rows.into()
    }

    /// Exactly the text this row draws, joined with a separator no symbol name
    /// can contain.
    ///
    /// This is the value the "no two rendered rows are textually identical"
    /// assertion compares. It exists as a method rather than being rebuilt in
    /// the test so that the test cannot drift from the template: adding a tier
    /// to [`crate::views::omni_search`]'s row without adding it here is the one
    /// way this guard could quietly stop guarding, and that is a review-visible
    /// edit in this file rather than an invisible one in a view.
    pub fn render_identity(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.leaf);
        out.push('\u{1}');
        if let Some(q) = self.qualifier.text() {
            out.push_str(&q);
        }
        out.push('\u{1}');
        for token in &self.sig {
            match token {
                UiSigToken::Kw(s) | UiSigToken::Punct(s) => out.push_str(s),
                UiSigToken::Ident(s) | UiSigToken::Generic(s) => out.push_str(s),
                UiSigToken::Ty { text, .. } => out.push_str(text),
                UiSigToken::Ws => out.push(' '),
            }
        }
        out.push('\u{1}');
        match self.kind {
            Some(k) => out.push_str(k.short_label()),
            None => out.push_str(&self.unknown_kind_label),
        }
        out
    }
}

/// Length of the path prefix shared by every listed row, capped so that each
/// of them keeps at least one segment.
///
/// Returns `0` for fewer than two rows: with nothing to compare against there
/// is no "shared" head, and eliding one row's own path would delete the only
/// thing it has to say.
fn common_head_len(segmented: &[(Vec<String>, &'static str)], group: &[usize]) -> usize {
    if group.len() < 2 {
        return 0;
    }
    let shortest = group
        .iter()
        .map(|&ix| segmented[ix].0.len())
        .min()
        .unwrap_or(0);
    // `shortest - 1`: the shortest member must retain a segment, otherwise it
    // renders as "no path" and reads like the unqualified case.
    let ceiling = shortest.saturating_sub(1);
    let first = &segmented[group[0]].0;
    let mut common = 0;
    while common < ceiling
        && group
            .iter()
            .all(|&ix| segmented[ix].0[common] == first[common])
    {
        common += 1;
    }
    common
}

/// The batch-wide row disambiguation pass, shared by the local
/// ([`PreparedRow::prepare`]) and remote ([`PreparedRow::prepare_remote`])
/// ingests so the "no two rows render the same text" invariant is enforced by
/// one rule for both. `names[i]` is the fully-qualified display name of
/// `rows[i]` (the source the path segments come from).
fn disambiguate(rows: &mut [PreparedRow], names: &[&str]) {
    // Segment every row's path once. `segments[i]` is the *path* only; the leaf
    // lives in `rows[i].leaf`.
    let segmented: Vec<(Vec<String>, &'static str)> =
        names.iter().map(|n| path_segments(n)).collect();

    // Group row indices by leaf name. Only groups larger than one need a
    // qualifier at all.
    let mut by_leaf: std::collections::HashMap<SharedString, Vec<usize>> =
        std::collections::HashMap::new();
    for (ix, row) in rows.iter().enumerate() {
        by_leaf.entry(row.leaf.clone()).or_default().push(ix);
    }

    for (_leaf, group) in by_leaf {
        if group.len() < 2 {
            continue;
        }
        // Elide only the head that *every namesake with a path* shares, and
        // never so much that one of them is left with nothing: the
        // disambiguating segment is precisely the one that must survive.
        let with_path: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&ix| !segmented[ix].0.is_empty())
            .collect();
        let common = common_head_len(&segmented, &with_path);

        for &ix in &group {
            let (segs, sep) = &segmented[ix];
            let keep = &segs[common.min(segs.len())..];
            let tail = if keep.is_empty() {
                SharedString::default()
            } else {
                SharedString::from(format!("{}{sep}", keep.join(sep)))
            };
            rows[ix].qualifier = RowQualifier::Disambiguated {
                tail,
                elided_head: common > 0 && !segs.is_empty(),
            };
        }
    }
}

/// The owning package — the leading segment of a path prefix (`"tokio::runtime::"`
/// → `"tokio"`), understanding both the `::` and `.` conventions. Empty path ⇒
/// empty package. One definition, shared by both ingests.
fn package_of(path: &str) -> SharedString {
    match path.find(separator_of(path)) {
        Some(end) => SharedString::from(path[..end].to_owned()),
        None => SharedString::default(),
    }
}

/// Map a [`SymbolKind`] onto the local kind discriminant used for the kind
/// badge — shared by every hit, local or remote, since both planes answer with
/// the same `SymbolHit::kind`. Total: `SymbolKind::Other` has no local
/// discriminant and returns `None`, which the row renders as a visible "other"
/// chip (LD-7) — never a panic, never a silently-dropped kind.
fn local_kind_of(kind: SymbolKind) -> Option<LocalKindDiscriminant> {
    use LocalKindDiscriminant as K;
    match kind {
        SymbolKind::Function => Some(K::Function),
        SymbolKind::Type => Some(K::Record),
        SymbolKind::Module => Some(K::Module),
        SymbolKind::Constant => Some(K::Const),
        SymbolKind::Variable => Some(K::Static),
        SymbolKind::Trait => Some(K::Trait),
        SymbolKind::Impl => Some(K::Impl),
        // The one kind with no local badge colour: a visible chip, not a guess.
        SymbolKind::Other => None,
    }
}

/// Resolve a [`StableReference`] (`heart::surface::SymbolHit::reference`) back
/// into the local engine's own [`SymbolKey`] (`StableRef { package, intro }`),
/// so a row built from *either* plane opens through the exact same
/// `SymbolStore::open` path.
///
/// # Why this is the fix, not a variant of the bug it replaces
///
/// The function this replaced (`remote_symbol_key`) fabricated a `SymbolKey`
/// by copying a UUID's 16 bytes into an `IntroId` and zeroing the rest. A real
/// `IntroId` is `blake3("nudox.intro.v5" ‖ …)` (`nudox_ir::change::intro`), so
/// a fabricated one can never equal a sealed entry's real key —
/// `resolve_symbol` (`nudox-engine/src/doc/mod.rs`) does an exact lookup and
/// returns `SymbolNotFound` for every single click. `StableReference` is not a
/// fabrication: it is `F:<eco>/<pkg>#<hex>`, the *same* frozen grammar
/// `nudox_engine::surface::stable_reference` derives from the real wire
/// `SymbolKey` on the way out — parsing it back is a decode of real identity,
/// not a guess. Returns `None` (never a panic, never a wrong key) when the hex
/// is not exactly 32 bytes — malformed input from a future producer is refused
/// rather than silently truncated or padded.
fn symbol_key_of_reference(reference: &StableReference) -> Option<SymbolKey> {
    use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName};

    let hex = reference.intro_hex();
    if hex.len() != 64 {
        return None;
    }
    let mut raw = [0u8; 32];
    let (chunks, _remainder) = hex.as_bytes().as_chunks::<2>();
    for (byte, chunk) in raw.iter_mut().zip(chunks) {
        *byte = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }

    Some(SymbolKey::new(
        PackageLineageId::new(
            EcosystemId::new(reference.ecosystem()),
            PackageName::new(reference.package()),
        ),
        IntroId::from_raw(raw),
    ))
}

/// Map [`Residence`] (which plane an item arrived from, and how much it can be
/// trusted while offline — `heart::surface::Residence`'s own doc comment) onto
/// LD-8's four-way trust chrome.
///
/// This is [`prepare_provenance`]'s sibling: `prepare_provenance` adapts the
/// older `nudox_engine::wire::Provenance` (still used by the symbol-page
/// header, which reads a `SymbolHead` rather than a federated search answer);
/// this adapts `heart::surface::Residence`, the type every `Serve<Symbols>`
/// answer — local, remote, or a federating merge of both — actually carries
/// per item. The two source enums are deliberately not unified (`Residence`
/// *is* `heart`'s promotion of `wire::Provenance`, per that type's own doc
/// comment, but the header's `SymbolHead` has not been migrated onto it yet),
/// so this stays a second small mapping rather than a wrapper around the first.
fn provenance_of_residence(residence: &Residence) -> Provenance {
    match residence {
        Residence::Local => Provenance::TrustedLocal,
        Residence::Synced { .. } => Provenance::SyncedLocal,
        Residence::Remote { .. } => Provenance::Remote,
        Residence::Stale { .. } => Provenance::Stale,
        // `Residence` is `#[non_exhaustive]`: a future residence kind reads as
        // untrusted-until-proven rather than as local — LD-8 is a trust claim,
        // so the safe default is the weakest one (mirrors `prepare_provenance`'s
        // fallback, once collapsed onto the same choice — see that function).
        _ => Provenance::Stale,
    }
}

/// Split a display name into its path segments and the separator the producer
/// used, discarding the leaf.
///
/// # Why two separators
///
/// `nudox-engine`'s `qualified_display_name` builds the qualified form from
/// `PackageIndexes::path_of`, whose monikers are **dot**-separated
/// (`memchr.arch.x86_64.avx2.memchr`), while a hand-built or cross-package
/// name is `::`-separated. Splitting on only `::` is why a fully-qualified
/// name arrived, was not recognised as qualified, and rendered as one enormous
/// leaf with an empty path tier — visible in `tests/shots/memchr/04-search-hits.png`
/// before this change.
///
/// The separator that was found is returned rather than normalised, because
/// rewriting `.` to `::` would be a claim about the language that this layer
/// has no way to check — lindsey documents seven of them.
/// Which separator a producer used in this qualified name.
///
/// One definition, because three copies of this rule is how the search row's
/// package label came to understand only `::` while the two functions beside
/// it understood both: the knowledge drifted in the copy nobody was looking at.
/// A name with no separator at all reports `"::"` — it has no path to split, so
/// the answer is arbitrary, and picking the majority convention keeps the
/// `Rust`-shaped default.
fn separator_of(name: &str) -> &'static str {
    if name.contains("::") {
        "::"
    } else if name.contains('.') {
        "."
    } else {
        "::"
    }
}

fn path_segments(name: &str) -> (Vec<String>, &'static str) {
    let sep = separator_of(name);
    let split: Vec<&str> = name.split(sep).collect();
    let mut segments: Vec<String> = split.into_iter().map(|s| s.to_owned()).collect();
    // The last piece is the leaf, which lives on `PreparedRow::leaf`.
    segments.pop();
    (segments, sep)
}

/// Split `a::b::c` into (`"a::b::"`, `"c"`), understanding both the `::` and
/// the `.` conventions — see [`path_segments`].
///
/// `HitRow::display_name` "may include path prefix for disambiguation" and the
/// wire carries no separate path field, so the split happens here. Done at
/// ingest, never in render.
pub fn split_qualified_name(name: &str) -> (SharedString, SharedString) {
    if let Some(ix) = name.rfind("::") {
        return (
            SharedString::from(name[..ix + 2].to_owned()),
            SharedString::from(name[ix + 2..].to_owned()),
        );
    }
    match name.rfind('.') {
        Some(ix) => (
            SharedString::from(name[..ix + 1].to_owned()),
            SharedString::from(name[ix + 1..].to_owned()),
        ),
        None => (SharedString::default(), SharedString::from(name.to_owned())),
    }
}

/// Map wire provenance (`SymbolHead`, from the symbol-page header's document
/// stream) onto the trust-chrome selector (LD-8).
///
/// The payloads (`generation`, `as_of`) do not affect chrome; §10.4 keys the
/// colour, glyph, and label off the level alone.
///
/// This is the *one* `nudox_engine::wire::Provenance` adapter in the app —
/// `views::symbol_page::header::trust_of` used to duplicate this exact match
/// with a different `#[non_exhaustive]` fallback arm (`Remote` there,
/// `Remote` here too before this fix — a live drift a reviewer could not see
/// without diffing both bodies by hand). `header::trust_of` now calls this
/// function instead of restating it. See [`provenance_of_residence`] for the
/// sibling that adapts `heart::surface::Residence`, the type a federated
/// search answer actually carries per item.
pub fn prepare_provenance(p: &WireProvenance) -> Provenance {
    match p {
        WireProvenance::TrustedLocal => Provenance::TrustedLocal,
        WireProvenance::SyncedLocal { .. } => Provenance::SyncedLocal,
        WireProvenance::Remote { .. } => Provenance::Remote,
        WireProvenance::Stale { .. } => Provenance::Stale,
        // `WireProvenance` is `#[non_exhaustive]` (LR-12): a level added by a
        // newer engine reads as untrusted-until-proven, never as local — LD-8
        // is a trust claim, so the safe default is the weakest one.
        _ => Provenance::Stale,
    }
}

/// Translate one `heart::surface::SigToken` (a federated `SymbolHit`'s
/// signature preview — local and remote hits both carry this type) into the
/// component's vocabulary.
///
/// The two enums are deliberately near-identical (see `signature_line.rs`), so
/// this is a mapping and not a re-implementation. Lifetimes have no dedicated
/// component variant and render as generics, which is how they read anyway.
pub fn prepare_sig_token(t: &HeartSigToken) -> UiSigToken {
    match t {
        HeartSigToken::Kw(s) => UiSigToken::Kw(SharedString::from(s.to_string())),
        HeartSigToken::Ident(s) => UiSigToken::Ident(SharedString::from(s.to_string())),
        HeartSigToken::Ty { text, target } => UiSigToken::Ty {
            text: SharedString::from(text.to_string()),
            // `SignatureLine`'s key is still the pre-wire mirror (its own
            // TODO(wire)); a resolved target is carried as its real, navigable
            // `StableReference` string (`F:<eco>/<pkg>#<hex>`) rather than a
            // debug dump, so the link text is at least meaningful if ever
            // inspected before the mirror is retired.
            target: target
                .as_ref()
                .map(|r| UiSymbolKey(SharedString::from(r.to_string()))),
        },
        HeartSigToken::Punct(s) => UiSigToken::Punct(SharedString::from(s.to_string())),
        HeartSigToken::Ws => UiSigToken::Ws,
        HeartSigToken::Generic(s) => UiSigToken::Generic(SharedString::from(s.to_string())),
        HeartSigToken::Lifetime(s) => UiSigToken::Generic(SharedString::from(s.to_string())),
        // `HeartSigToken` is `#[non_exhaustive]`: an unknown token renders as a
        // visible mark rather than vanishing, so a signature never silently
        // loses a piece (LD-7).
        _ => UiSigToken::Punct(SharedString::from("?")),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SectionStatus & SectionData
// ─────────────────────────────────────────────────────────────────────────────

/// What one section is currently doing (§15 states).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionStatus {
    /// No query issued yet.
    Idle,
    /// Query out, rows not yet delivered. Only the semantic section shows a
    /// shimmer for this — local sections beat the shimmer grace and would only
    /// flash (§8.1 grace periods, §15 "no skeleton" for local).
    Loading,
    /// Rows delivered.
    Ready,
    /// The backend is unreachable; this section is replaced by a `trust.stale`
    /// notice rather than an error (§15 offline state).
    Offline,
    /// Rows delivered, but over **part** of the resident corpus.
    ///
    /// Distinct from `Loading` on purpose, and the distinction is the reason
    /// this variant exists. `Loading` means *these rows are not here yet* — the
    /// right rendering is a shimmer where they will appear. This means *these
    /// rows are here and they are the whole truth about `covered` of `total`
    /// packages* — the right rendering is the rows themselves, plus a caption
    /// saying what they are a ranking over. Showing a shimmer for this would
    /// hide real answers the reader can already use; showing nothing at all
    /// would be the worse half of the same mistake.
    ///
    /// A package indexed later can outrank everything currently shown, which is
    /// why the caption carries counts rather than a spinner: "3 of 20 packages"
    /// is a caveat a reader can weigh.
    Building { covered: u32, total: u32 },
    /// This build cannot answer this section at all.
    ///
    /// Not an error and not a zero-hit result. The semantic section reports it
    /// when no embedding model is installed — which is every build in this
    /// repository today (`docs/AGENTS-DOCTRINE.md` §1, capability ports). Rendering
    /// it as "no results" would be a claim about the corpus that the engine is
    /// in no position to make.
    Unavailable,
}

/// One section's render-ready state.
#[derive(Clone, Debug)]
pub struct SectionData {
    /// Rows, pre-sorted by score by the store. Cloning this is a refcount bump.
    pub rows: Arc<[PreparedRow]>,
    /// Current phase.
    pub status: SectionStatus,
    /// Pre-formatted per-section latency readout (e.g. `"4 ms"`). Empty until
    /// a `SearchEvent::Latency` for this section arrives.
    pub latency: SharedString,
}

impl Default for SectionData {
    fn default() -> Self {
        SectionData {
            rows: Arc::from([] as [PreparedRow; 0]),
            status: SectionStatus::Idle,
            latency: SharedString::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchSnapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the overlay reads in one frame.
///
/// A single snapshot rather than a dozen getters: the view takes one immutable
/// borrow of the store, clones a handful of `Arc`s and `SharedString`s out of
/// it, and then never touches the store again while building elements — which
/// is what keeps `render` free of interleaved store borrows.
#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    /// Current query text.
    pub input: SharedString,
    /// Active mode chip.
    pub mode: SearchMode,
    /// Scope chips, in display order.
    pub scopes: Arc<[ScopeChip]>,
    /// The three sections, indexed by [`Section::index`].
    pub sections: [SectionData; SECTION_COUNT],
    /// Recent symbols, shown when the input is empty (§15, from `NavHistory`).
    pub recents: Arc<[PreparedRow]>,
    /// Current selection, if any.
    pub selection: Option<Cursor>,
    /// Monotonic generation counter. Drives entrance identity (§4.1).
    pub generation: u64,
    /// When this generation's first page landed. `None` until it does.
    pub gen_arrival: Option<Instant>,
    /// Whether the current answer is degraded — some configured source (today,
    /// only ever the remote index; the local engine is never itself a
    /// federation member that can degrade) did not answer, per
    /// `heart::surface::Frame::Degraded`. The local-first rows already in
    /// `sections` still render; this only says the answer may be missing
    /// whatever that source would have contributed. Not an error state — see
    /// `heart::surface::Federated`'s own doc comment on why offline is a
    /// smaller answer, never a failed one.
    pub offline: bool,
}

impl Default for SearchSnapshot {
    fn default() -> Self {
        SearchSnapshot {
            input: SharedString::default(),
            mode: SearchMode::Auto,
            scopes: Arc::from([] as [ScopeChip; 0]),
            sections: [
                SectionData::default(),
                SectionData::default(),
                SectionData::default(),
            ],
            recents: Arc::from([] as [PreparedRow; 0]),
            selection: None,
            generation: 0,
            gen_arrival: None,
            offline: false,
        }
    }
}

impl SearchSnapshot {
    /// Row counts per section — the input to every selection-movement rule.
    pub fn counts(&self) -> [usize; SECTION_COUNT] {
        [
            self.sections[0].rows.len(),
            self.sections[1].rows.len(),
            self.sections[2].rows.len(),
        ]
    }

    /// Total hits across all sections.
    pub fn total_hits(&self) -> usize {
        self.counts().iter().sum()
    }

    /// The row a cursor points at, if it is in range.
    pub fn row_at(&self, cursor: Cursor) -> Option<&PreparedRow> {
        self.sections
            .get(cursor.section)
            .and_then(|s| s.rows.get(cursor.row))
    }

    /// Whether any section has finished at least once — i.e. whether "zero
    /// hits" is a real answer rather than "nothing has arrived yet".
    pub fn any_section_settled(&self) -> bool {
        self.sections
            .iter()
            .any(|s| matches!(s.status, SectionStatus::Ready | SectionStatus::Offline))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchAccess — the contract between the view and the store
// ─────────────────────────────────────────────────────────────────────────────

/// The contract the omni-search view needs from its backing store (§12.3).
///
/// Lives in `search_model` (the store layer) rather than in `views/omni_search`
/// so that `SearchStore` can implement it without creating a `stores → views`
/// dependency.  The view re-exports this trait from its own module so existing
/// import paths remain unchanged.
///
/// The read half is a single [`SearchSnapshot`] producer so that adding a field
/// to the overlay never adds a method here.  The write half is deliberately
/// tiny: movement policy lives in the view as pure functions (`step_cursor`,
/// `next_section_cursor`, `jump_to_section_cursor`), because §15's "never
/// wraps silently across sections" is a view invariant and is unit-tested there.
/// The store only records where the cursor ended up.
pub trait SearchAccess: 'static + Sized {
    /// Everything the overlay reads for one frame.
    ///
    /// Implementations return pre-computed state (§12: stores hold
    /// render-ready state); this must not sort, filter, or format.
    fn snapshot(&self) -> SearchSnapshot;

    /// The user typed. Restarts the 24 ms debounce and supersedes the
    /// generation (§7.4).
    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>);

    /// A mode chip was clicked; re-issue the query.
    fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>);

    /// A scope chip at `ix` in [`SearchSnapshot::scopes`] was toggled.
    fn toggle_scope(&mut self, ix: usize, cx: &mut Context<Self>);

    /// Record the new cursor. Pure state; the view has already applied policy.
    fn set_selection(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use heart::{PackageId, Score};

    /// A hit whose path is `name` and whose signature is one keyword, i.e. the
    /// shape a `mod` hit really has. Wrapped as [`Located<Scored<SymbolHit>>`],
    /// exactly what a `Frame::Item` carries — this is the one ingest now, for
    /// both planes.
    ///
    /// The point of these cases is that the *rendered text* must differ, and
    /// nothing these rows carry but never draw (the package id, the residence)
    /// is what makes them differ.
    fn hit(name: &str) -> Located<Scored<SymbolHit>> {
        Located::new(
            Scored::new(
                SymbolHit {
                    package: PackageId::new_random(),
                    path: name.into(),
                    display_name: name.into(),
                    ecosystem: heart::Language::Rust,
                    kind: SymbolKind::Module,
                    signature: Some(heart::surface::Signature::new(vec![HeartSigToken::Kw(
                        "mod".into(),
                    )])),
                    reference: None,
                },
                Score::try_new(1.0).expect("1.0 is finite"),
            ),
            Residence::Local,
        )
    }

    /// Every rendered row in a batch must be textually distinct. This is the
    /// assertion that would have caught GUI-WORKORDER-2 F1 — seven consecutive
    /// `mod memchr` rows — and did not exist.
    fn assert_rows_distinct(rows: &[PreparedRow]) {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (ix, row) in rows.iter().enumerate() {
            if let Some(first) = seen.insert(row.render_identity(), ix) {
                panic!(
                    "rows {first} and {ix} render identically as {:?} — a user \
                     has no way to choose between them",
                    row.render_identity()
                );
            }
        }
    }

    /// The row's package label is decided at ingest, from the leading path
    /// segment — the value `render_row` used to slice out of `path` on every
    /// frame.
    #[test]
    fn the_package_label_is_the_leading_path_segment() {
        let rows = PreparedRow::prepare(&[hit("serde_json::value::Value")]);
        assert_eq!(rows[0].package.as_ref(), "serde_json");
    }

    /// A dot-separated producer gets a package label too.
    ///
    /// The render-time helper this replaced tested `path.find("::")` and
    /// nothing else, so every Java, Go, C# and Python hit came out with no
    /// package label in a cross-package search — invisibly, because the
    /// missing label looks exactly like the single-package case that is
    /// supposed to have none.
    #[test]
    fn a_dot_separated_producer_still_names_its_package() {
        let rows = PreparedRow::prepare(&[hit("memchr.arch.x86_64.avx2.memchr")]);
        assert_eq!(rows[0].package.as_ref(), "memchr");
    }

    /// A bare leaf has no path, so there is no package to name and the row
    /// draws no label — the empty string is that answer.
    #[test]
    fn a_bare_leaf_has_no_package_label() {
        let rows = PreparedRow::prepare(&[hit("Value")]);
        assert!(rows[0].path.is_empty());
        assert!(rows[0].package.is_empty());
    }

    #[test]
    fn qualified_names_split_into_path_and_leaf() {
        let (path, leaf) = split_qualified_name("serde_json::value::Value");
        assert_eq!(path.as_ref(), "serde_json::value::");
        assert_eq!(leaf.as_ref(), "Value");
    }

    /// `nudox-engine` builds its qualified display names from dot-separated
    /// monikers. Recognising only `::` is why `memchr.arch.x86_64.avx2.memchr`
    /// rendered as one giant leaf with an empty path tier (F1).
    #[test]
    fn dot_separated_monikers_split_into_path_and_leaf() {
        let (path, leaf) = split_qualified_name("memchr.arch.x86_64.avx2.memchr");
        assert_eq!(path.as_ref(), "memchr.arch.x86_64.avx2.");
        assert_eq!(leaf.as_ref(), "memchr");
    }

    /// The F1 case, in miniature: several modules that share a leaf name must
    /// come out of `prepare` distinguishable, and the segment that actually
    /// differs is the one shown.
    #[test]
    fn colliding_leaves_are_disambiguated_by_their_differing_segment() {
        let rows = PreparedRow::prepare(&[
            hit("memchr.arch.all.memchr"),
            hit("memchr.arch.x86_64.avx2.memchr"),
            hit("memchr.arch.wasm32.simd128.memchr"),
        ]);
        assert_rows_distinct(&rows);
        let tails: Vec<String> = rows
            .iter()
            .map(|r| {
                r.qualifier
                    .text()
                    .map(|t| t.to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            tails,
            vec!["…all.", "…x86_64.avx2.", "…wasm32.simd128."],
            "the shared `memchr.arch.` head must be elided and the differing \
             tail kept — truncating the tail instead throws away the only \
             informative part"
        );
    }

    /// A leaf that appears once needs no path: adding one would be noise on
    /// every row to serve none.
    #[test]
    fn unique_leaves_carry_no_qualifier() {
        let rows = PreparedRow::prepare(&[hit("memchr.Memchr"), hit("memchr.arch.all.memchr")]);
        assert_rows_distinct(&rows);
        for row in rows.iter() {
            assert_eq!(row.qualifier, RowQualifier::UniqueLeaf);
            assert!(row.qualifier.text().is_none());
        }
    }

    /// Elision must never consume a namesake's last segment: a row left with
    /// no path renders exactly like the unqualified case and the collision is
    /// back.
    #[test]
    fn elision_never_empties_a_namesake() {
        let rows = PreparedRow::prepare(&[hit("memchr.memchr"), hit("memchr.arch.all.memchr")]);
        assert_rows_distinct(&rows);
        let tails: Vec<String> = rows
            .iter()
            .map(|r| {
                r.qualifier
                    .text()
                    .map(|t| t.to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(tails, vec!["memchr.", "memchr.arch.all."]);
    }

    /// A namesake with no path at all (a package root, or a producer that
    /// recorded none) is still distinguishable — by the *absence* of the path
    /// tier its namesakes have.
    #[test]
    fn pathless_namesake_still_renders_distinctly() {
        let rows = PreparedRow::prepare(&[hit("memchr"), hit("memchr.arch.all.memchr")]);
        assert_rows_distinct(&rows);
        assert!(rows[0].qualifier.text().is_none());
        assert_eq!(
            rows[1].qualifier.text().unwrap().as_ref(),
            "memchr.arch.all."
        );
    }

    #[test]
    fn bare_names_have_no_path() {
        let (path, leaf) = split_qualified_name("Value");
        assert!(path.is_empty());
        assert_eq!(leaf.as_ref(), "Value");
    }

    #[test]
    fn section_count_is_three() {
        assert_eq!(SECTION_COUNT, 3);
        assert_eq!(Section::ALL.len(), SECTION_COUNT);
    }

    #[test]
    fn section_index_round_trips() {
        for s in Section::ALL {
            let ix = s.index();
            assert_eq!(Section::from_index(ix), Some(s));
        }
        assert_eq!(Section::from_index(3), None);
    }

    #[test]
    fn search_snapshot_counts_matches_section_rows() {
        let mut snap = SearchSnapshot::default();
        // Inject some rows into section 0 via a replacement Arc.
        snap.sections[0].rows = Arc::from([] as [PreparedRow; 0]);
        assert_eq!(snap.counts(), [0, 0, 0]);
        assert_eq!(snap.total_hits(), 0);
    }

    #[test]
    fn any_section_settled_requires_ready_or_offline() {
        let mut snap = SearchSnapshot::default();
        assert!(!snap.any_section_settled());
        snap.sections[1].status = SectionStatus::Ready;
        assert!(snap.any_section_settled());
    }

    #[test]
    fn row_at_returns_none_when_out_of_range() {
        let snap = SearchSnapshot::default();
        assert!(snap.row_at(Cursor { section: 0, row: 0 }).is_none());
        assert!(snap.row_at(Cursor { section: 5, row: 0 }).is_none());
    }

    #[test]
    fn search_mode_default_is_auto() {
        assert_eq!(SearchMode::default(), SearchMode::Auto);
    }

    #[test]
    fn search_mode_labels_are_non_empty() {
        for m in SearchMode::ALL {
            assert!(!m.label().is_empty());
        }
    }
}
