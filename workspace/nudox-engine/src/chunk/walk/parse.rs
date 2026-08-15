//! Markdown parsing helpers for the documentation walk.
//!
//! Splits markdown at H2 headings, promotes fenced code blocks to their own
//! sections, and detects callout blockquotes.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::wire::{
    CalloutLevel, LangId, ProseBlock, RenderSection, SectionId, SectionKind, SectionPlan,
    SharedStr, SizeHint,
};

use super::doc_link_table::DocLinkTable;

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
pub(crate) fn parse_markdown(
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
                    current_events.push(event);
                }
            }
            Event::End(TagEnd::Heading(level)) => {
                if in_heading == Some(*level) && *level == HeadingLevel::H2 {
                    if !current_events.is_empty() || current_heading.is_some() {
                        logical_sections
                            .push((current_heading.take(), std::mem::take(&mut current_events)));
                    }
                    current_heading = Some(heading_buf.trim().to_owned());
                    heading_buf.clear();
                    in_heading = None;
                } else {
                    in_heading = None;
                    heading_buf.clear();
                    current_events.push(event);
                }
            }
            Event::Text(text) | Event::Code(text) if in_heading.is_some() => {
                heading_buf.push_str(text);
                if in_heading != Some(HeadingLevel::H2) {
                    current_events.push(event);
                }
            }
            Event::Text(_) | Event::Code(_) => {
                current_events.push(event);
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

    for (heading, events) in logical_sections {
        let heading_text = heading.as_deref().unwrap_or("");

        let slices: Vec<(Vec<Event<'_>>, bool)> = split_at_code_blocks(events);

        let mut heading_used = false;

        for (slice_events, is_code_block) in slices {
            if is_code_block {
                let (lang_str, text_str) = extract_code_block_content(&slice_events);

                let line_count = text_str.chars().filter(|&c| c == '\n').count() as u32;
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
                let effective_heading = if heading_used {
                    ""
                } else {
                    heading_used = true;
                    heading_text
                };

                if slice_events.is_empty() && effective_heading.is_empty() {
                    continue;
                }

                let callout_level = detect_callout_level(&slice_events);

                let kind = if callout_level.is_some() {
                    SectionKind::Callout
                } else if effective_heading == "Example" || effective_heading == "Examples" {
                    SectionKind::Examples
                } else {
                    SectionKind::Prose
                };

                *id_counter += 1;
                let id = SectionId(*id_counter);

                let blocks = super::prose::build_prose_blocks(
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
fn split_at_code_blocks(events: Vec<Event<'_>>) -> Vec<(Vec<Event<'_>>, bool)> {
    let mut slices: Vec<(Vec<Event<'_>>, bool)> = Vec::new();
    let mut prose_buf: Vec<Event<'_>> = Vec::new();

    let mut iter = events.into_iter();

    while let Some(ev) = iter.next() {
        let is_fenced_start = matches!(&ev, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))));

        if is_fenced_start {
            if !prose_buf.is_empty() {
                slices.push((std::mem::take(&mut prose_buf), false));
            }

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

    if !prose_buf.is_empty() {
        slices.push((prose_buf, false));
    }

    slices
}

/// Extract `(lang_str, text_str)` from a fence event slice.
fn extract_code_block_content(events: &[Event<'_>]) -> (String, String) {
    let mut lang_str = String::new();
    let mut text_str = String::new();

    for ev in events {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                lang_str = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
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
pub(crate) fn prose_block_lines(blocks: &[ProseBlock]) -> u32 {
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
