//! ProseBlock builder — converts pulldown-cmark event streams into `ProseBlock`s.
//!
//! Also contains the shortcut-link consumer (`consume_bracket_run`), the one
//! constructor of a shortcut `InlineRun::Link` (`shortcut_link`), and inline-run
//! helpers. The opening delimiter is classified by `super::shortcut::open_shape`
//! *before* anything here runs, which is what lets `shortcut_link` decide
//! `Authored` vs `Repaired` from the observed spelling rather than from a
//! caller's assertion — see `crate::wire::repair` for why that matters.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

use crate::wire::{
    InlineRun, LangId, LinkOrigin, LinkRepair, LinkTarget, ProseBlock, SharedStr, SymbolKey,
};

use super::doc_link_table::DocLinkTable;
use super::shortcut::{DelimiterShape, ShortcutOutcome, open_shape};

/// Convert a stream of pulldown-cmark events into `ProseBlock`s.
///
/// The heading text is prepended as a `Heading` block when non-empty.
pub(crate) fn build_prose_blocks(
    events: Vec<Event<'_>>,
    source_spans: Vec<std::ops::Range<usize>>,
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
        //
        // See `consume_bracket_run` for the full algorithm and the reasoning
        // behind it (module docs above explain why a *separate* fallback
        // shape is what let raw `[markup]` leak to the reader in the first
        // place). The accepted opening shapes — and, crucially, *which of
        // them are rustdoc's own syntax and which are typos we repair* —
        // live in `shortcut::open_shape`. It returns a typed
        // `DelimiterShape` rather than a `bool` precisely so the answer
        // survives the call: a `bool` here is what made the repair
        // unrecordable downstream.
        //
        // Once a shape is recognised we have committed to treating this
        // position as a link *attempt*; `consume_bracket_run` is the only
        // place downstream allowed to decide what the reader actually sees
        // for it, and it never re-emits a bare `[` or `]` character.
        // ## The `has_declared_links()` gate — contract
        //
        // Brackets in doc prose are interpreted as intra-doc-link syntax
        // **only for a symbol that declared at least one doc link**
        // (`doc_link_table.has_declared_links()`) — this mirrors rustdoc's
        // own behaviour. For such a symbol, an unresolved shortcut like
        // `[SomeType]` has its brackets stripped and renders as plain
        // `SomeType` — never as leaked raw markdown (`consume_bracket_run`,
        // below, is what guarantees that). For a symbol with **no** declared
        // links at all, bracketed text is literal prose and passes through
        // **verbatim**, brackets included, because `[NOTE]`, a citation
        // marker, or `arr[0]` are all far likelier than a link attempt, and
        // stripping brackets from arbitrary prose corrupts content.
        //
        // This gate is load-bearing and adversarially tested on both sides:
        // `empty_doc_links_preserves_bracketed_prose_verbatim` in
        // `tests/hyperlink_flows.rs` pins the no-declared-links side (it is
        // the regression test for exactly the failure mode above — every
        // bracketed phrase losing its brackets, including text meant
        // literally — and it caught this when the guard was previously
        // dropped); `chunk/walk/tests.rs`'s "L17 adversarial regression
        // suite" pins the declared-links side.
        //
        // ### What this heuristic cannot do
        //
        // The decision is made **per symbol**, not per bracket run, because
        // nothing downstream of the producer records *which* bracket run in
        // a doc comment a given `DocLink` came from — `DocLink` carries only
        // `target`/`label`, no byte span. So a symbol that declares even one
        // doc link anywhere in its comment has *every* bracket run in that
        // comment scanned as a potential shortcut, including ones with
        // nothing to do with the declared link — a literal `[NOTE]` in a
        // comment that also happens to link `[SomeOtherType]` loses its
        // brackets too. See docs/LIMITATIONS.md for the tracked entry; the real
        // fix is span-accurate link-attempt data from the producer, which
        // does not exist today.
        let bracket_open = (doc_link_table.has_declared_link_at(source_spans[i].clone())
            && state != BlockState::Code
            && link_url.is_none())
        .then(|| open_shape(&events[i]))
        .flatten();

        if let Some(shape) = bracket_open {
            let (consumed, runs, _outcome) =
                consume_bracket_run(&events, i, shape, doc_link_table, in_strong, in_em);
            for run in runs {
                push_inline(&mut inline_buf, &mut blockquote_inline, in_blockquote, run);
            }
            i += consumed.max(1);
            continue;
        }

        // A bare `]` with nothing else in the token is, by construction of
        // pulldown-cmark's delimiter-stack algorithm, always leftover
        // link-close debris — it is only ever isolated into its own event
        // when the tokenizer tried (and failed) to pair it with a `[`
        // somewhere earlier, whether or not that `[` is still visible here
        // (a run consumed above may have already accounted for the `[`).
        // Never echo it: a reader has no use for a stray closing bracket,
        // and showing one is the exact symptom this fix exists to remove.
        // Gated on the same span-aware declaration check as the opening
        // bracket above: with no declared link at this source position there
        // was no link attempt to leave debris, so an isolated `]` is the
        // author's own text and swallowing it would silently corrupt prose.
        if doc_link_table.has_declared_link_at(source_spans[i].clone())
            && state != BlockState::Code
            && link_url.is_none()
            && matches!(&events[i], Event::Text(t) if t.as_ref() == "]")
        {
            i += 1;
            continue;
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
                        doc_link_table.resolve(&url).map_or_else(
                            || LinkTarget::Url {
                                url: SharedStr::from(url.as_str()),
                            },
                            |key| LinkTarget::Symbol { key: key.clone() },
                        )
                    } else {
                        LinkTarget::Url {
                            url: SharedStr::from(url.as_str()),
                        }
                    };
                    // `Authored`, unconditionally and correctly: pulldown-cmark
                    // only ever emits `Tag::Link` for spellings CommonMark
                    // itself accepts, so anything reaching here was written the
                    // way the spec says. This is not an oversight — there is no
                    // malformed shape that can arrive on this path.
                    let run = InlineRun::Link {
                        text: SharedStr::from(link_text_buf.as_str()),
                        target,
                        origin: LinkOrigin::Authored,
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
// The one constructor of a shortcut link
// ---------------------------------------------------------------------------

/// The ONLY constructor of an `InlineRun::Link` on the shortcut path.
///
/// Takes the `DelimiterShape` by value, so a caller physically cannot resolve
/// a link without having classified the spelling first, and cannot assert
/// `Authored` for a spelling that was not canonical — the verdict comes from
/// `repair_kind()`, not from the caller's fingers.
///
/// `raw` must be the author's bytes for the whole consumed span (`` `[foo`] ``,
/// not a reconstruction of what they *meant*), because it is shown to the
/// reader as the evidence of what we changed.
fn shortcut_link(
    shape: DelimiterShape,
    raw: &str,
    candidate: &str,
    key: &SymbolKey,
) -> (InlineRun, ShortcutOutcome) {
    let (origin, outcome) = shape.repair_kind().map_or_else(
        || (LinkOrigin::Authored, ShortcutOutcome::Linked),
        |kind| {
            (
                LinkOrigin::Repaired(LinkRepair {
                    kind,
                    raw: SharedStr::from(raw),
                    resolved: SharedStr::from(candidate),
                    note: SharedStr::from(
                        format!(
                            "Repaired link \u{2014} the source reads {raw} ({}); we linked {candidate}.",
                            kind.label()
                        )
                        .as_str(),
                    ),
                }),
                ShortcutOutcome::Repaired(kind),
            )
        },
    );
    (
        InlineRun::Link {
            text: SharedStr::from(candidate),
            target: LinkTarget::Symbol { key: key.clone() },
            origin,
        },
        outcome,
    )
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

/// Depth cap for nested `[`/`]` delimiters inside a shortcut-link attempt.
/// Real doc comments never nest anywhere close to this; the cap exists only
/// so a pathological input can't turn the scan into an unbounded loop.
const MAX_BRACKET_DEPTH: u32 = 64;

/// Consume one rustdoc shortcut-link attempt (or a malformed look-alike)
/// starting at `events[start]`, and decide — once and for all, in one place
/// — what the reader sees for it.
///
/// # Why this exists (see also the module docs on `doc_link_table`)
///
/// Before this function, "a link that resolved" and "a link that didn't"
/// were handled by *differently shaped* code paths: a successful match
/// produced an `InlineRun::Link`, but a failed one fell through to
/// whichever ordinary single-event handler happened to run next — and that
/// handler had no idea it was looking at the leftover pieces of a link
/// attempt, so it echoed the `[`/`]` delimiter characters verbatim. That
/// asymmetry is what let raw markdown reach the reader.
///
/// This function is the only place that shape decision is made. Every
/// caller that recognises the *start* of a bracket run hands the rest of
/// the decision to this one function, and every branch inside it emits
/// plain `InlineRun`s (`Text`, `Code`, `Strong`, `Em`, or `Link`) — never
/// the raw delimiter characters. Which branch ran is reported back as a
/// [`ShortcutOutcome`], so "what did we do to this bracket?" has a typed
/// answer instead of being inferable only from the runs' shape.
///
/// # Contract
///
/// `shape` must be `open_shape(&events[start])`, i.e. the caller has already
/// classified the opening delimiter. Passing it (rather than re-deriving it
/// here) is what lets `shortcut_link` decide `Authored` vs `Repaired` from the
/// *observed spelling* instead of from a caller's assertion.
///
/// Returns `(events_consumed, runs_to_emit, outcome)`. `events_consumed` is
/// always at least 1, and `outcome` is the closed [`ShortcutOutcome`] — the
/// complete answer to what this run did for the reader.
///
/// # What happens on each shape
///
/// - Closes cleanly and the inner text is a plausible symbol path
///   (`is_symbol_path`): resolves via `doc_link_table`. Match → `Link`.
///   No match → `Code` (if the whole span came from one code run) or
///   `Text`/`Strong`/`Em` otherwise. Never brackets either way.
/// - Closes cleanly but the inner text is empty (`[]`): emits nothing —
///   there is no reader-facing content to show.
/// - Closes cleanly but the inner text is *not* path-shaped (e.g. a
///   `[!NOTE]` callout lead): this was never a link attempt, so the
///   original span — brackets, backticks and all — is preserved verbatim
///   as plain text, exactly as if no scan had happened.
/// - Immediately followed by a second bracket run with nothing in between
///   (`[text][ref]`, CommonMark reference-link syntax with no definition
///   for us to look up): `ref` is plumbing, not reader content — the same
///   treatment a resolved `[text](url)` link already gives its URL — so it
///   is resolved/rendered the same way as any other bracket run and then
///   discarded rather than shown.
/// - Never closes, or nests deeper than `MAX_BRACKET_DEPTH`: we cannot
///   responsibly guess at a link the source never finished, so the exact
///   span consumed so far is preserved verbatim (this is the one case
///   where a `[` can still reach the reader — an intentionally-unclosed
///   run is genuinely ambiguous, and no adversarial case in this module's
///   test suite produces one).
fn consume_bracket_run(
    events: &[Event<'_>],
    start: usize,
    shape: DelimiterShape,
    doc_link_table: &DocLinkTable,
    in_strong: bool,
    in_em: bool,
) -> (usize, Vec<InlineRun>, ShortcutOutcome) {
    let n = events.len();
    let mut idx = start;
    let mut depth: u32;
    let mut inner = String::new();
    let mut raw = String::new();
    let mut only_code = true;
    let mut saw_any = false;
    let mut trailing: Option<String> = None;

    // Prime from the starting event, keyed off the shape the caller already
    // classified. `raw` accumulates **the author's bytes**, not a
    // reconstruction of what they meant: for the transposed shape that means
    // the backtick that actually came first, then the bracket. Priming it as
    // `'[' + stripped` (as this did before) rebuilt the *canonical* spelling,
    // which made `raw` useless as evidence of a repair — and silently rewrote
    // the verbatim-preserve paths below, which show `raw` to the reader.
    match &events[idx] {
        Event::Text(_) => {
            depth = 1;
            raw.push('[');
            idx += 1;
        }
        Event::Code(t) => {
            let stripped = t.as_ref().strip_prefix('[').unwrap_or_else(|| t.as_ref());
            depth = 1;
            raw.push('`'); // ← the author's backtick, which came first
            raw.push('[');
            raw.push_str(stripped);
            raw.push('`'); // ← closes the code span the tokenizer saw
            if !stripped.is_empty() {
                inner.push_str(stripped);
                saw_any = true;
            }
            idx += 1;
        }
        _ => {
            // Contract violation by a caller — treat as an empty, already
            // "closed" run rather than panicking on malformed input.
            depth = 0;
        }
    }

    while idx < n && depth > 0 && depth <= MAX_BRACKET_DEPTH {
        match &events[idx] {
            Event::Text(t) if t.as_ref() == "[" => {
                depth += 1;
                raw.push('[');
                idx += 1;
            }
            Event::Text(t) if t.as_ref().starts_with(']') => {
                depth -= 1;
                raw.push(']');
                let rest = &t.as_ref()[1..];
                if depth == 0 {
                    if !rest.is_empty() {
                        trailing = Some(rest.to_owned());
                    }
                } else if !rest.is_empty() {
                    inner.push_str(rest);
                    raw.push_str(rest);
                    saw_any = true;
                }
                idx += 1;
            }
            Event::Text(t) => {
                inner.push_str(t.as_ref());
                raw.push_str(t.as_ref());
                only_code = false;
                saw_any = true;
                idx += 1;
            }
            Event::Code(t) => {
                inner.push_str(t.as_ref());
                raw.push('`');
                raw.push_str(t.as_ref());
                raw.push('`');
                saw_any = true;
                idx += 1;
            }
            Event::SoftBreak | Event::HardBreak => {
                inner.push(' ');
                raw.push(' ');
                only_code = false;
                saw_any = true;
                idx += 1;
            }
            _ => {
                // Anything else (nested inline formatting, images, ...)
                // inside a bracket run is not a shape we try to
                // reconstruct — stop scanning and let the "never closed"
                // handling below decide what to show.
                break;
            }
        }
    }

    if depth != 0 {
        // Never closed (or we bailed above, or hit the depth cap). We
        // cannot respond to a link the source never finished; show exactly
        // the span consumed so far, verbatim.
        return (
            idx - start,
            vec![make_text_run(&raw, in_strong, in_em)],
            ShortcutOutcome::Verbatim,
        );
    }

    let mut out = Vec::new();
    let candidate = inner.trim();
    let mut outcome = ShortcutOutcome::Dropped;

    if saw_any && !candidate.is_empty() {
        if is_symbol_path(candidate) {
            match doc_link_table.resolve(candidate) {
                // `shortcut_link` is the sole constructor here — it derives the
                // origin from `shape`, so this call site never gets to name one.
                Some(key) => {
                    let (run, o) = shortcut_link(shape, &raw, candidate, key);
                    outcome = o;
                    out.push(run);
                }
                None if only_code => {
                    outcome = ShortcutOutcome::Stripped;
                    out.push(InlineRun::Code {
                        text: SharedStr::from(candidate),
                    });
                }
                None => {
                    outcome = ShortcutOutcome::Stripped;
                    out.push(make_text_run(candidate, in_strong, in_em));
                }
            }
        } else {
            // Not path-shaped: never a link attempt in the first place
            // (e.g. a `[!NOTE]` callout lead). Preserve exactly as written.
            outcome = ShortcutOutcome::Verbatim;
            out.push(make_text_run(&raw, in_strong, in_em));
        }
    }
    // else: `[]` — nothing for a reader to see; `outcome` stays `Dropped`.

    // Reference-style continuation: `[text][ref]` with no definition.
    // Resolve/render it exactly like any other bracket run, then discard
    // the result — `ref` is an address, not content, the same way a
    // resolved `[text](url)` link's `url` is never shown either.
    //
    // The recursive call passes `Canonical` and is allowed to: the `matches!`
    // guard on this branch *is* the canonical-opener test, so the shape is
    // known by construction rather than assumed.
    if idx < n && matches!(&events[idx], Event::Text(t) if t.as_ref() == "[") {
        let (consumed2, _dropped, _outcome2) = consume_bracket_run(
            events,
            idx,
            DelimiterShape::Canonical,
            doc_link_table,
            in_strong,
            in_em,
        );
        idx += consumed2.max(1);
    }

    if let Some(rest) = trailing
        && !rest.is_empty()
    {
        out.push(make_text_run(&rest, in_strong, in_em));
    }

    (idx - start, out, outcome)
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
