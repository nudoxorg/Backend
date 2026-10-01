//! Bounded, lossless source-page cursors and reader-owned navigation memory.

use crate::model::pages::{ByteSpan, SourceCoverage, SourceText};

/// Bounded source pages keep wrapping, syntax work and keyboard targets small
/// while every byte remains reachable, including a long minified line.
pub(super) const MAX_SOURCE_LINES: usize = 64;
pub(super) const MAX_SOURCE_BYTES: usize = 8 * 1024;
pub(super) const MAX_SOURCE_LINE_BYTES: usize = 2 * 1024;
const MAX_PAGE_HISTORY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceCursor {
    pub(super) line: u32,
    pub(super) byte: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VisibleLine {
    pub(super) number: u32,
    pub(super) span: ByteSpan,
    pub(super) continued: bool,
    pub(super) more_in_line: bool,
}

pub(super) struct SourcePage {
    pub(super) lines: Vec<VisibleLine>,
    pub(super) next: Option<SourceCursor>,
}

impl SourcePage {
    pub(super) fn at(source: &SourceText, cursor: SourceCursor) -> Self {
        let Some(first_index) = cursor
            .line
            .checked_sub(source.first_line)
            .and_then(|index| usize::try_from(index).ok())
        else {
            return Self {
                lines: Vec::new(),
                next: None,
            };
        };
        let spans = source.line_spans_in(first_index, MAX_SOURCE_LINES);
        let mut lines = Vec::with_capacity(spans.len());
        let mut used = 0_usize;
        let mut next = None;
        for (index, full_span) in spans.into_iter().enumerate() {
            let number = cursor
                .line
                .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
            let Some(full) = source.text().get(full_span.range()) else {
                break;
            };
            let offset = if index == 0 {
                cursor.byte.min(full.len())
            } else {
                0
            };
            let Some(rest) = full.get(offset..) else {
                break;
            };
            let allowed = MAX_SOURCE_BYTES
                .saturating_sub(used)
                .min(MAX_SOURCE_LINE_BYTES);
            let visible = utf8_prefix(rest, allowed);
            if visible.is_empty() && !rest.is_empty() {
                next = Some(SourceCursor {
                    line: number,
                    byte: offset,
                });
                break;
            }
            let start = full_span.start as usize + offset;
            let end = start.saturating_add(visible.len());
            let (Ok(start_byte), Ok(end_byte)) = (u32::try_from(start), u32::try_from(end)) else {
                break;
            };
            let more_in_line = visible.len() < rest.len();
            lines.push(VisibleLine {
                number,
                span: ByteSpan {
                    start: start_byte,
                    end: end_byte,
                },
                continued: offset > 0,
                more_in_line,
            });
            used = used.saturating_add(visible.len()).saturating_add(1);
            if more_in_line {
                next = Some(SourceCursor {
                    line: number,
                    byte: offset + visible.len(),
                });
                break;
            }
            if used >= MAX_SOURCE_BYTES || lines.len() >= MAX_SOURCE_LINES {
                let next_line = number.saturating_add(1);
                if usize::try_from(next_line.saturating_sub(source.first_line))
                    .is_ok_and(|index| index < source.line_count())
                {
                    next = Some(SourceCursor {
                        line: next_line,
                        byte: 0,
                    });
                }
                break;
            }
        }
        if next.is_none() {
            let following = lines.last().map(|line| line.number.saturating_add(1));
            if let Some(line) = following.filter(|line| {
                usize::try_from(line.saturating_sub(source.first_line))
                    .is_ok_and(|index| index < source.line_count())
            }) {
                next = Some(SourceCursor { line, byte: 0 });
            }
        }
        Self { lines, next }
    }
}

pub(super) fn previous_cursor(source: &SourceText, cursor: SourceCursor) -> Option<SourceCursor> {
    if cursor.byte > 0 {
        let span = source.line_span(cursor.line)?;
        let line = source.text().get(span.range())?;
        let mut byte = cursor
            .byte
            .min(line.len())
            .saturating_sub(MAX_SOURCE_LINE_BYTES);
        while !line.is_char_boundary(byte) {
            byte += 1;
        }
        return Some(SourceCursor {
            line: cursor.line,
            byte,
        });
    }
    let before = cursor.line.checked_sub(source.first_line)? as usize;
    if before == 0 {
        return None;
    }
    let first_index = before.saturating_sub(MAX_SOURCE_LINES);
    let spans = source.line_spans_in(first_index, before - first_index);
    let mut used = 0_usize;
    let mut start = None;
    for (index, span) in spans.iter().enumerate().rev() {
        let line = source.text().get(span.range())?;
        if line.len() > MAX_SOURCE_LINE_BYTES {
            if start.is_none() {
                let mut byte = line.len().saturating_sub(MAX_SOURCE_LINE_BYTES);
                while !line.is_char_boundary(byte) {
                    byte += 1;
                }
                let number = source
                    .first_line
                    .saturating_add(u32::try_from(first_index + index).ok()?);
                return Some(SourceCursor { line: number, byte });
            }
            break;
        }
        if used.saturating_add(line.len()).saturating_add(1) > MAX_SOURCE_BYTES {
            break;
        }
        used = used.saturating_add(line.len()).saturating_add(1);
        let number = source
            .first_line
            .saturating_add(u32::try_from(first_index + index).ok()?);
        start = Some(SourceCursor {
            line: number,
            byte: 0,
        });
    }
    start
}

pub(super) fn initial_cursor(source: &SourceText, line: u32, context: u32) -> SourceCursor {
    let target = line.max(source.first_line);
    let Some(target_index) = target
        .checked_sub(source.first_line)
        .map(|index| index as usize)
    else {
        return SourceCursor {
            line: source.first_line,
            byte: 0,
        };
    };
    let first_index = target_index.saturating_sub(context as usize);
    let spans = source.line_spans_in(first_index, target_index - first_index);
    let mut start = target;
    let mut bytes = 0_usize;
    for (index, span) in spans.iter().enumerate().rev() {
        let Some(line) = source.text().get(span.range()) else {
            break;
        };
        if line.len() > MAX_SOURCE_LINE_BYTES || bytes + line.len() + 1 > MAX_SOURCE_BYTES / 4 {
            break;
        }
        bytes += line.len() + 1;
        start = source
            .first_line
            .saturating_add(u32::try_from(first_index + index).unwrap_or(u32::MAX));
    }
    SourceCursor {
        line: start,
        byte: 0,
    }
}

/// Exact source-page position retained by Reader across navigation and Back.
/// The route and owner revision are keyed by Reader, never inferred here.
#[derive(Debug)]
pub(crate) struct PagingState {
    pub(super) cursor: SourceCursor,
    pub(super) reference_page: usize,
    pub(super) restored_focus_place: Option<u64>,
    back: Vec<SourceCursor>,
    forward: Vec<SourceCursor>,
}

impl PagingState {
    pub(crate) fn new(cursor: SourceCursor) -> Self {
        Self {
            cursor,
            reference_page: 0,
            restored_focus_place: None,
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    pub(super) fn previous_target(&self, source: &SourceText) -> Option<SourceCursor> {
        self.back
            .last()
            .copied()
            .or_else(|| previous_cursor(source, self.cursor))
    }

    pub(super) fn next_target(&self, page: &SourcePage) -> Option<SourceCursor> {
        self.forward.last().copied().or(page.next)
    }

    pub(super) fn previous(&mut self, fallback: SourceCursor) -> SourceCursor {
        let previous = self.back.pop().unwrap_or(fallback);
        push_history(&mut self.forward, self.cursor);
        self.cursor = previous;
        self.reference_page = 0;
        previous
    }

    pub(super) fn next(&mut self, fallback: SourceCursor) -> SourceCursor {
        let next = self.forward.pop().unwrap_or(fallback);
        push_history(&mut self.back, self.cursor);
        self.cursor = next;
        self.reference_page = 0;
        next
    }

    pub(super) fn jump(&mut self, line: u32) -> SourceCursor {
        let next = SourceCursor { line, byte: 0 };
        self.cursor = next;
        self.back.clear();
        self.forward.clear();
        self.reference_page = 0;
        next
    }
}

fn push_history(history: &mut Vec<SourceCursor>, cursor: SourceCursor) {
    if history.len() == MAX_PAGE_HISTORY {
        history.remove(0);
    }
    history.push(cursor);
}

pub(super) fn verified_link(
    coverage: SourceCoverage,
    identifier: ByteSpan,
    visible: ByteSpan,
) -> bool {
    let within = identifier.start >= visible.start
        && identifier.end <= visible.end
        && identifier.start < identifier.end;
    within
        && match coverage {
            SourceCoverage::CapturedExcerpt => true,
            SourceCoverage::LiveFileExcerptVerified { bytes } => {
                identifier.start >= bytes.start && identifier.end <= bytes.end
            }
            SourceCoverage::Unverified => false,
        }
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[..end]
}
