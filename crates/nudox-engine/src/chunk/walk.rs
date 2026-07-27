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
    SectionId, SectionKind, SectionPlan, SharedStr, SizeHint,
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
        parse_markdown(doc, &mut id_counter, &mut sections, &mut plan);
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
fn parse_markdown(
    doc: &str,
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
    for (heading, events) in logical_sections {
        let heading_text = heading.as_deref().unwrap_or("");

        // Detect callout: first blockquote paragraph starts with [!...].
        // We scan events for the pattern.
        let callout_level = detect_callout_level(&events);

        let kind = if callout_level.is_some() {
            SectionKind::Callout
        } else if heading_text == "Example" || heading_text == "Examples" {
            SectionKind::Examples
        } else {
            SectionKind::Prose
        };

        *id_counter += 1;
        let id = SectionId(*id_counter);

        // Build the blocks FIRST, then measure them.
        //
        // This ordering is the zero-jump guarantee (§9.4), and it is load-
        // bearing rather than stylistic. Measuring the raw pulldown-cmark event
        // stream instead — as this code originally did — counts a different
        // thing than the GUI will lay out: one paragraph is two events
        // (`Text` + `End(Paragraph)`) but a single `ProseBlock`, so every hint
        // came out roughly double. The skeleton was reserving five lines where
        // one arrived, and everything below it jumped up four lines mid-read.
        //
        // Deriving the hint from `blocks` makes the plan and the content
        // *the same measurement*, so they cannot drift apart no matter how the
        // markdown mapping changes later.
        let mut blocks = build_prose_blocks(events, heading_text);

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
fn build_prose_blocks(events: Vec<Event<'_>>, section_heading: &str) -> Vec<ProseBlock> {
    let mut blocks: Vec<ProseBlock> = Vec::new();

    if !section_heading.is_empty() {
        blocks.push(ProseBlock::Heading {
            level: 2,
            runs: vec![crate::wire::InlineRun::Text(SharedStr::from(section_heading))],
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

    for event in events {
        match event {
            // ── Block-level starts ─────────────────────────────────────────

            Event::Start(Tag::Paragraph) => {
                state = BlockState::Paragraph;
                inline_buf.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                if state == BlockState::Paragraph && !inline_buf.is_empty() {
                    blocks.push(ProseBlock::Paragraph(std::mem::take(&mut inline_buf)));
                }
                state = BlockState::None;
            }

            Event::Start(Tag::Heading { level, .. }) => {
                state = BlockState::Heading;
                heading_level = heading_level_to_u8(level);
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
                    blocks.push(ProseBlock::Paragraph(std::mem::take(&mut blockquote_inline)));
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

            Event::Start(Tag::Link { dest_url, .. }) => {
                link_url = Some(dest_url.to_string());
                link_text_buf.clear();
            }
            Event::End(TagEnd::Link) => {
                if let Some(url) = link_url.take() {
                    let run = crate::wire::InlineRun::Link {
                        text: SharedStr::from(link_text_buf.as_str()),
                        target: LinkTarget::Url(SharedStr::from(url.as_str())),
                    };
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
                link_text_buf.clear();
            }

            // ── Text and code ──────────────────────────────────────────────

            Event::Text(text) => {
                if state == BlockState::Code {
                    code_text.push_str(&text);
                } else if link_url.is_some() {
                    link_text_buf.push_str(&text);
                } else {
                    let run = if in_strong {
                        crate::wire::InlineRun::Strong(SharedStr::from(text.as_ref()))
                    } else if in_em {
                        crate::wire::InlineRun::Em(SharedStr::from(text.as_ref()))
                    } else {
                        crate::wire::InlineRun::Text(SharedStr::from(text.as_ref()))
                    };
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
            }

            Event::Code(text) => {
                let run = crate::wire::InlineRun::Code(SharedStr::from(text.as_ref()));
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }

            Event::SoftBreak | Event::HardBreak => {
                let run = crate::wire::InlineRun::Text(SharedStr::from(" "));
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }

            _ => {}
        }
    }

    blocks
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
