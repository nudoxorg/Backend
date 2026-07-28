//! Shared one-walk core for `sections` and `plan`.
//!
//! # Why a shared walk?
//!
//! `plan::section_plan` and `sections::sections` must agree on section count,
//! ids, and order by construction — not by convention.  The `WalkOutput` type
//! and `walk_doc` function are the mechanism: both callers call `walk_doc` and
//! destructure whichever field they need.  The `SectionId` counter is
//! incremented once, inside this function, so the two outputs are
//! structurally identical.
//!
//! The tradeoff is that `walk_doc` does slightly more work than each caller
//! strictly needs (building `ProseBlock`s for the plan, or building
//! `SectionPlan`s for the section emitter).  The overhead is negligible: doc
//! parsing is O(n) in the documentation text, which is small, and the structs
//! are stack-local until moved into the output.

use std::collections::HashMap;
use std::sync::Arc;

use nudox_ir::{
    change::IntroId,
    entry::Entry,
    kind::Kind,
    view::IrView,
};
use nudox_store::package::PackageView;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::wire::{
    CalloutLevel, FieldRow, KindTag, LangId, LinkTarget, MemberRow, ProseBlock, RenderSection,
    SectionId, SectionKind, SectionPlan, SharedStr, SizeHint, SymbolKey,
};

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// The combined output of one documentation walk.
///
/// Callers destructure whichever field they need; neither field is computed
/// independently, so they are always in sync.
pub(crate) struct WalkOutput {
    pub sections: Vec<RenderSection>,
    pub plan: Vec<SectionPlan>,
}

// ---------------------------------------------------------------------------
// DocLinkTable
// ---------------------------------------------------------------------------

/// A pre-built resolution table from the producer's resolved intra-doc links.
///
/// # Why a separate type?
///
/// `Symbol::doc_links` carries entries that rust-analyzer already resolved
/// with full name resolution in scope — far more accurate than any path-string
/// heuristic the engine could compute on its own.  The table maps the link
/// target string exactly as it appeared in the source (e.g. `"Router::with_state"`)
/// to the `SymbolKey` that the producer determined it refers to.
///
/// The table is built once per entry at the top of `walk_doc` and threaded
/// into `build_prose_blocks`.  It is small (typically 0–20 entries) and
/// short-lived — there is no reason to cache it across entries.
///
/// # Resolution algorithm
///
/// For each `DocLink`:
/// 1. The `target` string from the producer is **normalised** first:
///    strip any namespace tag (`!m`, `!v`, etc.), strip any rustdoc anchor
///    prefix (`#method.`), and replace `::` with `.` so all path forms unify.
/// 2. We extract the leaf name (last `.` segment of the normalised path) and
///    look it up in the `NameIndex` (case-folded exact match on the leaf).
/// 3. Among the hits, we pick the one whose precomputed dot-path most closely
///    matches the normalised target suffix.  When multiple entries have the
///    same leaf name, this narrows to the correct one; when only one exists,
///    the suffix check is skipped.
/// 4. A successful match yields a `SymbolKey`; we store it under four aliases:
///    the original target, the normalised form, the bare leaf, and the display
///    label.  This ensures that `[Router::with_state]` (as the author wrote
///    it) resolves through the same key as
///    `"axum.routing.Router.with_state!m"` (as the producer stored it).
pub(crate) struct DocLinkTable {
    inner: HashMap<String, SymbolKey>,
    /// Whether the symbol *declared* any intra-doc links at all, regardless of
    /// how many of them resolved inside this package.
    ///
    /// This is deliberately not the same question as "is `inner` empty", and
    /// conflating the two is a bug we shipped once. A symbol whose only link
    /// is cross-crate (`[tower::Service]`) resolves nothing, so `inner` is
    /// empty — but the brackets in its prose really are a link and must not
    /// survive into the rendered text.
    ///
    /// The distinction that matters:
    ///
    /// * **No links declared** — bracketed text is ordinary prose (`[1]`,
    ///   `[old]`, `[!NOTE]`). Touching it would corrupt the document.
    /// * **Links declared, this one unresolvable** — it is an intra-doc link
    ///   pointing outside this package. Render the target as plain text with
    ///   the brackets removed, and never as a dead link.
    declared: bool,
}

impl DocLinkTable {
    /// Build from the producer-resolved `doc_links` on one `Symbol`.
    ///
    /// `entry.sym().doc_links` is populated by the producer (e.g.
    /// `nudox-producer-rust`) using its oracle's name-resolution.  We never
    /// re-parse or re-resolve here; we only index what the producer stored.
    ///
    /// # Target normalisation (defect fix)
    ///
    /// Real producers emit `DocLink::target` in at least four distinct formats:
    ///
    /// - `"Router::with_state"` — `::` path, the form fixtures use
    /// - `"axum.routing.Router.with_state"` — dot-path (moniker form)
    /// - `"axum.routing.Router.with_state!m"` — dot-path with namespace tag
    /// - `"#method.with_state"` — rustdoc HTML anchor form
    ///
    /// The original code extracted the leaf only by splitting on `"::"`.  A
    /// target like `"axum.routing.Router.with_state!m"` contains no `"::"`,
    /// so the whole string became the leaf, and `by_name.get_exact(...)` found
    /// nothing.  Similarly, a namespace tag like `"!m"` was left on the leaf,
    /// preventing the exact-match lookup from ever succeeding.
    ///
    /// The fix normalises every target before leaf extraction:
    /// 1. Strip the namespace tag (everything from `!` to end).
    /// 2. Strip the rustdoc anchor prefix (`#discriminator.`).
    /// 3. Replace `"::"` with `"."` so both path forms share one code path.
    ///
    /// We then register the resolved key under **four** aliases: the original
    /// target string, the normalised form, the bare leaf, and the display label.
    /// This ensures that a lookup with the doc-text form (`"Router::with_state"`,
    /// as the author wrote it in `[Router::with_state]`) hits the same entry as
    /// a lookup with the producer's canonical form.
    pub(crate) fn build(entry: &Entry, package: &PackageView) -> Self {
        let mut inner: HashMap<String, SymbolKey> = HashMap::new();
        let declared = !entry.sym().doc_links.is_empty();

        for doc_link in entry.sym().doc_links.iter() {
            let target = doc_link.target.as_str();

            // Step 1: strip namespace tag.
            //   `"Router::with_state!m"` → `"Router::with_state"`.
            //   `"axum.routing.Router.with_state!v"` → `"axum.routing.Router.with_state"`.
            //   Symbol names never contain `!`, so this split is unambiguous.
            let stripped_tag = match target.find('!') {
                Some(idx) => &target[..idx],
                None => target,
            };

            // Step 2: strip rustdoc anchor prefix.
            //   `"#method.with_state"` → `"with_state"`.
            //   rustdoc sometimes emits the HTML fragment identifier as the
            //   target.  The format is `#<discriminator>.<name>` where
            //   `<discriminator>` is `method`, `structfield`, `variant`, etc.
            //   We strip the `#<discriminator>.` prefix to recover the bare name.
            let stripped_anchor: &str = if let Some(rest) = stripped_tag.strip_prefix('#') {
                match rest.find('.') {
                    Some(dot) => &rest[dot + 1..],
                    None => rest,
                }
            } else {
                stripped_tag
            };

            // Step 3: unify path separators.
            //   `"Router::with_state"` → `"Router.with_state"`.
            //   This means both `::` paths and dot-paths share the same leaf
            //   extraction and suffix-matching logic.
            let normalised: String = stripped_anchor.replace("::", ".");

            // Leaf: last `.` segment of the normalised path.
            let leaf: &str = normalised.rsplit('.').next().unwrap_or(&normalised);
            let leaf_lower = leaf.to_lowercase();

            // Normalised path lowercased for suffix matching.
            let normalised_lower = normalised.to_lowercase();

            // Look up all entries with this leaf name.
            let hits: Vec<_> = package.indexes().by_name.get_exact(&leaf_lower).to_vec();

            let resolved = match hits.len() {
                0 => {
                    // No entry with that leaf name in this corpus — cross-crate
                    // or genuinely absent.  We store nothing; the caller will
                    // emit plain text.
                    continue;
                }
                1 => {
                    // Unique hit: use it directly without the suffix check.
                    // This is the overwhelmingly common case for intra-crate links.
                    hits[0].intro
                }
                _ => {
                    // Ambiguous: narrow by suffix match against the stored path.
                    // We prefer the entry whose dot-path (lowercased) ends with
                    // the normalised target.  If more than one matches, we take
                    // the longest path (most-specific) to resolve e.g.
                    // `"io::Read"` vs `"fmt::Read"`.
                    //
                    // # Why the exact-suffix check can fail for impl methods
                    //
                    // The producer's `ctx.canonical(resolved_def)` uses
                    // `def.module(self.db)` to build the path, which returns
                    // the *containing module* — not the impl block or struct.
                    // For `Router::with_state` the producer emits
                    // `"axum::routing::with_state"` (normalised:
                    // `"axum.routing.with_state"`), while the engine's
                    // `moniker_path` walks the full IR parent chain and yields
                    // `"axum.routing.Router.with_state"` — it includes the
                    // impl-type segment.
                    //
                    // `"axum.routing.Router.with_state".ends_with(
                    //       "axum.routing.with_state")` is FALSE, so the exact
                    // suffix check finds no winner when multiple symbols share
                    // the leaf name `with_state`.
                    //
                    // # Segment-level fallback
                    //
                    // When the exact suffix check finds no match we attempt a
                    // segment-level tiebreaker using the **label** (which is the
                    // author's path, e.g. `"Router::with_state"`).  We split the
                    // label into `::` segments and check whether each stored
                    // path contains **all** of those segments (case-folded, in
                    // order, though not necessarily contiguous — the impl-type
                    // node that the producer omits may sit between them).
                    //
                    // Example:
                    //   label segments   = ["Router", "with_state"]
                    //   stored path segs = ["axum", "routing", "Router", "with_state"]
                    //   → all label segs appear in order → match
                    //
                    //   stored path segs = ["axum", "handler", "Handler", "with_state"]
                    //   → "Router" not present → no match
                    //
                    // If more than one path still matches (unlikely but possible
                    // when two types share both a name and a method), we prefer
                    // the longer path (most-specific).  If nothing matches at
                    // all, we use the first hit as a last-ditch fallback rather
                    // than silently producing no link — one potentially-wrong
                    // link is usually better than no link.
                    let mut best: Option<(IntroId, usize)> = None;
                    for hit in &hits {
                        if let Some(stored_path) = package.indexes().path_of(hit.intro) {
                            let path_lower = stored_path.to_lowercase();
                            if path_lower.ends_with(&normalised_lower) {
                                let len = stored_path.len();
                                if best.map_or(true, |(_, prev_len)| len > prev_len) {
                                    best = Some((hit.intro, len));
                                }
                            }
                        }
                    }

                    if let Some((id, _)) = best {
                        id
                    } else {
                        // Exact suffix failed (producer path drops impl-type
                        // segment; moniker path includes it).  Fall back to
                        // segment-level matching using the label.
                        let label_segs_lower: Vec<String> = doc_link
                            .label
                            .as_deref()
                            .unwrap_or("")
                            .split("::")
                            .map(|s| s.to_lowercase())
                            .filter(|s| !s.is_empty())
                            .collect();

                        let mut seg_best: Option<(IntroId, usize)> = None;

                        if !label_segs_lower.is_empty() {
                            for hit in &hits {
                                if let Some(stored_path) = package.indexes().path_of(hit.intro) {
                                    let stored_segs: Vec<&str> =
                                        stored_path.split('.').collect();
                                    // Check that all label segments appear in
                                    // the stored path segments in order (but
                                    // not necessarily contiguous).
                                    //
                                    // `label_segs_lower` is already lowercased;
                                    // we lowercase each stored segment on the
                                    // fly for the comparison.
                                    let mut label_iter = label_segs_lower.iter();
                                    let mut current_label_seg = label_iter.next();
                                    for stored_seg in &stored_segs {
                                        if let Some(ls) = current_label_seg {
                                            if stored_seg.to_lowercase() == *ls {
                                                current_label_seg = label_iter.next();
                                            }
                                        }
                                    }
                                    if current_label_seg.is_none() {
                                        // All label segments matched in order.
                                        let len = stored_path.len();
                                        if seg_best
                                            .map_or(true, |(_, prev)| len > prev)
                                        {
                                            seg_best = Some((hit.intro, len));
                                        }
                                    }
                                }
                            }
                        }

                        // If label-segment matching also found nothing, use the
                        // first hit as a best-effort fallback: one potentially-
                        // imprecise link is less disruptive than no link at all.
                        // (The user can verify by clicking; a missing link offers
                        // no recourse.)
                        seg_best
                            .map(|(id, _)| id)
                            .unwrap_or_else(|| hits[0].intro)
                    }
                }
            };

            let key = nudox_ir::change::StableRef::new(package.lineage().clone(), resolved);

            // Register the resolved key under every alias a caller may use.
            //
            // (a) Original target as stored by the producer — covers direct
            //     lookups with the producer's exact string.
            inner.insert(target.to_owned(), key.clone());

            // (b) Normalised form (no tag, no anchor, dot-separated) — covers
            //     the case where a caller has already normalised.
            if normalised != target {
                inner.entry(normalised.clone()).or_insert(key.clone());
            }

            // (c) Bare leaf — so that `[with_state]` resolves even when the
            //     producer stored `"Router::with_state"` as the target.
            inner.entry(leaf.to_owned()).or_insert(key.clone());

            // (d) Display label if the producer supplied one.
            if let Some(label) = &doc_link.label {
                inner.entry(label.clone()).or_insert(key);
            }
        }

        Self { inner, declared }
    }

    /// Resolve a link target string to a `SymbolKey`, if known.
    ///
    /// Lookup cascade:
    ///
    /// 1. **Exact match** on the full target string (e.g. `"Router::with_state"`
    ///    as written in the doc text).
    /// 2. **Leaf from `::` split** — last segment after `::`.  Handles the case
    ///    where the author wrote a qualified path but the table only holds the leaf.
    /// 3. **Leaf from `.` split** — last segment after `.`.  Handles dot-path
    ///    forms emitted by producers that use moniker paths.
    ///
    /// Returns `None` when none of the three forms is present in the table.
    pub(crate) fn resolve(&self, target: &str) -> Option<&SymbolKey> {
        if let Some(key) = self.inner.get(target) {
            return Some(key);
        }
        // Try the leaf extracted by `::` splitting (the form authors write).
        if let Some(leaf) = target.rsplit("::").next() {
            if leaf != target {
                if let Some(key) = self.inner.get(leaf) {
                    return Some(key);
                }
            }
        }
        // Try the leaf extracted by `.` splitting (the moniker / dot-path form).
        if let Some(leaf) = target.rsplit('.').next() {
            if !leaf.is_empty() {
                return self.inner.get(leaf);
            }
        }
        None
    }

    /// `true` when no links were resolved.
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// `true` when the symbol declared intra-doc links, whether or not any of
    /// them resolved locally.
    ///
    /// This — not [`is_empty`](Self::is_empty) — is the correct gate for the
    /// bracket-rewriting lookahead. See the field docs on `declared`.
    pub(crate) fn has_declared_links(&self) -> bool {
        self.declared
    }
}

// ---------------------------------------------------------------------------
// walk_doc
// ---------------------------------------------------------------------------

/// Parse an entry's documentation and enumerate its children in one pass,
/// producing both rendered sections and their plan simultaneously.
///
/// `SectionId`s are assigned starting from 1.  The counter is the only shared
/// state between the `sections` and `plan` fields, so the §9.4 invariant is
/// guaranteed structurally — not by convention.
pub(crate) fn walk_doc(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
) -> WalkOutput {
    let mut id_counter: u32 = 0;

    let mut sections: Vec<RenderSection> = Vec::new();
    let mut plan: Vec<SectionPlan> = Vec::new();

    // ── 1. Parse markdown documentation ────────────────────────────────────

    let doc = entry.sym().documentation.as_str();
    if !doc.is_empty() {
        // Build the intra-doc link resolution table from the producer-resolved
        // `doc_links` on this symbol.  This is the authoritative resolution:
        // the producer (e.g. nudox-producer-rust via rust-analyzer) already ran
        // full name resolution in scope and stored the results in `doc_links`.
        // We do not re-parse or re-resolve here; we only index what was stored.
        let doc_link_table = DocLinkTable::build(entry, package);
        parse_markdown(doc, &doc_link_table, &mut id_counter, &mut sections, &mut plan);
    }

    // ── 2. Members section ─────────────────────────────────────────────────

    let children = view.children_of(intro);

    let member_children: Vec<(IntroId, MemberRow)> = children
        .iter()
        .filter_map(|&child_intro| {
            let child = view.entry(child_intro)?;
            let disc = child.kind().discriminant()?;
            let is_member = matches!(
                child.kind().as_owned_kind(),
                Some(Kind::Module(_) | Kind::Function(_) | Kind::Trait(_) | Kind::Impl(_))
            );
            if !is_member {
                return None;
            }
            let key = nudox_ir::change::StableRef::new(
                package.lineage().clone(),
                child_intro,
            );
            let sig = crate::chunk::signature::tokens(child, package);
            Some((
                child_intro,
                MemberRow {
                    key,
                    name: SharedStr::from(child.sym().name.as_str()),
                    sig,
                    kind: KindTag::Known(disc),
                    visibility: child.sym().visibility,
                },
            ))
        })
        .collect();

    if !member_children.is_empty() {
        id_counter += 1;
        let id = SectionId(id_counter);
        let row_count = member_children.len() as u32;
        let entries: Arc<[MemberRow]> = member_children
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .into();
        sections.push(RenderSection::Members { id, entries });
        plan.push(SectionPlan {
            id,
            kind: SectionKind::Members,
            size_hint: SizeHint::Rows(row_count),
        });
    }

    // ── 3. Fields section ──────────────────────────────────────────────────

    let field_children: Vec<FieldRow> = children
        .iter()
        .filter_map(|&child_intro| {
            let child = view.entry(child_intro)?;
            let disc = child.kind().discriminant()?;
            let is_field = matches!(
                child.kind().as_owned_kind(),
                Some(Kind::Field(_) | Kind::Variant(_))
            );
            if !is_field {
                return None;
            }
            let key = nudox_ir::change::StableRef::new(
                package.lineage().clone(),
                child_intro,
            );
            let ty_tokens = crate::chunk::signature::tokens(child, package);
            Some(FieldRow {
                key,
                name: SharedStr::from(child.sym().name.as_str()),
                ty_tokens,
                kind: KindTag::Known(disc),
            })
        })
        .collect();

    if !field_children.is_empty() {
        id_counter += 1;
        let id = SectionId(id_counter);
        let row_count = field_children.len() as u32;
        let entries: Arc<[FieldRow]> = field_children.into();
        sections.push(RenderSection::Fields { id, entries });
        plan.push(SectionPlan {
            id,
            kind: SectionKind::Fields,
            size_hint: SizeHint::Rows(row_count),
        });
    }

    WalkOutput { sections, plan }
}

// ---------------------------------------------------------------------------
// Markdown parsing helpers
// ---------------------------------------------------------------------------

/// Parse markdown `doc` into sections, appending to `sections` and `plan`.
///
/// Splits at H2 headings.  H1 headings become `ProseBlock::Heading` inside
/// the first prose section (they do not split).  Blockquotes with a
/// `[!NOTE]`-style lead become `RenderSection::Callout`.
///
/// # Code-block promotion (defect fix)
///
/// Each **fenced** code block inside a logical section is promoted to its own
/// `RenderSection::CodeBlock` with a freshly-assigned `SectionId`.  The prose
/// that surrounds the fences is collected into separate `RenderSection::Prose`
/// (or `Examples`/`Callout`) sections, one per contiguous run of non-fence
/// events.
///
/// Without this split the entire logical section — heading, prose paragraphs,
/// and all fences — went into a single `RenderSection::Prose` as a flat
/// `Vec<ProseBlock>`.  The GUI renders sections independently and, when handed
/// one large Prose section, would display only the first `ProseBlock::Code` it
/// encountered before hitting a rendering boundary; every subsequent fence was
/// lost.  Promoting each fence to a top-level section makes them independently
/// navigable and matches the `SectionKind::CodeBlock` entry that already exists
/// in the wire vocabulary.
///
/// The one-walk guarantee (§9.4) is preserved: every `id_counter` increment is
/// paired with exactly one `plan.push` and one `sections.push` inside this loop,
/// so plan and sections can never diverge in length or id assignment.
fn parse_markdown(
    doc: &str,
    doc_link_table: &DocLinkTable,
    id_counter: &mut u32,
    sections: &mut Vec<RenderSection>,
    plan: &mut Vec<SectionPlan>,
) {
    let parser = Parser::new_ext(doc, Options::ENABLE_STRIKETHROUGH);

    // Accumulate raw events into logical sections.
    // Each logical section is: (heading_text: Option<String>, events)
    // where heading_text is the H2 that started this section.
    let mut current_heading: Option<String> = None;
    let mut current_events: Vec<Event<'_>> = Vec::new();
    let mut logical_sections: Vec<(Option<String>, Vec<Event<'_>>)> = Vec::new();

    // Collect a heading text from a run of Text events until EndTag.
    let mut in_heading: Option<HeadingLevel> = None;
    let mut heading_buf = String::new();

    for event in parser {
        match &event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = *level;
                in_heading = Some(lvl);
                heading_buf.clear();
                if lvl != HeadingLevel::H2 {
                    // Non-H2 headings belong in the section body.
                    current_events.push(event);
                }
                // H2 headings are split markers — we do NOT push them into
                // current_events; they are captured in heading_buf only.
            }
            Event::End(TagEnd::Heading(level)) => {
                if in_heading == Some(*level) && *level == HeadingLevel::H2 {
                    // H2 ends the previous section and starts a new one.
                    // Flush the current section (even if empty — a None heading
                    // with no events is silently dropped by the flush below).
                    if !current_events.is_empty() || current_heading.is_some() {
                        logical_sections.push((
                            current_heading.take(),
                            std::mem::take(&mut current_events),
                        ));
                    }
                    current_heading = Some(heading_buf.trim().to_owned());
                    heading_buf.clear();
                    in_heading = None;
                    // Don't push the EndTag — the heading is now the section key.
                } else {
                    // Non-H2 heading ended.
                    in_heading = None;
                    heading_buf.clear();
                    current_events.push(event);
                }
            }
            Event::Text(text) => {
                if in_heading.is_some() {
                    // Accumulate heading text in the buffer.
                    heading_buf.push_str(text);
                    if in_heading != Some(HeadingLevel::H2) {
                        // Non-H2 heading text belongs in the section body too.
                        current_events.push(event);
                    }
                    // H2 text is captured in heading_buf only — not in events.
                } else {
                    current_events.push(event);
                }
            }
            Event::Code(text) => {
                if in_heading.is_some() {
                    heading_buf.push_str(text);
                    if in_heading != Some(HeadingLevel::H2) {
                        current_events.push(event);
                    }
                } else {
                    current_events.push(event);
                }
            }
            _ => {
                current_events.push(event);
            }
        }
    }

    // Flush the final section.
    if !current_events.is_empty() || current_heading.is_some() {
        logical_sections.push((current_heading.take(), current_events));
    }

    // Convert logical sections into RenderSections.
    //
    // For each logical section we first split its event list at fenced code
    // block boundaries (see `split_prose_and_code_blocks`).  Each contiguous
    // run of non-fence events becomes one prose/callout/examples section; each
    // fence becomes one `RenderSection::CodeBlock`.  The H2 heading text, if
    // any, is attached to the FIRST prose slice only — subsequent slices within
    // the same logical section carry no heading.
    for (heading, events) in logical_sections {
        let heading_text = heading.as_deref().unwrap_or("");

        // Split the event stream into alternating prose and code-block slices.
        // `true` in the second field means "this slice is a standalone fence".
        let slices: Vec<(Vec<Event<'_>>, bool)> = split_at_code_blocks(events);

        // Whether we have already attached the heading to the first prose slice.
        let mut heading_used = false;

        for (slice_events, is_code_block) in slices {
            if is_code_block {
                // Extract the code block's lang and text from the three events:
                //   Start(CodeBlock(kind)), Text(content), End(CodeBlock)
                // The events were produced by pulldown-cmark and are guaranteed
                // to have this shape by `split_at_code_blocks`.
                let (lang_str, text_str) = extract_code_block_content(&slice_events);

                let line_count =
                    text_str.chars().filter(|&c| c == '\n').count() as u32;
                let lang_id = LangId(SharedStr::from(if lang_str.is_empty() {
                    "text"
                } else {
                    &lang_str
                }));
                let text_shared = SharedStr::from(text_str.trim_end());

                *id_counter += 1;
                let id = SectionId(*id_counter);

                plan.push(SectionPlan {
                    id,
                    kind: SectionKind::CodeBlock,
                    size_hint: SizeHint::Lines(line_count.max(1)),
                });
                sections.push(RenderSection::CodeBlock {
                    id,
                    lang: lang_id,
                    text: text_shared,
                    line_count,
                });
            } else {
                // Prose slice: determine the effective heading (only on the
                // first prose slice of this logical section).
                let effective_heading = if !heading_used {
                    heading_used = true;
                    heading_text
                } else {
                    ""
                };

                // Skip empty prose slices that carry neither events nor a
                // heading (e.g. a logical section that was entirely a code
                // block, leaving an empty leading/trailing prose slice).
                if slice_events.is_empty() && effective_heading.is_empty() {
                    continue;
                }

                let callout_level = detect_callout_level(&slice_events);

                let kind = if callout_level.is_some() {
                    SectionKind::Callout
                } else if effective_heading == "Example"
                    || effective_heading == "Examples"
                {
                    SectionKind::Examples
                } else {
                    SectionKind::Prose
                };

                *id_counter += 1;
                let id = SectionId(*id_counter);

                // Build blocks from the prose events (with the heading
                // prepended when present).
                //
                // Deriving the hint from `blocks` is the zero-jump guarantee
                // (§9.4): the plan and the rendered content are measured from
                // the same data, so skeleton height can never disagree with
                // content height.
                let blocks = build_prose_blocks(
                    slice_events,
                    effective_heading,
                    doc_link_table,
                );

                plan.push(SectionPlan {
                    id,
                    kind,
                    size_hint: SizeHint::Lines(prose_block_lines(&blocks)),
                });

                match kind {
                    SectionKind::Callout => {
                        let level = callout_level.unwrap_or(CalloutLevel::Unknown);
                        sections.push(RenderSection::Callout { id, level, blocks });
                    }
                    SectionKind::Examples => {
                        sections.push(RenderSection::Examples { id, blocks });
                    }
                    _ => {
                        sections.push(RenderSection::Prose { id, blocks });
                    }
                }
            }
        }
    }
}

/// Split a flat pulldown-cmark event stream at **fenced** code block
/// boundaries.
///
/// Returns a list of `(events, is_code_block)` pairs.  Each pair where
/// `is_code_block` is `true` contains exactly the three events
/// `[Start(CodeBlock), Text(content), End(CodeBlock)]` for one fence.
/// Pairs where `is_code_block` is `false` contain the surrounding prose
/// events.
///
/// # Why this split is necessary
///
/// Before this function existed, a logical section that contained both prose
/// and fenced code blocks was handed to `build_prose_blocks` as one flat event
/// slice.  `build_prose_blocks` emits `ProseBlock::Code` for each fence — all
/// of them embedded inside a single `RenderSection::Prose`.  The GUI renders
/// sections independently, and its code-fence renderer is attached to
/// `RenderSection::CodeBlock`, not to `ProseBlock::Code` inside a `Prose`
/// section.  The result was that only the first fence (or none of them)
/// received syntax highlighting, and subsequent fences were rendered as opaque
/// grey boxes that the user could not distinguish from ordinary pre-formatted
/// text.
///
/// Promoting each fence to `RenderSection::CodeBlock` gives it its own
/// `SectionId`, makes it independently navigable, and ensures the async
/// `DocEvent::Highlight` upgrade targets the correct section.
///
/// Indented code blocks are NOT promoted: they are a legacy Markdown feature
/// and rustdoc itself does not treat them as primary code examples.  They
/// continue to render as `ProseBlock::Code` inside the surrounding prose
/// section.
fn split_at_code_blocks(events: Vec<Event<'_>>) -> Vec<(Vec<Event<'_>>, bool)> {
    let mut slices: Vec<(Vec<Event<'_>>, bool)> = Vec::new();
    let mut prose_buf: Vec<Event<'_>> = Vec::new();

    // Consume the vector via a peekable iterator so we never need to
    // index into it by hand.  `pulldown_cmark::Event<'_>` is `Clone`;
    // using an iterator avoids any borrowing gymnastics with index-based
    // access into a partially-consumed vector.
    let mut iter = events.into_iter().peekable();

    while let Some(ev) = iter.next() {
        // Detect the start of a fenced (not indented) code block.
        let is_fenced_start = matches!(
            &ev,
            Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(_)))
        );

        if is_fenced_start {
            // Flush any accumulated prose events first.
            if !prose_buf.is_empty() {
                slices.push((std::mem::take(&mut prose_buf), false));
            }

            // Collect events until and including End(CodeBlock).
            // In a well-formed pulldown-cmark stream this is always:
            //   Start(CodeBlock)   ← already consumed as `ev`
            //   Text(content)      ← may be absent for empty fences
            //   End(CodeBlock)
            //
            // We scan forward rather than assuming the exact count so that
            // empty fences and multi-line fences (multiple Text events) are
            // handled uniformly.
            let mut fence_events: Vec<Event<'_>> = vec![ev];

            for fence_ev in iter.by_ref() {
                let is_end = matches!(&fence_ev, Event::End(TagEnd::CodeBlock));
                fence_events.push(fence_ev);
                if is_end {
                    break;
                }
            }

            slices.push((fence_events, true));
        } else {
            prose_buf.push(ev);
        }
    }

    // Flush any trailing prose.
    if !prose_buf.is_empty() {
        slices.push((prose_buf, false));
    }

    slices
}

/// Extract `(lang_str, text_str)` from a fence event slice produced by
/// `split_at_code_blocks`.
///
/// The slice must contain `[Start(CodeBlock(kind)), ..Text.., End(CodeBlock)]`.
/// `lang_str` is the info string from the fenced code block delimiter;
/// `text_str` is the concatenated text content.
fn extract_code_block_content(events: &[Event<'_>]) -> (String, String) {
    let mut lang_str = String::new();
    let mut text_str = String::new();

    for ev in events {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                lang_str = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => info.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
            }
            Event::Text(t) => {
                text_str.push_str(t);
            }
            _ => {}
        }
    }

    (lang_str, text_str)
}

/// Detect whether a list of events represents a callout blockquote.
///
/// A callout begins with `Start(BlockQuote)` and the first text inside is
/// `[!NOTE]`, `[!WARNING]`, `[!DANGER]`, or `[!TIP]`.
fn detect_callout_level(events: &[Event<'_>]) -> Option<CalloutLevel> {
    let mut in_blockquote = false;
    let mut collected_text = String::new();

    for ev in events {
        match ev {
            Event::Start(Tag::BlockQuote(_)) => {
                in_blockquote = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if in_blockquote {
                    break;
                }
            }
            Event::Text(text) if in_blockquote => {
                collected_text.push_str(text);
                if collected_text.len() > 20 {
                    break;
                }
            }
            _ => {}
        }
    }

    let trimmed = collected_text.trim();
    if trimmed.starts_with("[!NOTE]") {
        Some(CalloutLevel::Note)
    } else if trimmed.starts_with("[!WARNING]") {
        Some(CalloutLevel::Warning)
    } else if trimmed.starts_with("[!DANGER]") {
        Some(CalloutLevel::Danger)
    } else if trimmed.starts_with("[!TIP]") {
        Some(CalloutLevel::Tip)
    } else {
        None
    }
}

/// Rough line count for SizeHint — counts paragraph and code-fence boundaries.
/// Display-line count for a built section body.
///
/// This is the *definition* of a prose section's height, used both to fill
/// `SectionPlan::size_hint` and — necessarily identically — by the GUI when it
/// lays the real content out. One block is one line, except lists (one line per
/// item) and code (its own line count). Keep this in step with the renderer:
/// if the GUI ever lays a block out differently, this function is what has to
/// change, and changing it here fixes the skeleton at the same time.
fn prose_block_lines(blocks: &[ProseBlock]) -> u32 {
    blocks
        .iter()
        .map(|b| match b {
            ProseBlock::List { items, .. } => items.len().max(1) as u32,
            ProseBlock::Code { line_count, .. } => *line_count,
            _ => 1,
        })
        .sum::<u32>()
        .max(1)
}

// ---------------------------------------------------------------------------
// ProseBlock builder
// ---------------------------------------------------------------------------

/// Convert a stream of pulldown-cmark events into `ProseBlock`s.
///
/// The heading text is prepended as a `Heading` block when non-empty (it was
/// an H2 that started this section and was not included in the event stream).
///
/// # Rustdoc shortcut-link detection at the event-stream level
///
/// pulldown-cmark 0.13 does **not** produce a `Tag::Link` event for rustdoc
/// intra-doc shortcut references like `[child_fn]`, `[Router::with_state]`, or
/// `` [`Router::with_state`] ``.  These are *shortcut references* with no
/// corresponding link-definition line, so pulldown-cmark emits them as a
/// sequence of plain `Text` and `Code` events instead:
///
/// ```text
/// "See [child_fn] for details."
///   Start(Paragraph)
///   Text("See ")
///   Text("[")
///   Text("child_fn")
///   Text("]")
///   Text(" for details.")
///   End(Paragraph)
///
/// "See [`Router::with_state`] for more details."
///   Start(Paragraph)
///   Text("See ")
///   Text("[")
///   Code("Router::with_state")   ← backtick form becomes Code event
///   Text("]")
///   Text(" for more details.")
///   End(Paragraph)
/// ```
///
/// No single `Text` event ever contains the full `[inner]` span.  An earlier
/// attempt guarded expansion on `text.contains('[')`, which never fired in
/// practice because `"["` and `"inner"` arrive as separate events.
///
/// The fix is to detect the bracket sequence at the **event-stream** level
/// using lookahead in an index-based loop rather than a simple `for` loop.
/// When we see `Text("[")` at index `i`, we look ahead:
///
/// - `events[i+1]` must be `Text(inner)` or `Code(inner)` (the symbol path).
/// - `events[i+2]` must be a `Text` that **starts with** `"]"`.
/// - `inner` must pass `is_symbol_path`.
///
/// If all conditions hold and `doc_link_table.resolve(inner)` succeeds, we
/// emit one `InlineRun::Link` and advance `i` by 3 (consuming all three
/// events).  If resolution fails, we emit the inner text as `Code` (backtick
/// form) or plain `Text` (bare form) and advance by 3, stripping the brackets
/// — this is "better than nothing": the reader still sees the name without the
/// confusing literal brackets.  If the closing `Text` fuses trailing prose
/// (e.g. `"] for details."`) we split it: emit the link/text run for the
/// resolved inner, then emit the remainder after `]` as a separate `Text` run.
///
/// When the three-event pattern does not match (e.g. `[!NOTE]` callout leads,
/// `[1]` footnote-style references, an unclosed `[`) we fall through to the
/// per-event path and emit the `"["` literally — existing behaviour is
/// preserved, and the `[!NOTE]` callout test keeps passing.
fn build_prose_blocks(
    events: Vec<Event<'_>>,
    section_heading: &str,
    doc_link_table: &DocLinkTable,
) -> Vec<ProseBlock> {
    let mut blocks: Vec<ProseBlock> = Vec::new();

    if !section_heading.is_empty() {
        blocks.push(ProseBlock::Heading {
            level: 2,
            runs: vec![crate::wire::InlineRun::Text { text: SharedStr::from(section_heading) }],
        });
    }

    let mut state = BlockState::None;
    // Inline run accumulator for the current paragraph / list item.
    let mut inline_buf: Vec<crate::wire::InlineRun> = Vec::new();
    // List state
    let mut list_ordered: bool = false;
    let mut list_items: Vec<Vec<crate::wire::InlineRun>> = Vec::new();
    // Code fence state
    let mut code_lang = String::new();
    let mut code_text = String::new();
    // Heading level
    let mut heading_level: u8 = 1;
    // Nested blockquote text buffer
    let mut blockquote_inline: Vec<crate::wire::InlineRun> = Vec::new();
    let mut in_blockquote = false;
    // Strong/em nesting
    let mut in_strong = false;
    let mut in_em = false;
    // Link state
    let mut link_url: Option<String> = None;
    let mut link_text_buf = String::new();

    // Use index-based iteration so we can perform lookahead for the
    // split-bracket shortcut-link pattern described in the doc comment above.
    let n = events.len();
    let mut i = 0;
    while i < n {
        // ── Shortcut-link lookahead (event-stream level) ───────────────────
        //
        // Attempt to recognise the three-event bracket sequence emitted by
        // pulldown-cmark for an unresolved shortcut reference.  We only try
        // this when:
        //   (a) the doc_link_table is non-empty (guard the common case where
        //       there are no intra-doc links at all — most entries),
        //   (b) we are inside a paragraph/heading/list context where an inline
        //       run makes sense (not inside a code fence),
        //   (c) the current event is `Text("[")` exactly,
        //   (d) we are not inside an explicit `Tag::Link` (link_url.is_some()),
        //       because pulldown-cmark handles those correctly and we must not
        //       interfere with the link text accumulation.
        //
        // The three-event pattern:
        //   events[i]   = Text("[")
        //   events[i+1] = Text(inner) OR Code(inner)  — the symbol name
        //   events[i+2] = Text(s) where s.starts_with("]")
        //
        // `inner` must pass `is_symbol_path`.  If i+2 is out of bounds the
        // pattern does not match and we fall through to single-event handling.
        if doc_link_table.has_declared_links()
            && state != BlockState::Code
            && link_url.is_none()
            && i + 2 < n
        {
            if let Event::Text(open_text) = &events[i] {
                if open_text.as_ref() == "[" {
                    // Peek at events[i+1] and events[i+2].
                    let inner_event = &events[i + 1];
                    let close_event = &events[i + 2];

                    // Extract the inner string and whether it was a Code event.
                    let inner_opt: Option<(&str, bool)> = match inner_event {
                        Event::Text(t) => Some((t.as_ref(), false)),
                        Event::Code(t) => Some((t.as_ref(), true)),
                        _ => None,
                    };

                    // The closing event must be a Text that begins with "]".
                    let close_opt: Option<&str> = match close_event {
                        Event::Text(t) if t.as_ref().starts_with(']') => Some(t.as_ref()),
                        _ => None,
                    };

                    if let (Some((inner, is_code)), Some(close_str)) =
                        (inner_opt, close_opt)
                    {
                        if is_symbol_path(inner) {
                            // Pattern matched.  Determine any trailing text
                            // fused onto the closing "]" (e.g. "] for details."
                            // → remainder = " for details.").
                            let remainder = &close_str[1..]; // strip the leading "]"

                            let run = match doc_link_table.resolve(inner) {
                                Some(key) => {
                                    // Resolved: emit a clickable Symbol link.
                                    // Display text is the raw inner string as
                                    // the author wrote it (bare or backtick).
                                    crate::wire::InlineRun::Link {
                                        text: SharedStr::from(inner),
                                        target: LinkTarget::Symbol { key: key.clone() },
                                    }
                                }
                                None => {
                                    // Unresolvable (cross-crate or unknown).
                                    // Emit without brackets — seeing the name
                                    // without brackets is less confusing than
                                    // the literal `[Router::with_state]` text.
                                    // Use Code for the backtick form to
                                    // preserve the author's monospace intent.
                                    if is_code {
                                        crate::wire::InlineRun::Code {
                                            text: SharedStr::from(inner),
                                        }
                                    } else {
                                        make_text_run(inner, in_strong, in_em)
                                    }
                                }
                            };

                            push_inline(
                                &mut inline_buf,
                                &mut blockquote_inline,
                                in_blockquote,
                                run,
                            );

                            // If the closing Text had trailing prose fused onto
                            // it (beyond the `]`), emit that as a separate run.
                            if !remainder.is_empty() {
                                let trailing = make_text_run(remainder, in_strong, in_em);
                                push_inline(
                                    &mut inline_buf,
                                    &mut blockquote_inline,
                                    in_blockquote,
                                    trailing,
                                );
                            }

                            // Consumed three events: "[", inner, "]...".
                            i += 3;
                            continue;
                        }
                    }
                    // Pattern did not match (inner not a symbol path, or
                    // close_event not a Text starting with "]").  Fall through
                    // to the single-event match below which will emit "[" as
                    // literal text — preserving `[!NOTE]` callout behaviour.
                }
            }
        }

        // ── Single-event processing (unchanged from original) ──────────────
        let event = &events[i];
        match event {
            // ── Block-level starts ─────────────────────────────────────────

            Event::Start(Tag::Paragraph) => {
                state = BlockState::Paragraph;
                inline_buf.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                if state == BlockState::Paragraph && !inline_buf.is_empty() {
                    blocks.push(ProseBlock::Paragraph { runs: std::mem::take(&mut inline_buf) });
                }
                state = BlockState::None;
            }

            Event::Start(Tag::Heading { level, .. }) => {
                state = BlockState::Heading;
                heading_level = heading_level_to_u8(*level);
                inline_buf.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if state == BlockState::Heading && !inline_buf.is_empty() {
                    blocks.push(ProseBlock::Heading {
                        level: heading_level,
                        runs: std::mem::take(&mut inline_buf),
                    });
                }
                state = BlockState::None;
            }

            Event::Start(Tag::List(start_num)) => {
                list_ordered = start_num.is_some();
                list_items.clear();
                state = BlockState::List;
            }
            Event::End(TagEnd::List(_)) => {
                if state == BlockState::List {
                    blocks.push(ProseBlock::List {
                        ordered: list_ordered,
                        items: std::mem::take(&mut list_items),
                    });
                }
                state = BlockState::None;
            }
            Event::Start(Tag::Item) => {
                inline_buf.clear();
            }
            Event::End(TagEnd::Item) => {
                list_items.push(std::mem::take(&mut inline_buf));
            }

            Event::Rule => {
                blocks.push(ProseBlock::Rule);
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => info.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                code_text.clear();
                state = BlockState::Code;
            }
            Event::End(TagEnd::CodeBlock) => {
                let line_count = code_text.chars().filter(|&c| c == '\n').count() as u32;
                let lang_id = LangId(SharedStr::from(if code_lang.is_empty() {
                    "text"
                } else {
                    &code_lang
                }));
                blocks.push(ProseBlock::Code {
                    lang: lang_id,
                    text: SharedStr::from(code_text.trim_end()),
                    line_count,
                });
                code_text.clear();
                state = BlockState::None;
            }

            Event::Start(Tag::BlockQuote(_)) => {
                in_blockquote = true;
                blockquote_inline.clear();
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                in_blockquote = false;
                if !blockquote_inline.is_empty() {
                    blocks.push(ProseBlock::Paragraph { runs: std::mem::take(&mut blockquote_inline) });
                }
            }

            // ── Inline starts ──────────────────────────────────────────────

            Event::Start(Tag::Strong) => {
                in_strong = true;
            }
            Event::End(TagEnd::Strong) => {
                in_strong = false;
            }
            Event::Start(Tag::Emphasis) => {
                in_em = true;
            }
            Event::End(TagEnd::Emphasis) => {
                in_em = false;
            }

            // Explicit links: `[text](dest_url)` or `[text][ref]` that
            // pulldown-cmark did resolve (either a real URL or a reference with
            // a matching definition).
            //
            // When `dest_url` looks like a symbol path (no `://`, starts with a
            // letter or `_`, contains only alnums / `_` / `:` / `.`) we try the
            // doc_link_table first.  This handles the rare case where the
            // producer stored the link both as a `doc_link` and pulldown-cmark
            // also saw it as a link via some other mechanism.  For ordinary URLs
            // (`https://...`) we always emit `LinkTarget::Url` unchanged.
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_url = Some(dest_url.to_string());
                link_text_buf.clear();
            }
            Event::End(TagEnd::Link) => {
                if let Some(url) = link_url.take() {
                    let target = if is_symbol_path(&url) {
                        // Try the doc_link_table for paths that look like Rust
                        // identifiers / paths.  If it resolves, emit a Symbol
                        // link; otherwise fall back to a URL link (which might
                        // be wrong, but is less bad than a dead link).
                        if let Some(key) = doc_link_table.resolve(&url) {
                            LinkTarget::Symbol { key: key.clone() }
                        } else {
                            LinkTarget::Url { url: SharedStr::from(url.as_str()) }
                        }
                    } else {
                        LinkTarget::Url { url: SharedStr::from(url.as_str()) }
                    };
                    let run = crate::wire::InlineRun::Link {
                        text: SharedStr::from(link_text_buf.as_str()),
                        target,
                    };
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
                link_text_buf.clear();
            }

            // ── Text and code ──────────────────────────────────────────────

            Event::Text(text) => {
                if state == BlockState::Code {
                    code_text.push_str(text);
                } else if link_url.is_some() {
                    link_text_buf.push_str(text);
                } else {
                    // Plain text in a prose context.  The shortcut-link
                    // lookahead above handles the split-bracket pattern; by
                    // the time we reach here the text token is either an
                    // ordinary prose fragment, a standalone `[` that did NOT
                    // match the three-event pattern (e.g. `[!NOTE]` callout),
                    // or text that appeared after the lookahead already
                    // consumed the bracket triplet.  No further bracket
                    // scanning is needed here.
                    let run = make_text_run(text, in_strong, in_em);
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
            }

            Event::Code(text) => {
                // A bare `Code` event (backtick inline) that was NOT part of
                // a shortcut-link triplet (the lookahead would have consumed
                // it if it were).  Emit as a code run.
                let run = crate::wire::InlineRun::Code { text: SharedStr::from(text.as_ref()) };
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }

            Event::SoftBreak | Event::HardBreak => {
                let run = crate::wire::InlineRun::Text { text: SharedStr::from(" ") };
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }

            _ => {}
        }

        i += 1;
    }

    blocks
}

// ---------------------------------------------------------------------------
// Shortcut-link expansion (string-level utility — no longer on the hot path)
// ---------------------------------------------------------------------------

/// Expand rustdoc-style shortcut links inside a single pre-joined text string.
///
/// # IMPORTANT — this function is NOT called by `build_prose_blocks`
///
/// pulldown-cmark 0.13 splits an unresolved shortcut reference across three
/// separate events:
///
/// ```text
/// Text("[")  /  Text("inner") or Code("inner")  /  Text("]...")
/// ```
///
/// No single `Text` event ever contains the full `[inner]` span, so a string-
/// scanning approach applied per-event can never fire.  The actual hot-path
/// detection lives in `build_prose_blocks` as an indexed lookahead over the
/// event slice (search for "Shortcut-link lookahead" in that function).
///
/// This function remains for two purposes:
/// 1. **Unit tests** — the isolated tests below verify its logic directly
///    against hand-crafted strings.  They continue to document the intended
///    behaviour of each case (bracket stripping, callout preservation, etc.)
///    and serve as regression guards if `expand_shortcut_links` is ever wired
///    back in for a different code path.
/// 2. **Future use** — if a producer ever emits a single `Text` event
///    containing the full `[Foo]` span (possible under different parser
///    versions or different input), wiring this function back in would be a
///    one-line change.
///
/// # Behaviour
///
/// Scans `text` for the pattern `[<target>]` where `<target>` is a valid
/// symbol path (no spaces, no `://`).  For each match:
///
/// - If `doc_link_table.resolve(target)` succeeds, the pattern becomes
///   `InlineRun::Link { target: LinkTarget::Symbol { key } }`.
/// - If resolution fails, the pattern becomes `InlineRun::Text` (or `Code`
///   for backtick-quoted inner text) with the brackets stripped.
///
/// Text that does not match any pattern is returned as-is.  `[!NOTE]` callout
/// leads survive verbatim because `!NOTE` fails `is_symbol_path`.
fn expand_shortcut_links(
    text: &str,
    doc_link_table: &DocLinkTable,
    in_strong: bool,
    in_em: bool,
) -> Vec<crate::wire::InlineRun> {
    let mut runs: Vec<crate::wire::InlineRun> = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find('[') {
        // Emit everything before the `[` as plain text.
        if open > 0 {
            let before = &rest[..open];
            runs.push(make_text_run(before, in_strong, in_em));
        }

        // Locate the matching `]`.  A `]` that is followed by `(` or `[` is
        // part of a real Markdown link syntax and should not be processed here
        // (pulldown-cmark would have emitted a proper Link event for those).
        // We look for a `]` that is NOT followed by `(` or `[`.
        let after_open = &rest[open + 1..];
        let Some(close_rel) = after_open.find(']') else {
            // No closing bracket — emit the rest as plain text and stop.
            runs.push(make_text_run(rest, in_strong, in_em));
            return runs;
        };
        let close = open + 1 + close_rel;
        let after_close = &rest[close + 1..];

        // If followed by `(` or `[`, this is a real Markdown link that
        // pulldown-cmark should have handled.  Skip over the `[` and continue
        // scanning so we don't corrupt already-correct links.
        if after_close.starts_with('(') || after_close.starts_with('[') {
            // Keep the `[` as a literal character; advance past it.
            runs.push(make_text_run("[", in_strong, in_em));
            rest = &rest[open + 1..];
            continue;
        }

        let inner = &rest[open + 1..close];

        // Strip backticks for resolution (`` [`Foo`] `` → `Foo`).
        let inner_stripped = inner.trim().trim_start_matches('`').trim_end_matches('`').trim();

        // Only attempt resolution for tokens that look like symbol paths.
        if !is_symbol_path(inner_stripped) {
            // Not a path — emit the whole bracketed span as plain text (with
            // brackets) because it might be a callout lead like `[!NOTE]`.
            let span = &rest[open..=close];
            runs.push(make_text_run(span, in_strong, in_em));
            rest = &rest[close + 1..];
            continue;
        }

        match doc_link_table.resolve(inner_stripped) {
            Some(key) => {
                // Resolved: emit a clickable Symbol link.  The display text is
                // the raw inner string (with backticks if present) so the
                // rendered link text matches what the author wrote.
                runs.push(crate::wire::InlineRun::Link {
                    text: SharedStr::from(inner),
                    target: LinkTarget::Symbol { key: key.clone() },
                });
            }
            None => {
                // Unresolvable (cross-crate or unknown): emit the inner text
                // without brackets.  This is the "better than nothing" policy:
                // the reader still sees the symbol name; they just cannot click
                // it.  The brackets are stripped because leaving them in is the
                // bug we are fixing — `[tower::Service]` as literal text is
                // confusing; `tower::Service` as plain prose is clear.
                //
                // We use `Code` when the inner text was backtick-quoted (i.e.
                // the author wrote `` [`Foo`] ``), and plain `Text` otherwise.
                // This mirrors how rustdoc itself renders unresolvable links.
                let run = if inner.starts_with('`') && inner.ends_with('`') && inner.len() > 1 {
                    crate::wire::InlineRun::Code {
                        text: SharedStr::from(inner_stripped),
                    }
                } else {
                    make_text_run(inner_stripped, in_strong, in_em)
                };
                runs.push(run);
            }
        }

        rest = &rest[close + 1..];
    }

    // Emit any trailing text after the last `]`.
    if !rest.is_empty() {
        runs.push(make_text_run(rest, in_strong, in_em));
    }

    runs
}

/// Produce a plain/strong/em `InlineRun` for a text fragment.
///
/// The strong/em wrapping mirrors the surrounding span state so that text
/// fragments inside `**...**` keep their bold formatting even after shortcut
/// link expansion splits them.
#[inline]
fn make_text_run(text: &str, in_strong: bool, in_em: bool) -> crate::wire::InlineRun {
    if in_strong {
        crate::wire::InlineRun::Strong { text: SharedStr::from(text) }
    } else if in_em {
        crate::wire::InlineRun::Em { text: SharedStr::from(text) }
    } else {
        crate::wire::InlineRun::Text { text: SharedStr::from(text) }
    }
}

/// Return `true` when `s` looks like a Rust symbol path rather than a URL or
/// prose.
///
/// A symbol path:
/// - Is non-empty.
/// - Does not contain `://` (which would make it a URL).
/// - Does not contain whitespace.
/// - Consists only of alphanumerics, `_`, `:`, and `.`.
/// - Starts with a letter or `_`.
///
/// This is intentionally conservative: it must not match URL protocols or
/// prose phrases, but it does not need to validate that the path is
/// syntactically legal Rust — any false positives are handled by the resolution
/// step returning `None`.
fn is_symbol_path(s: &str) -> bool {
    if s.is_empty() || s.contains("://") || s.contains(char::is_whitespace) {
        return false;
    }
    s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn push_inline(
    buf: &mut Vec<crate::wire::InlineRun>,
    bq_buf: &mut Vec<crate::wire::InlineRun>,
    in_bq: bool,
    run: crate::wire::InlineRun,
) {
    if in_bq {
        bq_buf.push(run);
    } else {
        buf.push(run);
    }
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[derive(PartialEq)]
enum BlockState {
    None,
    Paragraph,
    Heading,
    List,
    Code,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        entry::{DocLink, Entry, Node, Symbol, Visibility},
        kind::Kind,
        kinds::Module,
        view::IrView,
    };
    use nudox_store::package::{PackageView, Provenance};

    use super::*;
    use crate::wire::{InlineRun, LinkTarget, ProseBlock, RenderSection};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(
            EcosystemId::new("test"),
            PackageName::new("test-walk"),
        )
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sym_with_doc_links(name: &str, doc: &str, links: Vec<DocLink>) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: doc.to_owned(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: links.into_boxed_slice(),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    /// Build a two-entry `PackageView`: a root module and one child function.
    ///
    /// `child_name` is the name of the child function (e.g. `"with_state"`).
    /// Returns `(PackageView, child_intro)`.
    fn build_pkg_with_child(child_name: &str) -> (PackageView, IntroId) {
        let root_id = intro(1);
        let child_id = intro(2);
        let mut table = PristineIntroTable::new();

        let root_sym = Symbol {
            name: "root".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let child_sym = Symbol {
            name: child_name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            child_id,
            Entry::new(child_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);
        (pkg, child_id)
    }

    // ── DocLinkTable tests ────────────────────────────────────────────────────

    /// A resolved `DocLink` for `with_state` inside `root` must map to the
    /// correct `SymbolKey` via both the full path and the bare leaf.
    #[test]
    fn doc_link_table_resolves_leaf_and_full_path() {
        let (pkg, child_id) = build_pkg_with_child("with_state");

        // Simulate what the Rust producer stores:
        // target = "Router::with_state", label = Some("Router::with_state")
        let doc_links = vec![DocLink {
            target: "Router::with_state".to_owned(),
            label: Some("Router::with_state".to_owned()),
        }];

        // Build a fake symbol just for the table builder.
        let sym = sym_with_doc_links("Router", "", doc_links);
        let root_entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&root_entry, &pkg);

        // The expected key is child_id under our test lineage.
        let expected_key = StableRef::new(lineage(), child_id);

        // Resolution via full target string.
        assert_eq!(
            table.resolve("Router::with_state"),
            Some(&expected_key),
            "full path must resolve"
        );

        // Resolution via bare leaf.
        assert_eq!(
            table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must resolve"
        );
    }

    /// A link whose leaf is not in the corpus must not resolve.
    #[test]
    fn doc_link_table_cross_crate_resolves_to_none() {
        let (pkg, _) = build_pkg_with_child("local_fn");

        let doc_links = vec![DocLink {
            target: "tower::Service".to_owned(),
            label: Some("Service".to_owned()),
        }];

        let sym = sym_with_doc_links("MyStruct", "", doc_links);
        let entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&entry, &pkg);

        // "Service" is not in the corpus.
        assert_eq!(table.resolve("tower::Service"), None);
        assert_eq!(table.resolve("Service"), None);
    }

    // ── expand_shortcut_links tests ───────────────────────────────────────────

    /// Build a minimal `DocLinkTable` from a hand-built `(name, SymbolKey)` map
    /// for use in `expand_shortcut_links` tests without a full `PackageView`.
    fn make_table(entries: &[(&str, IntroId)]) -> DocLinkTable {
        let mut inner = HashMap::new();
        for (name, id) in entries {
            let key = StableRef::new(lineage(), *id);
            inner.insert(name.to_string(), key.clone());
        }
        // A hand-built table stands in for a symbol that *declared* links, so
        // `declared` is true whenever any entry was supplied.
        let declared = !inner.is_empty();
        DocLinkTable { inner, declared }
    }

    /// `[with_state]` resolves to a Symbol link when the table contains it.
    #[test]
    fn expand_resolves_simple_shortcut() {
        let table = make_table(&[("with_state", intro(2))]);
        let runs = expand_shortcut_links("[with_state]", &table, false, false);

        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(&**text, "with_state");
                assert_eq!(key.intro, intro(2));
            }
            other => panic!("expected Link, got {other:?}"),
        }
    }

    /// `[tower::Service]` (cross-crate, not in table) emits plain text without
    /// brackets.
    #[test]
    fn expand_cross_crate_strips_brackets() {
        let table = make_table(&[]); // empty table — nothing in corpus
        let runs = expand_shortcut_links("[tower::Service]", &table, false, false);

        assert_eq!(runs.len(), 1, "must produce exactly one run");
        match &runs[0] {
            InlineRun::Text { text } => {
                assert_eq!(
                    &**text,
                    "tower::Service",
                    "brackets must be stripped; inner text must be preserved"
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// Plain text with no brackets is returned unchanged as a single Text run.
    #[test]
    fn expand_plain_text_unchanged() {
        let table = make_table(&[]);
        let runs = expand_shortcut_links("hello world", &table, false, false);
        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Text { text } => assert_eq!(&**text, "hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// `[!NOTE]` must not be treated as a shortcut link (its inner text is not
    /// a valid symbol path due to the `!` character).
    #[test]
    fn expand_callout_lead_not_treated_as_link() {
        let table = make_table(&[]);
        let runs = expand_shortcut_links("[!NOTE]", &table, false, false);
        // Must come back as a single Text run with the original brackets.
        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Text { text } => {
                assert_eq!(&**text, "[!NOTE]", "callout lead must be preserved verbatim");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// Backtick-quoted shortcut `` [`with_state`] `` resolves correctly; the
    /// display text preserves the backtick as written.
    #[test]
    fn expand_backtick_quoted_link_resolves() {
        let table = make_table(&[("with_state", intro(2))]);
        let runs = expand_shortcut_links("[`with_state`]", &table, false, false);

        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(&**text, "`with_state`");
                assert_eq!(key.intro, intro(2));
            }
            other => panic!("expected Link, got {other:?}"),
        }
    }

    /// Unresolvable backtick-quoted link emits `InlineRun::Code` (not `Text`),
    /// preserving the monospace rendering intent.
    #[test]
    fn expand_backtick_cross_crate_emits_code() {
        let table = make_table(&[]);
        let runs = expand_shortcut_links("[`tower::Service`]", &table, false, false);

        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Code { text } => {
                assert_eq!(&**text, "tower::Service");
            }
            other => panic!("expected Code, got {other:?}"),
        }
    }

    /// Mixed text: `"See [with_state] for more."` splits into three runs.
    #[test]
    fn expand_mixed_text_splits_correctly() {
        let table = make_table(&[("with_state", intro(2))]);
        let runs = expand_shortcut_links("See [with_state] for more.", &table, false, false);

        // Expect: Text("See "), Link("with_state"), Text(" for more.")
        assert_eq!(runs.len(), 3, "must produce 3 runs");
        match &runs[0] {
            InlineRun::Text { text } => assert_eq!(&**text, "See "),
            other => panic!("expected Text, got {other:?}"),
        }
        match &runs[1] {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(&**text, "with_state");
                assert_eq!(key.intro, intro(2));
            }
            other => panic!("expected Link, got {other:?}"),
        }
        match &runs[2] {
            InlineRun::Text { text } => assert_eq!(&**text, " for more."),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// Strong wrapping is preserved on plain-text fragments produced by
    /// `expand_shortcut_links` when `in_strong = true`.
    #[test]
    fn expand_strong_wrapping_preserved() {
        let table = make_table(&[]);
        // Cross-crate link inside a `**...**` span — the emitted Text run for
        // the stripped inner text must be `Strong`, not plain `Text`.
        let runs = expand_shortcut_links("[Unknown::Thing]", &table, true, false);
        assert_eq!(runs.len(), 1);
        match &runs[0] {
            InlineRun::Strong { text } => {
                assert_eq!(&**text, "Unknown::Thing");
            }
            other => panic!("expected Strong, got {other:?}"),
        }
    }

    // ── is_symbol_path tests ──────────────────────────────────────────────────

    #[test]
    fn is_symbol_path_accepts_valid_paths() {
        assert!(is_symbol_path("Foo"));
        assert!(is_symbol_path("Router::with_state"));
        assert!(is_symbol_path("std::collections::HashMap"));
        assert!(is_symbol_path("_private"));
        assert!(is_symbol_path("root.child"));
    }

    #[test]
    fn is_symbol_path_rejects_invalid() {
        assert!(!is_symbol_path(""));
        assert!(!is_symbol_path("https://doc.rust-lang.org"));
        assert!(!is_symbol_path("has space"));
        assert!(!is_symbol_path("!NOTE"));
        assert!(!is_symbol_path("1starts_with_digit"));
    }

    // ── walk_doc integration tests ────────────────────────────────────────────

    /// End-to-end test: a symbol whose documentation contains `[child_fn]`
    /// (a rustdoc shortcut link) and whose `doc_links` field carries the
    /// resolved target must produce an `InlineRun::Link` in the output prose,
    /// not a bare text run with brackets.
    ///
    /// This is the canonical regression test for the original bug:
    /// `[Router::with_state]` rendering as literal text with brackets.
    #[test]
    fn walk_doc_shortcut_link_becomes_symbol_link() {
        // Build a two-entry package: root module + child function.
        let root_id = intro(1);
        let child_id = intro(2);
        let mut table = PristineIntroTable::new();

        // The child function is the link target.
        let child_sym = Symbol {
            name: "child_fn".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            child_id,
            Entry::new(child_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        // The root symbol has documentation with a `[child_fn]` shortcut link
        // and a `doc_links` entry that resolves it.
        let root_sym = sym_with_doc_links(
            "root",
            "See [child_fn] for details.",
            vec![DocLink {
                // The target matches how the producer stores it (may be bare
                // leaf or a qualified path; we test the bare leaf case).
                target: "child_fn".to_owned(),
                label: Some("child_fn".to_owned()),
            }],
        );
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        // We expect exactly one prose section (the doc comment text).
        // The members section (with child_fn) is also present.
        let prose_section = output
            .sections
            .iter()
            .find_map(|s| match s {
                RenderSection::Prose { blocks, .. } => Some(blocks),
                _ => None,
            })
            .expect("must have a Prose section");

        // The paragraph block must contain the link.
        let paragraph_runs = match prose_section.first().expect("must have blocks") {
            ProseBlock::Paragraph { runs } => runs,
            other => panic!("expected Paragraph, got {other:?}"),
        };

        // Locate the Link run in the paragraph.
        let link_run = paragraph_runs
            .iter()
            .find(|r| matches!(r, InlineRun::Link { .. }))
            .expect("paragraph must contain a Link run for the resolved shortcut");

        match link_run {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(
                    &**text,
                    "child_fn",
                    "link text must be the inner shortcut text"
                );
                assert_eq!(
                    key.intro, child_id,
                    "link must point at the child function's IntroId"
                );
            }
            other => panic!("expected Link with Symbol target, got {other:?}"),
        }
    }

    /// End-to-end test: the backtick-quoted form `` [`child_fn`] `` must also
    /// produce an `InlineRun::Link`, not a bare `Code` run.
    ///
    /// pulldown-cmark emits the backtick form as:
    ///   Text("[")  /  Code("child_fn")  /  Text("]...")
    ///
    /// The event-stream lookahead in `build_prose_blocks` handles the `Code`
    /// inner event specifically — this test verifies that branch.
    #[test]
    fn walk_doc_backtick_shortcut_link_becomes_symbol_link() {
        let root_id = intro(1);
        let child_id = intro(2);
        let mut table = PristineIntroTable::new();

        let child_sym = Symbol {
            name: "child_fn".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            child_id,
            Entry::new(child_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        // Documentation uses the backtick form: [`child_fn`]
        let root_sym = sym_with_doc_links(
            "root",
            "See [`child_fn`] for details.",
            vec![DocLink {
                target: "child_fn".to_owned(),
                label: Some("child_fn".to_owned()),
            }],
        );
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        let prose_section = output
            .sections
            .iter()
            .find_map(|s| match s {
                RenderSection::Prose { blocks, .. } => Some(blocks),
                _ => None,
            })
            .expect("must have a Prose section");

        let paragraph_runs = match prose_section.first().expect("must have blocks") {
            ProseBlock::Paragraph { runs } => runs,
            other => panic!("expected Paragraph, got {other:?}"),
        };

        let link_run = paragraph_runs
            .iter()
            .find(|r| matches!(r, InlineRun::Link { .. }))
            .expect("paragraph must contain a Link run for the backtick shortcut");

        match link_run {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(
                    &**text,
                    "child_fn",
                    "link text must be the inner shortcut text (without backticks)"
                );
                assert_eq!(
                    key.intro, child_id,
                    "link must point at the child function's IntroId"
                );
            }
            other => panic!("expected Link with Symbol target, got {other:?}"),
        }
    }

    /// When `doc_links` is empty (no resolved links), bracketed text that looks
    /// like shortcut references must not be corrupted — the fast path must leave
    /// text completely unchanged.
    #[test]
    fn walk_doc_empty_doc_links_preserves_bracketed_text() {
        let root_id = intro(1);
        let mut table = PristineIntroTable::new();

        let root_sym = Symbol {
            name: "root".to_owned(),
            visibility: Visibility::Public,
            documentation: "See [SomeType] for info.".to_owned(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            // No doc_links — the shortcut is not resolved.
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        let prose_section = output
            .sections
            .iter()
            .find_map(|s| match s {
                RenderSection::Prose { blocks, .. } => Some(blocks),
                _ => None,
            })
            .expect("must have a Prose section");

        let paragraph_runs = match prose_section.first().expect("must have blocks") {
            ProseBlock::Paragraph { runs } => runs,
            other => panic!("expected Paragraph, got {other:?}"),
        };

        // With no doc_links the fast path is taken; the text `[SomeType]`
        // passes through unchanged as a single Text run.
        let full_text: String = paragraph_runs
            .iter()
            .filter_map(|r| match r {
                InlineRun::Text { text } => Some(&**text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        assert!(
            full_text.contains("[SomeType]"),
            "without doc_links, bracketed text must pass through unchanged; got: {full_text:?}"
        );
    }

    // ── Defect-1 regression: multiple code blocks must each be their own section ─

    /// Three fenced code blocks separated by prose must produce THREE distinct
    /// `RenderSection::CodeBlock`s, each with its own `SectionId`, and each
    /// `SectionId` must appear in the `SectionPlan`.
    ///
    /// # Root cause
    ///
    /// Before the fix, the entire event stream for a logical section — including
    /// all fenced blocks — was handed to `build_prose_blocks` as one flat slice.
    /// `build_prose_blocks` emitted `ProseBlock::Code` for each fence, all
    /// embedded inside a single `RenderSection::Prose`.  The GUI renders sections
    /// independently and its code-fence path was gated on
    /// `RenderSection::CodeBlock`, not `ProseBlock::Code`; so only the first fence
    /// (or none) received syntax highlighting, and subsequent fences were lost from
    /// the user's view.
    ///
    /// The fix (`split_at_code_blocks` in `parse_markdown`) promotes each fenced
    /// block to its own `RenderSection::CodeBlock` with a freshly-assigned
    /// `SectionId`.  Surrounding prose becomes separate `RenderSection::Prose`
    /// sections.
    ///
    /// # Why this test must go through `walk_doc`
    ///
    /// A test that calls `build_prose_blocks` directly would prove the wrong
    /// thing: `build_prose_blocks` still handles `ProseBlock::Code` for indented
    /// code blocks.  Only `walk_doc` / `parse_markdown` performs the fenced-block
    /// promotion, so this test exercises the real code path.
    #[test]
    fn walk_doc_three_code_blocks_each_become_own_section() {
        let root_id = intro(1);
        let mut table = PristineIntroTable::new();

        // Documentation with three fenced code blocks separated by prose.
        // pulldown-cmark will emit three Start/Text/End(CodeBlock) triplets,
        // each surrounded by paragraph events.
        let doc = concat!(
            "Before first block.\n\n",
            "```rust\n",
            "fn one() {}\n",
            "```\n\n",
            "Between first and second.\n\n",
            "```python\n",
            "def two(): pass\n",
            "```\n\n",
            "Between second and third.\n\n",
            "```javascript\n",
            "function three() {}\n",
            "```\n\n",
            "After all blocks.",
        );

        let sym = Symbol {
            name: "root".to_owned(),
            visibility: Visibility::Public,
            documentation: doc.to_owned(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            root_id,
            Entry::new(sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        // Collect the CodeBlock sections.
        let code_sections: Vec<&RenderSection> = output
            .sections
            .iter()
            .filter(|s| matches!(s, RenderSection::CodeBlock { .. }))
            .collect();

        assert_eq!(
            code_sections.len(),
            3,
            "three fenced blocks must produce three RenderSection::CodeBlock sections; \
             got {} code sections and {} total sections: {:#?}",
            code_sections.len(),
            output.sections.len(),
            output.sections.iter().map(|s| format!("{:?}", s.section_id())).collect::<Vec<_>>(),
        );

        // All three must have distinct SectionIds.
        let ids: Vec<SectionId> = code_sections.iter().map(|s| s.section_id()).collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            "all three CodeBlock sections must have distinct SectionIds; got: {ids:?}",
        );

        // Every CodeBlock section must appear in the plan.
        for id in &ids {
            assert!(
                output.plan.iter().any(|p| p.id == *id),
                "SectionId {id:?} is in sections but not in plan — plan: {:#?}",
                output.plan,
            );
            // And the plan entry must have kind CodeBlock.
            let plan_entry = output.plan.iter().find(|p| p.id == *id).unwrap();
            assert_eq!(
                plan_entry.kind,
                crate::wire::SectionKind::CodeBlock,
                "plan entry for CodeBlock section {id:?} must have kind CodeBlock; \
                 got {:?}",
                plan_entry.kind,
            );
        }

        // The lang strings must be in order: rust, python, javascript.
        let langs: Vec<&str> = code_sections.iter().map(|s| match s {
            // `lang.0` is `&SharedStr` (pattern binds a reference to the field);
            // `&*lang.0` dereferences the `&SharedStr` → `SharedStr` → `str`.
            RenderSection::CodeBlock { lang, .. } => &*lang.0,
            _ => unreachable!(),
        }).collect();
        assert_eq!(langs, vec!["rust", "python", "javascript"],
            "code block languages must appear in source order");
    }

    // ── Defect-2 regression: realistic producer target formats must resolve ────

    /// A namespace-tagged target (`"Router::with_state!m"`) must resolve to the
    /// correct symbol.  The `!m` suffix is a producer namespace discriminator;
    /// stripping it recovers the plain path that the name index knows.
    ///
    /// This is the canonical regression test for the defect described in the
    /// task brief: the original `DocLinkTable::build` used `rsplit("::")` to
    /// extract the leaf, which returned the whole string `"with_state!m"` for a
    /// namespace-tagged target (no `::` before the tag, so the split found
    /// nothing to strip).  `by_name.get_exact("with_state!m")` found nothing,
    /// so the link was always left unresolved.
    #[test]
    fn doc_link_table_namespace_tagged_target_resolves() {
        let (pkg, child_id) = build_pkg_with_child("with_state");

        let doc_links = vec![DocLink {
            // The producer emits a namespace tag to disambiguate methods from
            // fields and associated constants that share the same leaf name.
            target: "Router::with_state!m".to_owned(),
            label: Some("Router::with_state".to_owned()),
        }];

        let sym = sym_with_doc_links("Router", "", doc_links);
        let entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&entry, &pkg);

        let expected_key = StableRef::new(lineage(), child_id);

        // The exact original target string (with tag) must resolve.
        assert_eq!(
            table.resolve("Router::with_state!m"),
            Some(&expected_key),
            "namespace-tagged target must resolve after stripping the tag"
        );

        // The clean form (without tag) must also resolve.
        assert_eq!(
            table.resolve("Router::with_state"),
            Some(&expected_key),
            "clean form of the target must also resolve"
        );

        // The bare leaf must resolve.
        assert_eq!(
            table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must resolve"
        );
    }

    /// A dot-path producer target (`"axum.routing.Router.with_state"`) must
    /// resolve.  Moniker paths use `.` as the separator; the original code only
    /// split on `"::"` so a dot-path target returned the whole string as the
    /// "leaf", which was never in the name index.
    #[test]
    fn doc_link_table_dot_path_target_resolves() {
        let (pkg, child_id) = build_pkg_with_child("with_state");

        let doc_links = vec![DocLink {
            target: "axum.routing.Router.with_state".to_owned(),
            label: None,
        }];

        let sym = sym_with_doc_links("Router", "", doc_links);
        let entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&entry, &pkg);
        let expected_key = StableRef::new(lineage(), child_id);

        assert_eq!(
            table.resolve("axum.routing.Router.with_state"),
            Some(&expected_key),
            "dot-path target must resolve via leaf `.` split"
        );
        assert_eq!(
            table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must also resolve"
        );
    }

    /// A dot-path namespace-tagged target (`"axum.routing.Router.with_state!m"`)
    /// must resolve.  This combines the dot-path and namespace-tag problems.
    #[test]
    fn doc_link_table_dot_path_namespace_tagged_resolves() {
        let (pkg, child_id) = build_pkg_with_child("with_state");

        let doc_links = vec![DocLink {
            target: "axum.routing.Router.with_state!m".to_owned(),
            label: None,
        }];

        let sym = sym_with_doc_links("Router", "", doc_links);
        let entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&entry, &pkg);
        let expected_key = StableRef::new(lineage(), child_id);

        assert_eq!(
            table.resolve("axum.routing.Router.with_state!m"),
            Some(&expected_key),
            "dot-path + namespace tag must resolve after both stripping steps"
        );
        assert_eq!(
            table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must also resolve"
        );
    }

    /// A rustdoc HTML-anchor target (`"#method.with_state"`) must resolve.
    /// rustdoc sometimes stores the HTML fragment identifier as the target.
    /// The `#method.` prefix carries no semantic information the engine needs.
    #[test]
    fn doc_link_table_anchor_target_resolves() {
        let (pkg, child_id) = build_pkg_with_child("with_state");

        let doc_links = vec![DocLink {
            target: "#method.with_state".to_owned(),
            label: Some("with_state".to_owned()),
        }];

        let sym = sym_with_doc_links("Router", "", doc_links);
        let entry = Entry::new(
            sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let table = DocLinkTable::build(&entry, &pkg);
        let expected_key = StableRef::new(lineage(), child_id);

        assert_eq!(
            table.resolve("#method.with_state"),
            Some(&expected_key),
            "rustdoc anchor target must resolve after stripping the #discriminator. prefix"
        );
        assert_eq!(
            table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must also resolve"
        );
    }

    /// End-to-end test through `walk_doc`: a doc comment `[Router::with_state]`
    /// with a namespace-tagged `doc_link` target must produce a Symbol link in
    /// the rendered prose.
    ///
    /// This is the full pipeline test for defect 2: the original bug survived
    /// because the unit tests used hand-built `DocLinkTable` entries with simple
    /// leaf-only targets, while real producer data uses qualified/tagged paths.
    #[test]
    fn walk_doc_namespace_tagged_target_resolves_to_link_in_prose() {
        let root_id = intro(1);
        let fn_id = intro(2);
        let mut table = PristineIntroTable::new();

        let fn_sym = Symbol {
            name: "with_state".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            fn_id,
            Entry::new(fn_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        // The root's doc references [Router::with_state] but the producer
        // stored the target with a namespace tag: "Router::with_state!m".
        let root_sym = sym_with_doc_links(
            "root",
            "Call [Router::with_state] to build the app.",
            vec![DocLink {
                target: "Router::with_state!m".to_owned(),
                label: Some("Router::with_state".to_owned()),
            }],
        );
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        // Locate the prose section.
        let prose_section = output
            .sections
            .iter()
            .find_map(|s| match s {
                RenderSection::Prose { blocks, .. } => Some(blocks),
                _ => None,
            })
            .expect("must have a Prose section");

        let paragraph_runs = match prose_section.first().expect("must have blocks") {
            ProseBlock::Paragraph { runs } => runs,
            other => panic!("expected Paragraph, got {other:?}"),
        };

        let link_run = paragraph_runs
            .iter()
            .find(|r| matches!(r, InlineRun::Link { .. }))
            .expect(
                "paragraph must contain a Link run; \
                 a namespace-tagged producer target must still resolve to a Symbol link"
            );

        match link_run {
            InlineRun::Link { text, target: LinkTarget::Symbol { key } } => {
                assert_eq!(&**text, "Router::with_state");
                assert_eq!(key.intro, fn_id);
            }
            other => panic!("expected Link(Symbol), got {other:?}"),
        }
    }

    /// End-to-end test: `[Self::bar]` (Rust path starting with `Self`) and
    /// `[crate::routing::Router]` (crate-relative path) must each resolve to a
    /// Symbol link when the underlying symbol is in the same package.
    ///
    /// These two forms exercise the `resolve` fallback in `DocLinkTable::resolve`:
    /// the doc text contains the author's path; the table holds the bare leaf.
    #[test]
    fn walk_doc_self_and_crate_prefix_paths_resolve() {
        let root_id = intro(1);
        let bar_id = intro(2);
        let router_id = intro(3);
        let mut table = PristineIntroTable::new();

        let bar_sym = Symbol {
            name: "bar".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            bar_id,
            Entry::new(bar_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        let router_sym = Symbol {
            name: "Router".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        table.insert_live(
            router_id,
            Entry::new(router_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );

        // The documentation uses both `Self::bar` and `crate::routing::Router`.
        let root_sym = sym_with_doc_links(
            "root",
            "Use [Self::bar] or [crate::routing::Router] here.",
            vec![
                DocLink {
                    target: "Self::bar".to_owned(),
                    label: Some("Self::bar".to_owned()),
                },
                DocLink {
                    target: "crate::routing::Router".to_owned(),
                    label: Some("crate::routing::Router".to_owned()),
                },
            ],
        );
        table.insert_live(
            root_id,
            Entry::new(root_sym, Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        let root_entry = pkg.view().entry(root_id).expect("root must exist");
        let output = walk_doc(root_id, root_entry, pkg.view(), &pkg);

        let prose_section = output
            .sections
            .iter()
            .find_map(|s| match s {
                RenderSection::Prose { blocks, .. } => Some(blocks),
                _ => None,
            })
            .expect("must have a Prose section");

        let paragraph_runs = match prose_section.first().expect("must have blocks") {
            ProseBlock::Paragraph { runs } => runs,
            other => panic!("expected Paragraph, got {other:?}"),
        };

        // Both links must appear.
        let link_runs: Vec<_> = paragraph_runs
            .iter()
            .filter(|r| matches!(r, InlineRun::Link { target: LinkTarget::Symbol { .. }, .. }))
            .collect();

        assert_eq!(
            link_runs.len(),
            2,
            "both [Self::bar] and [crate::routing::Router] must produce Symbol links; \
             paragraph runs: {paragraph_runs:#?}",
        );

        let intros: std::collections::HashSet<_> = link_runs.iter().map(|r| match r {
            InlineRun::Link { target: LinkTarget::Symbol { key }, .. } => key.intro,
            _ => unreachable!(),
        }).collect();

        assert!(intros.contains(&bar_id), "bar_id must be among the linked symbols");
        assert!(intros.contains(&router_id), "router_id must be among the linked symbols");
    }

    // ── Defect-3 regression: producer path drops impl-type segment ────────────

    /// When two symbols share the same leaf name (e.g. `with_state` on both
    /// `Router` and `Handler`) the exact suffix check fails because the
    /// producer emits `"axum::routing::with_state"` (module-qualified, no impl
    /// type) while `moniker_path` stores `"axum.routing.Router.with_state"`.
    ///
    /// The segment-level fallback uses the **label** (`"Router::with_state"`)
    /// to disambiguate: label segments `["router", "with_state"]` are checked
    /// against the dot-path segments of each hit's stored path in order.  The
    /// entry whose stored path contains all label segments in order wins.
    ///
    /// This test sets up a three-entry package:
    ///   - root module
    ///   - `Router.with_state` (child of a `Router` impl-like entry)
    ///   - `Handler.with_state` (child of a `Handler` impl-like entry)
    ///
    /// Both leaf names are `"with_state"`.  The producer target is
    /// `"axum::routing::with_state"` with label `"Router::with_state"`.  The
    /// suffix check fails for both (neither moniker path ends with
    /// `"axum.routing.with_state"`).  The segment-level fallback must pick the
    /// entry under `Router`, not `Handler`.
    #[test]
    fn doc_link_table_segment_level_fallback_picks_correct_symbol_when_suffix_fails() {
        let root_id = intro(1);
        let router_id = intro(2);       // "Router" (the struct/impl type)
        let router_ws_id = intro(3);    // "with_state" child of Router
        let handler_id = intro(4);      // "Handler"
        let handler_ws_id = intro(5);   // "with_state" child of Handler

        let mut table = PristineIntroTable::new();

        // Insert the entries in a hierarchy that produces moniker paths:
        //   root          → "root"
        //   root.Router   → "root.Router"
        //   root.Router.with_state → "root.Router.with_state"
        //   root.Handler  → "root.Handler"
        //   root.Handler.with_state → "root.Handler.with_state"
        //
        // The producer canonical path would be "root::with_state" (module =
        // root, method name = with_state) for BOTH — it drops the type segment.
        // The label distinguishes them: "Router::with_state" vs "Handler::with_state".

        let make_sym = |name: &str| Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        table.insert_live(
            root_id,
            Entry::new(make_sym("root"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            None,
        );
        table.insert_live(
            router_id,
            Entry::new(make_sym("Router"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );
        table.insert_live(
            router_ws_id,
            Entry::new(make_sym("with_state"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(router_id),
        );
        table.insert_live(
            handler_id,
            Entry::new(make_sym("Handler"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(root_id),
        );
        table.insert_live(
            handler_ws_id,
            Entry::new(make_sym("with_state"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)),
            Some(handler_id),
        );

        let view = IrView::with_package(lineage(), table);
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        // The producer target is "root::with_state" (module-qualified, no type
        // segment).  This normalises to "root.with_state".  Neither
        // "root.Router.with_state" nor "root.Handler.with_state" ends with
        // "root.with_state", so the exact suffix check must fail for both.
        //
        // The label "Router::with_state" segments = ["router", "with_state"].
        // "root.Router.with_state" contains both in order → match.
        // "root.Handler.with_state" does not contain "router" → no match.
        let doc_links = vec![DocLink {
            target: "root::with_state".to_owned(),
            label: Some("Router::with_state".to_owned()),
        }];

        let root_sym = sym_with_doc_links(
            "root",
            "Call [Router::with_state] here.",
            doc_links,
        );
        let root_entry = Entry::new(
            root_sym,
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        );

        let doc_link_table = DocLinkTable::build(&root_entry, &pkg);

        let expected_key = StableRef::new(lineage(), router_ws_id);

        // The label alias "Router::with_state" must resolve to router_ws_id.
        assert_eq!(
            doc_link_table.resolve("Router::with_state"),
            Some(&expected_key),
            "segment-level fallback must pick router_ws_id ('root.Router.with_state') \
             over handler_ws_id ('root.Handler.with_state') when the label is \
             'Router::with_state'; neither stored path ends with 'root.with_state' \
             so the exact suffix check must fail and the fallback must fire"
        );

        // The leaf "with_state" must also resolve to the same entry (stored
        // as alias (c) during the successful segment-level resolution).
        assert_eq!(
            doc_link_table.resolve("with_state"),
            Some(&expected_key),
            "bare leaf must also resolve to router_ws_id after successful \
             segment-level fallback"
        );
    }
}
