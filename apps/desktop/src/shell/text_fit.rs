//! Fitting identifiers and code to the width they get — never by clipping.
//!
//! - A **name** (a page's subject) wraps at identifier boundaries — CamelCase
//!   humps, after `_`, after `::` — with no hyphen glyph; only when a single
//!   segment still cannot fit does its size step down, to a floor of 0.6×.
//! - **Code** soft-wraps each source line at token boundaries (after a
//!   space, `,`, an opening bracket, `::`; before `->` / `=>`) with a
//!   hanging indent; a token longer than the line breaks as a last resort.
//!
//! Widths come from the text system's glyph advances for the exact face,
//! weight and size the text is set in (tracking included), so the fit is the
//! layout's own arithmetic, not a guess.

use facet::fonts;
use facet::tokens::TypeRole;
use facet::{Measure, Typeset as _};
use gpui::{App, Font, FontStyle, FontWeight, HighlightStyle, Pixels, SharedString, px};
use std::ops::Range;

/// The smallest a name steps down to, as a share of its role's size.
const NAME_FLOOR: f32 = 0.6;

/// The width `text` takes set in `role` (already resolved for its measure).
pub(crate) fn text_width(text: &str, role: &TypeRole, cx: &App) -> Pixels {
    let system = cx.text_system();
    let font = Font {
        family: SharedString::new_static(fonts::family(*role)),
        features: fonts::features(*role),
        fallbacks: None,
        weight: FontWeight(role.weight),
        style: if role.italic { FontStyle::Italic } else { FontStyle::Normal },
    };
    let id = system.resolve_font(&font);
    let size = px(role.size);
    let mut width = px(0.0);
    let mut count = 0_u32;
    for character in text.chars() {
        if let Ok(advance) = system.advance(id, size, character) {
            width += advance.width;
        }
        count += 1;
    }
    let tracking = role.tracking * role.size * count.saturating_sub(1) as f32;
    width + px(tracking)
}

/// Splits an identifier where a reader's eye does: `RelationLabel` →
/// `Relation|Label`, `as_str` → `as_|str`, `Page::relations` →
/// `Page::|relations`, `HTTPServer` → `HTTP|Server`.
pub(crate) fn identifier_segments(name: &str) -> Vec<&str> {
    let chars = name.char_indices().collect::<Vec<_>>();
    let mut cuts = Vec::new();
    for window in 1..chars.len() {
        let (at, current) = chars[window];
        let previous = chars[window - 1].1;
        let next = chars.get(window + 1).map(|(_, c)| *c);
        let hump = current.is_uppercase()
            && (previous.is_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_uppercase() && next.is_some_and(char::is_lowercase)));
        let after_underscore = previous == '_' && current != '_';
        let after_path = previous == ':' && current != ':' && window >= 2 && chars[window - 2].1 == ':';
        let after_dot = previous == '.' && current != '.';
        if hump || after_underscore || after_path || after_dot {
            cuts.push(at);
        }
    }
    let mut segments = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for cut in cuts {
        segments.push(&name[start..cut]);
        start = cut;
    }
    segments.push(&name[start..]);
    segments
}

/// Fits a name into `width`: its lines, and the role they are set in.
pub(crate) fn fit_name(name: &str, base: TypeRole, measure: &Measure, width: Pixels, cx: &App) -> (Vec<String>, TypeRole) {
    let resolved = measure.role(base);
    let width = width * 0.98;
    let segments = identifier_segments(name);
    let mut factor = 1.0_f32;
    loop {
        let role = TypeRole {
            size: resolved.size * factor,
            line: resolved.line * factor,
            ..resolved
        };
        let widest = segments
            .iter()
            .map(|segment| text_width(segment, &role, cx))
            .fold(px(0.0), Pixels::max);
        let at_floor = factor <= NAME_FLOOR + f32::EPSILON;
        if widest <= width || at_floor {
            return (pack(&segments, &role, width, at_floor, cx), role);
        }
        factor = (factor - 0.05).max(NAME_FLOOR);
    }
}

/// `text` in lines no wider than `width`, broken at identifier boundaries
/// (between characters only when one segment is wider than a line).
pub(crate) fn wrap_identifier(text: &str, role: &TypeRole, width: Pixels, cx: &App) -> Vec<String> {
    pack(&identifier_segments(text), role, width * 0.98, true, cx)
}

/// Greedy line packing; at the floor a segment wider than the line breaks
/// between characters (the last resort — still never a clip).
fn pack(segments: &[&str], role: &TypeRole, width: Pixels, split: bool, cx: &App) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for segment in segments {
        let candidate = format!("{line}{segment}");
        if line.is_empty() || text_width(&candidate, role, cx) <= width {
            line = candidate;
        } else {
            lines.push(std::mem::take(&mut line));
            line = (*segment).to_owned();
        }
        if split && text_width(&line, role, cx) > width {
            let mut piece = String::new();
            for character in line.chars() {
                piece.push(character);
                if text_width(&piece, role, cx) > width && piece.chars().count() > 1 {
                    let last = piece.pop().unwrap_or_default();
                    lines.push(std::mem::take(&mut piece));
                    piece.push(last);
                }
            }
            line = piece;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// A name set in its fitted lines, one text per line.
pub(crate) fn name_lines(lines: &[String], role: TypeRole, color: impl Into<gpui::Hsla>) -> gpui::Div {
    use gpui::{ParentElement as _, Styled as _};
    let color = color.into();
    gpui::div().flex().flex_col().children(lines.iter().enumerate().map(|(index, line)| {
        facet::probe::text(
            gpui::ElementId::Name(SharedString::from(format!("name:{index}:{line}"))),
            SharedString::from(line.clone()),
            role,
            1.0,
            facet::probe::TextOverflow::Clip,
            gpui::div().whitespace_nowrap().typeset_at(role, 1.0).text_color(color).child(line.clone()),
        )
    }))
}

/// One visual line of wrapped code.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CodeLine {
    /// Which source line it belongs to (0-based within the text).
    pub source: usize,
    /// Whether it continues the line above (no line number).
    pub continued: bool,
    /// The text, hanging indent included.
    pub text: String,
    /// Highlight runs, in this text's byte offsets.
    pub runs: Vec<(Range<usize>, HighlightStyle)>,
}

/// How many monospace columns fit in `width` for `role`.
pub(crate) fn columns(width: Pixels, role: &TypeRole, cx: &App) -> usize {
    let advance = text_width("0", role, cx) + px(role.tracking * role.size);
    if advance <= px(0.0) {
        return 80;
    }
    ((f32::from(width) / f32::from(advance)).floor() as usize).max(8)
}

/// Soft-wraps `text` (with `runs` over its whole byte range) into lines of
/// at most `columns` characters at token boundaries, continuation lines
/// hanging four columns past their source line's indent.
pub(crate) fn wrap_code(text: &str, runs: &[(Range<usize>, HighlightStyle)], columns: usize) -> Vec<CodeLine> {
    let mut out = Vec::new();
    let mut offset = 0;
    for (source, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_runs = runs
            .iter()
            .filter_map(|(range, style)| {
                let start = range.start.max(offset);
                let end = range.end.min(offset + line.len());
                (start < end).then(|| (start - offset..end - offset, *style))
            })
            .collect::<Vec<_>>();
        let indent = line.len() - line.trim_start().len();
        let hang = " ".repeat((indent + 4).min(columns / 2));
        let mut start = 0;
        let mut first = true;
        loop {
            let room = if first { columns } else { columns.saturating_sub(hang.len()).max(8) };
            let rest = &line[start..];
            let (piece_end, done) = if rest.chars().count() <= room {
                (line.len(), true)
            } else {
                (start + break_at(rest, room), false)
            };
            let piece = &line[start..piece_end];
            let (prefix, shift) = if first { (String::new(), 0) } else { (hang.clone(), hang.len()) };
            let piece_text = if first { piece.to_owned() } else { piece.trim_start().to_owned() };
            let trimmed = piece.len() - piece_text.len();
            let runs = line_runs
                .iter()
                .filter_map(|(range, style)| {
                    let from = range.start.max(start + trimmed);
                    let to = range.end.min(piece_end);
                    (from < to).then(|| (from - start - trimmed + shift..to - start - trimmed + shift, *style))
                })
                .collect();
            out.push(CodeLine {
                source,
                continued: !first,
                text: format!("{prefix}{piece_text}"),
                runs,
            });
            if done {
                break;
            }
            start = piece_end;
            first = false;
        }
        offset += raw.len() + 1;
    }
    out
}

/// Where to end a piece of at most `room` characters: after the last token
/// boundary inside it, or at `room` characters when there is none.
fn break_at(rest: &str, room: usize) -> usize {
    let limit = rest.char_indices().nth(room).map_or(rest.len(), |(at, _)| at);
    let bytes = rest.as_bytes();
    let mut best = None;
    for (at, character) in rest[..limit].char_indices() {
        let end = at + character.len_utf8();
        let boundary = match character {
            ' ' | ',' | '(' | '[' | '{' | '<' | ';' => Some(end),
            ':' if at > 0 && bytes[at - 1] == b':' => Some(end),
            '-' | '=' if bytes.get(at + 1) == Some(&b'>') && at > 0 => Some(at),
            _ => None,
        };
        if let Some(boundary) = boundary
            && boundary > 0
            && boundary < rest.len()
        {
            best = Some(boundary);
        }
    }
    best.filter(|at| *at > limit / 3).unwrap_or(limit.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_split_where_the_eye_does() {
        assert_eq!(identifier_segments("RelationLabel"), ["Relation", "Label"]);
        assert_eq!(identifier_segments("as_str"), ["as_", "str"]);
        assert_eq!(identifier_segments("Page::relations"), ["Page::", "relations"]);
        assert_eq!(identifier_segments("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(identifier_segments("x"), ["x"]);
        assert_eq!(identifier_segments("parse_json_into_struct").concat(), "parse_json_into_struct");
    }

    #[test]
    fn code_wraps_at_tokens_with_a_hanging_indent_and_loses_nothing() {
        let line = "    Typed(SemanticLinkKind, RelationDirection),";
        let lines = wrap_code(line, &[], 30);
        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines.iter().all(|line| line.text.chars().count() <= 30), "{lines:?}");
        assert!(!lines[0].continued && lines[1].continued);
        assert!(lines[1].text.starts_with("        "), "hangs past the indent: {lines:?}");
        // Nothing is lost: the pieces, unhung, are the line.
        let joined = lines.iter().map(|line| line.text.trim_start().to_owned()).collect::<Vec<_>>().join("");
        assert_eq!(joined.replace(' ', ""), line.replace(' ', ""));
        // No break inside a token when a boundary exists.
        assert!(lines.iter().all(|line| !line.text.trim_end().ends_with("SemanticLinkKi")), "{lines:?}");
    }

    #[test]
    fn runs_follow_their_text_across_a_wrap() {
        let text = "pub fn relation_label(link: &Link) -> RelationLabel";
        let style = HighlightStyle::default();
        let start = text.find("RelationLabel").expect("name");
        let lines = wrap_code(text, &[(start..start + "RelationLabel".len(), style)], 24);
        let marked = lines
            .iter()
            .flat_map(|line| line.runs.iter().map(move |(range, _)| line.text[range.clone()].to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(marked.concat(), "RelationLabel");
    }
}
