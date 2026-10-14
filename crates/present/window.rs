//! Bounded windows over list-bearing answers.
//!
//! One policy, shared by the CLI and the MCP: **no read ever fails because of
//! its size**. A declaration's members, a package outline, a reference list or
//! a neighbourhood is rendered up to a budget, then a single closing line says
//! how many rows remain and spells the exact next call.
//!
//! The owner pages search, resolve and graph rows with its own authenticated
//! cursors. Everything else arrives as one complete list the owner never paged
//! (a type's members are rows of its package, a reference list is one reply),
//! so this module pages *that* list at the presentation boundary. A window is
//! stateless: its cursor names the list, the offset, and a fingerprint of the
//! rows it was cut from. A cursor taken from a list that has since changed
//! refuses with the shared stale-cursor fault instead of silently skipping or
//! repeating rows.

use crate::fault::Fault;
use backend_client::ClientError;

/// Spelling every window cursor starts with.
pub const WINDOW_PREFIX: &str = "w1-";

/// Rows one window can never exceed, whatever limit the caller asks for. It
/// is the largest page bound the shared grammar admits.
pub const MAX_WINDOW_ROWS: usize = 200;

/// Text bytes a window's rows may spend when the caller supplied no limit.
///
/// Under 1,500 tokens with the header and the closing line around it: enough
/// for every member of an ordinary type, small enough that a response never
/// dominates an agent's context.
pub const WINDOW_TEXT_BUDGET: usize = 5 * 1024 + 512;

/// Text bytes a window's rows may spend when the caller asked for a count.
pub const WINDOW_TEXT_CEILING: usize = 24 * 1024;

/// Typed-projection bytes a window's rows may spend, with or without a limit.
///
/// The typed rows travel beside the readable text, and the whole response
/// shares one transport budget. A window measures its rows as serialized, so
/// no list can make the pair exceed it.
pub const WINDOW_DTO_BUDGET: usize = 24 * 1024;

/// Which list a window cursor pages.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WindowKind {
    /// The members of one declaration.
    Members,
    /// The rows of a product list: references, reads, diffs.
    Rows,
    /// The entries of a package outline, in outline order.
    Outline,
    /// The lines of one declaration's source text.
    Source,
}

impl WindowKind {
    const fn letter(self) -> char {
        match self {
            Self::Members => 'm',
            Self::Rows => 'r',
            Self::Outline => 'o',
            Self::Source => 's',
        }
    }

    const fn from_letter(letter: char) -> Option<Self> {
        match letter {
            'm' => Some(Self::Members),
            'r' => Some(Self::Rows),
            'o' => Some(Self::Outline),
            's' => Some(Self::Source),
            _ => None,
        }
    }

    /// Returns the plural noun the closing line counts.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Members => "members",
            Self::Rows => "rows",
            Self::Outline => "declarations",
            Self::Source => "source lines",
        }
    }
}

/// A stateless continuation into one window-paged list.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowCursor {
    kind: WindowKind,
    offset: usize,
    fingerprint: u64,
}

impl WindowCursor {
    /// Names the first row of the next window.
    #[must_use]
    pub const fn new(kind: WindowKind, offset: usize, fingerprint: u64) -> Self {
        Self {
            kind,
            offset,
            fingerprint,
        }
    }

    /// Returns which list this cursor pages.
    #[must_use]
    pub const fn kind(self) -> WindowKind {
        self.kind
    }

    /// Returns the zero-based index of the first row the window shows.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Returns whether `text` is spelled as a window cursor.
    #[must_use]
    pub fn is_window_token(text: &str) -> bool {
        text.starts_with(WINDOW_PREFIX)
    }

    /// Renders the cursor as a short, URL-safe token: `w1-m-35-9f3a…`.
    #[must_use]
    pub fn encode(self) -> String {
        format!(
            "{WINDOW_PREFIX}{}-{}-{:016x}",
            self.kind.letter(),
            self.offset,
            self.fingerprint
        )
    }

    /// Reads a cursor back.
    ///
    /// # Errors
    ///
    /// Returns a usage fault naming `cursor` when the token is not one this
    /// module issued.
    pub fn decode(text: &str) -> Result<Self, Fault> {
        let malformed = || {
            Fault::usage(
                "cursor",
                "the cursor is not a continuation this command issued; \
                 copy it exactly from the previous page",
            )
        };
        let body = text.strip_prefix(WINDOW_PREFIX).ok_or_else(malformed)?;
        let mut parts = body.splitn(3, '-');
        let kind = parts
            .next()
            .filter(|part| part.len() == 1)
            .and_then(|part| part.chars().next())
            .and_then(WindowKind::from_letter)
            .ok_or_else(malformed)?;
        let offset = parts
            .next()
            .and_then(|part| part.parse::<usize>().ok())
            .ok_or_else(malformed)?;
        let fingerprint = parts
            .next()
            .filter(|part| part.len() == 16)
            .and_then(|part| u64::from_str_radix(part, 16).ok())
            .ok_or_else(malformed)?;
        Ok(Self::new(kind, offset, fingerprint))
    }
}

/// What the caller asked of a window: how many rows, and from where.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WindowRequest {
    limit: Option<usize>,
    cursor: Option<WindowCursor>,
}

impl WindowRequest {
    /// The first window, sized by the budget.
    #[must_use]
    pub const fn first() -> Self {
        Self {
            limit: None,
            cursor: None,
        }
    }

    /// Bounds the window to at most `limit` rows.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit.clamp(1, MAX_WINDOW_ROWS));
        self
    }

    /// Resumes at a cursor an earlier window issued.
    #[must_use]
    pub const fn with_cursor(mut self, cursor: WindowCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Reads the request from the textual `limit` and `cursor` operands the
    /// CLI and the MCP both carry.
    ///
    /// # Errors
    ///
    /// Returns a usage fault naming the operand that is not admissible.
    pub fn parse(limit: Option<&str>, cursor: Option<&str>) -> Result<Self, Fault> {
        let mut request = Self::first();
        if let Some(limit) = limit {
            let count = limit
                .parse::<usize>()
                .ok()
                .filter(|count| (1..=MAX_WINDOW_ROWS).contains(count))
                .ok_or_else(|| Fault::usage("limit", "limit must be an integer from 1 through 200"))?;
            request = request.with_limit(count);
        }
        if let Some(cursor) = cursor {
            request = request.with_cursor(WindowCursor::decode(cursor)?);
        }
        Ok(request)
    }

    /// Returns the caller's row bound, when one was supplied.
    #[must_use]
    pub const fn limit(self) -> Option<usize> {
        self.limit
    }

    /// Returns the cursor to resume at, when one was supplied.
    #[must_use]
    pub const fn cursor(self) -> Option<WindowCursor> {
        self.cursor
    }

    /// Returns the zero-based index of the window's first row, checking that
    /// the cursor was cut from this kind of list at this exact content.
    ///
    /// # Errors
    ///
    /// Returns the shared stale-cursor fault when the cursor belongs to a
    /// different list or to rows that have since changed, and a usage fault
    /// when it points past the end of the list.
    pub fn start(self, kind: WindowKind, fingerprint: u64, total: usize) -> Result<usize, Fault> {
        let Some(cursor) = self.cursor else {
            return Ok(0);
        };
        if cursor.kind != kind || cursor.fingerprint != fingerprint {
            return Err(Fault::from_client_error(
                &ClientError::StaleCursor,
                crate::fault::Operand::Argument("cursor".to_owned()),
            ));
        }
        if cursor.offset > total {
            return Err(Fault::usage(
                "cursor",
                "the cursor points past the end of the list; restart without a cursor",
            ));
        }
        Ok(cursor.offset)
    }
}

/// One cut of a list: which rows a response carries and how many remain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Window {
    kind: WindowKind,
    offset: usize,
    shown: usize,
    total: usize,
    fingerprint: u64,
}

impl Window {
    /// Chooses how many rows of a list to show.
    ///
    /// `text_costs` and `dto_costs` are the bytes each row spends in the
    /// readable text and in the typed projection, in list order; they are
    /// read from `offset` on. A window shows the rows the caller's `limit`
    /// names, or as many as fit the budget, never more than the ceilings, and
    /// always at least one row when any remain: a single large row is clipped
    /// by its renderer, never withheld.
    #[must_use]
    pub fn select(
        kind: WindowKind,
        request: WindowRequest,
        offset: usize,
        fingerprint: u64,
        text_costs: &[usize],
        dto_costs: &[usize],
    ) -> Self {
        Self::select_within(
            kind,
            request,
            offset,
            fingerprint,
            text_costs,
            dto_costs,
            WINDOW_TEXT_BUDGET,
        )
    }

    /// As [`Self::select`], with the text budget a window of this list spends
    /// when the caller supplied no limit. Source text, which an agent reads
    /// to the end, spends more than a list of one-line rows.
    #[must_use]
    pub fn select_within(
        kind: WindowKind,
        request: WindowRequest,
        offset: usize,
        fingerprint: u64,
        text_costs: &[usize],
        dto_costs: &[usize],
        default_text_budget: usize,
    ) -> Self {
        let total = text_costs.len();
        let offset = offset.min(total);
        let text_budget = if request.limit.is_some() {
            WINDOW_TEXT_CEILING
        } else {
            default_text_budget
        };
        let cap = request.limit.unwrap_or(MAX_WINDOW_ROWS);
        let mut text = 0_usize;
        let mut dto = 0_usize;
        let mut shown = 0_usize;
        for (index, cost) in text_costs.iter().enumerate().skip(offset) {
            if shown >= cap {
                break;
            }
            let typed = dto_costs.get(index).copied().unwrap_or(0);
            let next_text = text.saturating_add(*cost);
            let next_dto = dto.saturating_add(typed);
            if shown > 0 && (next_text > text_budget || next_dto > WINDOW_DTO_BUDGET) {
                break;
            }
            text = next_text;
            dto = next_dto;
            shown += 1;
        }
        Self {
            kind,
            offset,
            shown,
            total,
            fingerprint,
        }
    }

    /// Returns which list was cut.
    #[must_use]
    pub const fn kind(self) -> WindowKind {
        self.kind
    }

    /// Returns the zero-based index of the first row shown.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Returns how many rows this window shows.
    #[must_use]
    pub const fn shown(self) -> usize {
        self.shown
    }

    /// Returns how many rows the whole list holds.
    #[must_use]
    pub const fn total(self) -> usize {
        self.total
    }

    /// Returns the half-open range of rows shown.
    #[must_use]
    pub const fn range(self) -> core::ops::Range<usize> {
        self.offset..self.offset + self.shown
    }

    /// Returns how many rows follow this window.
    #[must_use]
    pub const fn remaining(self) -> usize {
        self.total.saturating_sub(self.offset + self.shown)
    }

    /// Returns whether rows follow this window.
    #[must_use]
    pub const fn has_more(self) -> bool {
        self.remaining() > 0
    }

    /// Returns whether this window leaves any row of the list out: it starts
    /// past the first row, or rows follow it.
    #[must_use]
    pub const fn is_cut(self) -> bool {
        self.offset > 0 || self.has_more()
    }

    /// Returns the cursor that resumes after this window, when rows remain.
    #[must_use]
    pub fn next(self) -> Option<WindowCursor> {
        self.has_more().then(|| {
            WindowCursor::new(self.kind, self.offset + self.shown, self.fingerprint)
        })
    }

    /// Returns the fingerprint of the list this window was cut from.
    #[must_use]
    pub const fn fingerprint(self) -> u64 {
        self.fingerprint
    }
}

/// Fingerprints the rows of a list so a cursor can tell the list changed.
///
/// FNV-1a over each part and a separator: stable across processes, Rust
/// versions and platforms, which a cursor handed from one CLI run to the next
/// requires. It detects change; it is not an authority and carries no secret.
#[must_use]
pub fn fingerprint<'a>(parts: impl IntoIterator<Item = &'a str>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for part in parts {
        for byte in part.bytes().chain(core::iter::once(0x1f)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// Clips one line to `limit` characters on a character boundary, collapsing
/// every run of whitespace (including newlines) to one space first.
///
/// A signature or summary spans several lines in source; a list row is one.
#[must_use]
pub fn one_line(text: &str, limit: usize) -> String {
    let mut line = String::with_capacity(text.len().min(limit.saturating_add(4)));
    let mut pending_space = false;
    let mut count = 0_usize;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !line.is_empty();
            continue;
        }
        if pending_space {
            if count >= limit {
                line.push('…');
                return line;
            }
            line.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= limit {
            line.push('…');
            return line;
        }
        line.push(character);
        count += 1;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_round_trips_and_stays_short() {
        let cursor = WindowCursor::new(WindowKind::Members, 35, 0x9f3a_1c0b_5d2e_7a44);
        let token = cursor.encode();
        assert_eq!(token, "w1-m-35-9f3a1c0b5d2e7a44");
        assert!(token.len() < 40);
        assert_eq!(WindowCursor::decode(&token).expect("round trip"), cursor);
        assert!(WindowCursor::is_window_token(&token));
    }

    #[test]
    fn a_foreign_or_damaged_cursor_is_a_usage_fault() {
        for token in [
            "",
            "pc4-abc",
            "w1-",
            "w1-x-3-0000000000000000",
            "w1-m--1-0000000000000000",
            "w1-m-3-xyz",
            "w1-m-3-00000000000000",
            "w1-mm-3-0000000000000000",
        ] {
            let fault = WindowCursor::decode(token).expect_err(token);
            assert!(fault.slug().is_usage(), "{token}");
        }
    }

    #[test]
    fn a_cursor_from_another_list_or_changed_rows_is_stale() {
        let cursor = WindowCursor::new(WindowKind::Members, 3, 7);
        let request = WindowRequest::first().with_cursor(cursor);
        assert_eq!(request.start(WindowKind::Members, 7, 10).expect("same"), 3);
        for (kind, print) in [(WindowKind::Rows, 7), (WindowKind::Members, 8)] {
            let fault = request.start(kind, print, 10).expect_err("stale");
            assert_eq!(fault.slug(), crate::FaultSlug::CursorMismatch);
        }
        let past = request.start(WindowKind::Members, 7, 2).expect_err("past");
        assert!(past.slug().is_usage());
    }

    #[test]
    fn a_window_fills_the_budget_and_spells_the_cursor_for_the_rest() {
        let text = vec![1_000_usize; 20];
        let dto = vec![100_usize; 20];
        let first = Window::select(WindowKind::Members, WindowRequest::first(), 0, 5, &text, &dto);
        assert_eq!(first.shown(), 5, "5.5 KiB of 1,000-byte rows");
        assert_eq!(first.remaining(), 15);
        let next = first.next().expect("rows remain");
        assert_eq!((next.offset(), next.kind()), (5, WindowKind::Members));
        let last = Window::select(WindowKind::Members, WindowRequest::first(), 18, 5, &text, &dto);
        assert_eq!((last.shown(), last.remaining()), (2, 0));
        assert!(last.next().is_none());
    }

    #[test]
    fn an_explicit_limit_is_honoured_up_to_the_ceilings() {
        let text = vec![100_usize; 400];
        let dto = vec![50_usize; 400];
        let asked = WindowRequest::first().with_limit(150);
        let window = Window::select(WindowKind::Rows, asked, 0, 1, &text, &dto);
        assert_eq!(window.shown(), 150);
        // The typed projection's ceiling still binds an enormous request.
        let heavy = vec![5_000_usize; 400];
        let window = Window::select(WindowKind::Rows, asked, 0, 1, &text, &heavy);
        assert_eq!(window.shown(), WINDOW_DTO_BUDGET / 5_000);
    }

    #[test]
    fn one_row_larger_than_the_budget_is_still_shown() {
        let window = Window::select(
            WindowKind::Rows,
            WindowRequest::first(),
            0,
            1,
            &[100_000, 10],
            &[200_000, 10],
        );
        assert_eq!((window.shown(), window.remaining()), (1, 1));
    }

    #[test]
    fn fingerprints_separate_parts_and_are_stable() {
        assert_eq!(fingerprint(["a", "b"]), fingerprint(["a", "b"]));
        assert_ne!(fingerprint(["ab"]), fingerprint(["a", "b"]));
        assert_ne!(fingerprint(["a", "b"]), fingerprint(["b", "a"]));
        // Pinned: a cursor issued by one build must verify under another.
        assert_eq!(fingerprint(["x"]), 0x08f0_e907_b58d_3dc8_u64);
    }

    #[test]
    fn one_line_collapses_whitespace_and_clips_on_a_character() {
        assert_eq!(one_line("func  (c *Color)\n\tAdd()", 80), "func (c *Color) Add()");
        assert_eq!(one_line("  padded  ", 80), "padded");
        assert_eq!(one_line("héllo wörld", 5), "héllo…");
        assert_eq!(one_line("héllo", 5), "héllo");
        assert_eq!(one_line("", 5), "");
    }
}
