//! The classification of a bracket run's *opening delimiter*, and the closed
//! set of things one bracket run can turn into.
//!
//! # Why this file exists
//!
//! `prose.rs` used to decide "is this the start of a link attempt?" with an
//! inline `bool`-producing `match` that accepted two different spellings and
//! then forgot which one it had seen. One of those spellings is rustdoc's own
//! syntax (fidelity); the other is a typo we correct (a repair). Collapsing
//! both to `true` is what made the repair unrecordable downstream.
//!
//! [`open_shape`] replaces that predicate with a *typed* answer, and
//! [`DelimiterShape::repair_kind`] is the compile-time chokepoint: a new
//! accepted spelling cannot reach the link path without first declaring
//! whether rendering a link from it is a repair, and if so which one.

use pulldown_cmark::Event;

use crate::wire::LinkRepairKind;

/// The literal delimiter spelling observed at the start of a bracket run.
///
/// Closed on purpose. This is the ONLY gate into the shortcut-link path
/// ([`open_shape`], below), so a future spelling we decide to accept must add a
/// variant here — and [`DelimiterShape::repair_kind`] will not compile until
/// that variant has declared whether it is a repair and, if so, which
/// [`LinkRepairKind`] it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DelimiterShape {
    /// `Event::Text("[")` — `[foo]` or `` [`foo`] ``. Rustdoc's own syntax.
    Canonical,
    /// `Event::Code(s)` with `s.starts_with('[') && !s.ends_with(']')` —
    /// `` `[foo`] ``, backtick and bracket transposed.
    TransposedOpen,
}

impl DelimiterShape {
    /// `None` when this spelling is what rustdoc specifies; `Some(kind)` when
    /// rendering a link from it is a repair.
    ///
    /// No wildcard arm — this is the assertion that no shape can enter the
    /// link path without a repair verdict.
    pub(crate) fn repair_kind(self) -> Option<LinkRepairKind> {
        match self {
            Self::Canonical => None,
            Self::TransposedOpen => Some(LinkRepairKind::TransposedOpenDelimiter),
        }
    }
}

/// Every reader-visible result one bracket run can have. Closed, no wildcard.
///
/// This is the exhaustive answer to "what does the engine do to a `[...]` in
/// doc prose?" — the only enum anyone has to read to know.
///
/// `LinkRepairKind` covers repairs *that produce a link*. It would be
/// dishonest to stop there: `consume_bracket_run` has three other
/// reader-visible behaviours that also diverge from a strict reading, so they
/// are typed here alongside, and the full surface is enumerable in one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutOutcome {
    /// Well-formed rustdoc shortcut, resolved. Byte-for-byte what rustdoc
    /// does. Not a repair.
    Linked,
    /// A malformed spelling, resolved anyway. A repair — carries which one.
    Repaired(LinkRepairKind),
    /// Path-shaped but unresolved: brackets stripped, inner text shown as
    /// plain text or `Code`. This is the L17 fallback, already owned and
    /// tested (`docs/LIMITATIONS.md` L17 / L27). Produces no link.
    Stripped,
    /// Not path-shaped (`[!NOTE]`, `arr[0]`, an unclosed run): the exact
    /// consumed span is re-emitted verbatim. Produces no link.
    Verbatim,
    /// `[]`, or a reference-style `[text][ref]`'s trailing `[ref]`: nothing
    /// for a reader to see. Produces no link.
    Dropped,
}

/// Classify `event` as the opening of a bracket run. Returns `None` when this
/// event is not a link-attempt opener.
///
/// **This function is the only entry to the shortcut-link path.**
///
/// The two accepted shapes:
///
/// - `Event::Text("[")` — the ordinary, well-formed opening delimiter for
///   `[foo]` / `` [`foo`] ``.
/// - `Event::Code(text)` where `text` starts with `'['` but is not *also*
///   closed within the same span (`[foo]` inside one pair of backticks stays a
///   normal, legitimate code span like `` `[u8]` `` and is deliberately
///   excluded here). This is the malformed case: a doc comment with its
///   backtick and bracket swapped, e.g. `` `[memrchr_iter`] `` instead of
///   `` [`memrchr_iter`] ``. Code spans bind tighter than link brackets in
///   CommonMark, so pulldown-cmark folds the stray `[` into the code span
///   instead of emitting it as its own token — and without this branch that
///   `[` (plus the bare `]` that follows as its own `Text` event) leaked
///   straight through, half of it rendered as inline code.
pub(crate) fn open_shape(event: &Event<'_>) -> Option<DelimiterShape> {
    match event {
        Event::Text(t) if t.as_ref() == "[" => Some(DelimiterShape::Canonical),
        Event::Code(t) => {
            let s = t.as_ref();
            (s.starts_with('[') && !s.ends_with(']')).then_some(DelimiterShape::TransposedOpen)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::CowStr;

    /// The canonical opener is rustdoc's own syntax, so producing a link from
    /// it must never be recorded as a repair — otherwise every ordinary link
    /// on every page would carry a bogus "we changed this" mark.
    #[test]
    fn canonical_opener_is_not_a_repair() {
        let ev = Event::Text(CowStr::Borrowed("["));
        assert_eq!(open_shape(&ev), Some(DelimiterShape::Canonical));
        assert_eq!(DelimiterShape::Canonical.repair_kind(), None);
    }

    /// The transposed opener is the one and only repair, and it must classify
    /// as such at the gate — not somewhere downstream where it could be lost.
    #[test]
    fn transposed_opener_is_the_transposed_repair() {
        let ev = Event::Code(CowStr::Borrowed("[memrchr_iter"));
        assert_eq!(open_shape(&ev), Some(DelimiterShape::TransposedOpen));
        assert_eq!(
            DelimiterShape::TransposedOpen.repair_kind(),
            Some(LinkRepairKind::TransposedOpenDelimiter)
        );
    }

    /// `` `[u8]` `` is a legitimate code span, not a broken link. If this ever
    /// classified as an opener we would turn slice-type notation into links.
    #[test]
    fn self_closed_code_span_is_not_an_opener() {
        assert_eq!(open_shape(&Event::Code(CowStr::Borrowed("[u8]"))), None);
        assert_eq!(open_shape(&Event::Code(CowStr::Borrowed("Vec"))), None);
        assert_eq!(open_shape(&Event::Text(CowStr::Borrowed("]"))), None);
        assert_eq!(open_shape(&Event::Text(CowStr::Borrowed("hello"))), None);
        assert_eq!(open_shape(&Event::SoftBreak), None);
    }
}
