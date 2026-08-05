//! ProseBlock builder — converts pulldown-cmark event streams into `ProseBlock`s.
//!
//! Also contains shortcut-link expansion and inline-run helpers.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

use crate::wire::{InlineRun, LangId, LinkTarget, ProseBlock, SharedStr};

use super::doc_link_table::DocLinkTable;

/// Convert a stream of pulldown-cmark events into `ProseBlock`s.
///
/// The heading text is prepended as a `Heading` block when non-empty.
pub(crate) fn build_prose_blocks(
    events: Vec<Event<'_>>,
    section_heading: &str,
    doc_link_table: &DocLinkTable,
) -> Vec<ProseBlock> {
    let mut blocks: Vec<ProseBlock> = Vec::new();

    if !section_heading.is_empty() {
        blocks.push(ProseBlock::Heading {
            level: 2,
            runs: vec![InlineRun::Text {
                text: SharedStr::from(section_heading),
            }],
        });
    }

    let mut state = BlockState::None;
    let mut inline_buf: Vec<InlineRun> = Vec::new();
    let mut list_ordered: bool = false;
    let mut list_items: Vec<Vec<InlineRun>> = Vec::new();
    let mut code_lang = String::new();
    let mut code_text = String::new();
    let mut heading_level: u8 = 1;
    let mut blockquote_inline: Vec<InlineRun> = Vec::new();
    let mut in_blockquote = false;
    let mut in_strong = false;
    let mut in_em = false;
    let mut link_url: Option<String> = None;
    let mut link_text_buf = String::new();

    let n = events.len();
    let mut i = 0;
    while i < n {
        // ── Shortcut-link lookahead (event-stream level) ───────────────────
        if doc_link_table.has_declared_links()
            && state != BlockState::Code
            && link_url.is_none()
            && i + 2 < n
        {
            if let Event::Text(open_text) = &events[i] {
                if open_text.as_ref() == "[" {
                    let inner_event = &events[i + 1];
                    let close_event = &events[i + 2];

                    let inner_opt: Option<(&str, bool)> = match inner_event {
                        Event::Text(t) => Some((t.as_ref(), false)),
                        Event::Code(t) => Some((t.as_ref(), true)),
                        _ => None,
                    };

                    let close_opt: Option<&str> = match close_event {
                        Event::Text(t) if t.as_ref().starts_with(']') => Some(t.as_ref()),
                        _ => None,
                    };

                    if let (Some((inner, is_code)), Some(close_str)) = (inner_opt, close_opt) {
                        if is_symbol_path(inner) {
                            let remainder = &close_str[1..];

                            let run = match doc_link_table.resolve(inner) {
                                Some(key) => InlineRun::Link {
                                    text: SharedStr::from(inner),
                                    target: LinkTarget::Symbol { key: key.clone() },
                                },
                                None => {
                                    if is_code {
                                        InlineRun::Code {
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

                            if !remainder.is_empty() {
                                let trailing = make_text_run(remainder, in_strong, in_em);
                                push_inline(
                                    &mut inline_buf,
                                    &mut blockquote_inline,
                                    in_blockquote,
                                    trailing,
                                );
                            }

                            i += 3;
                            continue;
                        }
                    }
                }
            }
        }

        // ── Single-event processing ──────────────────────────────────────
        let event = &events[i];
        match event {
            Event::Start(Tag::Paragraph) => {
                state = BlockState::Paragraph;
                inline_buf.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                if state == BlockState::Paragraph && !inline_buf.is_empty() {
                    blocks.push(ProseBlock::Paragraph {
                        runs: std::mem::take(&mut inline_buf),
                    });
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
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
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
                    blocks.push(ProseBlock::Paragraph {
                        runs: std::mem::take(&mut blockquote_inline),
                    });
                }
            }

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
                    let target = if is_symbol_path(&url) {
                        if let Some(key) = doc_link_table.resolve(&url) {
                            LinkTarget::Symbol { key: key.clone() }
                        } else {
                            LinkTarget::Url {
                                url: SharedStr::from(url.as_str()),
                            }
                        }
                    } else {
                        LinkTarget::Url {
                            url: SharedStr::from(url.as_str()),
                        }
                    };
                    let run = InlineRun::Link {
                        text: SharedStr::from(link_text_buf.as_str()),
                        target,
                    };
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
                link_text_buf.clear();
            }

            Event::Text(text) => {
                if state == BlockState::Code {
                    code_text.push_str(text);
                } else if link_url.is_some() {
                    link_text_buf.push_str(text);
                } else {
                    let run = make_text_run(text, in_strong, in_em);
                    push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
                }
            }

            Event::Code(text) => {
                let run = InlineRun::Code {
                    text: SharedStr::from(text.as_ref()),
                };
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }

            Event::SoftBreak | Event::HardBreak => {
                let run = InlineRun::Text {
                    text: SharedStr::from(" "),
                };
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
/// This function remains for two purposes:
/// 1. **Unit tests** — the isolated tests verify its logic directly.
/// 2. **Future use** — if a producer ever emits a single `Text` event
///    containing the full `[Foo]` span.
/// Test-only utility (kept for unit tests and future use).
#[allow(dead_code)]
pub(crate) fn expand_shortcut_links(
    text: &str,
    doc_link_table: &DocLinkTable,
    in_strong: bool,
    in_em: bool,
) -> Vec<InlineRun> {
    let mut runs: Vec<InlineRun> = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find('[') {
        if open > 0 {
            let before = &rest[..open];
            runs.push(make_text_run(before, in_strong, in_em));
        }

        let after_open = &rest[open + 1..];
        let Some(close_rel) = after_open.find(']') else {
            runs.push(make_text_run(rest, in_strong, in_em));
            return runs;
        };
        let close = open + 1 + close_rel;
        let after_close = &rest[close + 1..];

        if after_close.starts_with('(') || after_close.starts_with('[') {
            runs.push(make_text_run("[", in_strong, in_em));
            rest = &rest[open + 1..];
            continue;
        }

        let inner = &rest[open + 1..close];
        let inner_stripped = inner
            .trim()
            .trim_start_matches('`')
            .trim_end_matches('`')
            .trim();

        if !is_symbol_path(inner_stripped) {
            let span = &rest[open..=close];
            runs.push(make_text_run(span, in_strong, in_em));
            rest = &rest[close + 1..];
            continue;
        }

        match doc_link_table.resolve(inner_stripped) {
            Some(key) => {
                runs.push(InlineRun::Link {
                    text: SharedStr::from(inner),
                    target: LinkTarget::Symbol { key: key.clone() },
                });
            }
            None => {
                let run = if inner.starts_with('`') && inner.ends_with('`') && inner.len() > 1 {
                    InlineRun::Code {
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

    if !rest.is_empty() {
        runs.push(make_text_run(rest, in_strong, in_em));
    }

    runs
}

/// Produce a plain/strong/em `InlineRun` for a text fragment.
#[inline]
fn make_text_run(text: &str, in_strong: bool, in_em: bool) -> InlineRun {
    if in_strong {
        InlineRun::Strong {
            text: SharedStr::from(text),
        }
    } else if in_em {
        InlineRun::Em {
            text: SharedStr::from(text),
        }
    } else {
        InlineRun::Text {
            text: SharedStr::from(text),
        }
    }
}

/// Return `true` when `s` looks like a Rust symbol path rather than a URL or prose.
pub(crate) fn is_symbol_path(s: &str) -> bool {
    if s.is_empty() || s.contains("://") || s.contains(char::is_whitespace) {
        return false;
    }
    s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn push_inline(buf: &mut Vec<InlineRun>, bq_buf: &mut Vec<InlineRun>, in_bq: bool, run: InlineRun) {
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
