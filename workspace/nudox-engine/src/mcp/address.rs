//! The symbol ADDRESS scheme (docs/MCP-SURFACE-PLAN.md §4): a readable,
//! typeable, resolvable alternative to a bare `ecosystem:name#introhex` key.
//!
//! # Why this exists
//!
//! `ecosystem:name#<64 lowercase hex>` costs ~54 tokens (measured with both
//! `cl100k_base` and `o200k_base`) and is neither constructible from what an
//! agent already knows nor verifiable by a human reading the transcript.
//! `cargo:serde@1.0.196::serde::de::Deserializer::deserialize_map[method]`
//! costs ~20 and is both. This module adds that readable form as an
//! *accepted input* and an *emitted field* — it never removes or weakens the
//! hash (§4.14's closure property: the worst case of the new scheme is
//! exactly today's behaviour).
//!
//! # Identity never moves
//!
//! [`IntroId`] remains identity, forever. Everything in this module is
//! *resolution* and *display* (§4.8's verdict table): a way to turn readable
//! text into the same `IntroId` a search result would have given you, and a
//! way to render an `IntroId` back into that readable text. Nothing here
//! changes what a key means or how it is minted.
//!
//! # Grammar
//!
//! ```text
//! address    ::= pkg-coord [ "::" sym-path ] [ "#" key ]
//! pkg-coord  ::= ecosystem ":" pkg-name [ "@" version ]
//! sym-path   ::= segment { sep segment }
//! sep        ::= "::" | "." | "/"          ; all normalize to a segment boundary
//! segment    ::= name [ qual-block ] | qual-block   ; bare qual-block = anonymous (impl blocks)
//! name       ::= ident | "'" quoted "'"
//! qual-block ::= "[" qual { "," qual } "]"          ; split at nesting depth 0 only
//! qual       ::= kind | "~" digit+
//! key        ::= 64 lowercase hex   (prefixes are a later stage)
//! ```
//!
//! `[]`, `()`, `<>` nest and must balance — that is what lets
//! `readValue[method(byte[],Class<?>)]` parse (the whole parenthesized text
//! becomes one opaque `kind` qualifier; this module does not attempt to
//! structurally interpret it — see "What this module deliberately does not
//! do" below). `::` is the package/symbol boundary. The version separator is
//! the **last** `@` in `pkg-coord`, so `npm:@types/node@20.11.0` parses.
//! purl-style ecosystem tags are normalized on input (`golang`→`go`,
//! `clang`→`cpp`) but never emitted.
//!
//! **The legacy form is a strict subset:** `cargo:serde#3f1a…` parses as
//! `pkg-coord` + empty `sym-path` + `key` — every key in every existing
//! result and test is already a valid address, so there is no cutover.
//!
//! # The four-stage resolver (§4.10)
//!
//! | Stage | Mechanism | Cost |
//! |---|---|---|
//! | 1 · physical exact | brute-force `(kind × root-elision)` blake3 over the typed path | no index |
//! | 2 · alias index | exact lookup in the inverted `Symbol.aliases` map | one `HashMap` probe |
//! | 3 · name-plan + suffix filter | plan on the leaf *name*, filter by path suffix | never a `FullScan` |
//!
//! Stages run in that order and stop at the first one that yields exactly
//! one candidate (or one after the collapse heuristics in
//! [`AutoResolveHeuristic`] apply) — Stage 1 is pure hashing with no index at
//! all, so trying it first is free relative to the two index probes below
//! it.
//!
//! # What this module deliberately does not do
//!
//! * **It never reconstructs `Disambiguator::FnOverload`/`TraitImpl` bytes.**
//!   Those are structural skeletons built from a full declaration's AST at
//!   seal time and are not recoverable from a bare path string (§4.7: "the
//!   sealer is the only component that both holds the types needed to render
//!   and holds every key in the package"). Stage 1 can only ever construct
//!   `Disambiguator::None` candidates, so it silently misses any declaration
//!   whose disambiguator escalated past `None` — which, for Rust, is *every*
//!   `impl` block, unconditionally: the sealer applies `TraitImpl` to every
//!   impl regardless of whether its name happens to collide with a sibling.
//!   (Correction to an earlier draft of this doc and of the design doc's
//!   §4.3(d): a Rust `impl` block's `Symbol.name` is **not** the bare literal
//!   `"impl"` — the producer renders a real descriptive name via
//!   `impl_display_name` in `workspace/compiler/languages/src/rust/ra/item.rs`,
//!   e.g. `"impl Deserialize<'de> for Mutex<T>"`. So most impl members *do*
//!   have a distinguishing ancestor segment and Stage 3 usually finds them
//!   uniquely — the disambiguator-tier limitation above still means Stage 1
//!   never finds them by hash, but Stage 3 finding exactly one is the common
//!   case, not `Ambiguous`. Real collisions still happen — most simply, two
//!   separate inherent `impl SelfType { .. }` blocks on the same type render
//!   the identical name `"impl SelfType"` (no trait to distinguish them) —
//!   and Stage 3 correctly reports [`ResolveOutcome::Ambiguous`] for those;
//!   see `impl_block_methods_never_get_a_silent_pick` in this crate's corpus
//!   tests, which discovers a real collision empirically rather than
//!   assuming impls are collision-prone by default.
//! * **It does not implement heuristic (a)** ("all candidates collapse to
//!   one terminal definition via re-export") from the design doc's list of
//!   permitted silent resolutions — not because it is hard (a cycle-guarded,
//!   depth-16-capped walk of each candidate's `EntryInner::Reference` chain
//!   via `Ref::Intro` hops is a dozen lines, and was built and measured), but
//!   because it was measured against 5 real packages — memchr-2.8.3,
//!   serde-1.0.196, syn-1.0.109, regex-1.10.3, indexmap-2.13.0, 31,163
//!   lowered entries total — and found to fire on **none** of them:
//!   `reexport_convergence_and_chain_depth_census`
//!   (`tests/mcp/address_resolution.rs`) found zero name-groups matching the
//!   convergent shape and a maximum real `Reference` chain depth of 1 hop
//!   (never `Reference -> Reference -> ... -> Owned`, so never a cycle
//!   either). A candidate set that is all `Reference` rows with no owned
//!   definition among them therefore still falls through to
//!   [`ResolveOutcome::Ambiguous`] — conservative and honest, per "never
//!   auto-resolve real ambiguity" (§4's non-negotiable), and in this corpus
//!   also simply correct: there was nothing real to collapse. The
//!   implementation is preserved in git history (see that test's doc
//!   comment) for the day a corpus package or another ecosystem's re-export
//!   patterns actually exhibit the shape. Heuristics (b) and (c) *are*
//!   implemented in full; see [`AutoResolveHeuristic`].
//!
//! # Option A (decided)
//!
//! Re-exports become resolvable by attaching the public path to the
//! *definition* via the inverted alias index (Stage 2) — never by making a
//! `Reference` row itself searchable. `collect_name_hits` and
//! `memchr_reexport_aliases_are_invisible_to_name_search` are untouched by
//! this module.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::Arc;

use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::intro::{Disambiguator, bootstrap_intro_id};
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::reflect::{PathStyle, moniker_path_styled, moniker_segments};

use crate::store::corpus::Corpus;
use crate::store::package::PackageView;
use crate::versions::VersionRegistry;
use crate::wire::SymbolKey;

// ---------------------------------------------------------------------------
// Address AST
// ---------------------------------------------------------------------------

/// One qualifier inside a segment's `[...]` block.
///
/// This is deliberately narrower than the disambiguator vocabulary
/// `docs/MCP-SURFACE-PLAN.md` §4's full grammar sketches
/// (`FnOverload`/`TraitImpl`/`at file:line`): those require sealer-rendered
/// text that does not exist yet (§4.11 items 3–4, "the real project"). What
/// *is* buildable today is a `kind` tag (always known — it is the
/// `KindDiscriminant` every declaration already carries) and a positional
/// `~n` marker for the small "unstable, do not cache across versions" class
/// (§4's `Span`/`Ordinal`-tier declarations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Qualifier {
    /// A kind tag (`"trait"`, `"method"`, …) or, for text this parser does
    /// not further interpret (e.g. a parenthesized overload skeleton copied
    /// from docs), whatever opaque text appeared between the brackets.
    Kind(String),
    /// `~n`: a positional, generation-scoped marker. Never resolved against
    /// stored disambiguator bytes (there are none to compare against — see
    /// the module docs); carried through parsing and rendering for fidelity
    /// only.
    Positional(u32),
}

/// One segment of a symbol path.
///
/// `name` is `None` for an *anonymous* segment — the grammar's sugar for
/// "I don't know or don't want to type this ancestor's exact name". A real
/// Rust `impl` block's `Symbol.name` is **not** a content-free placeholder —
/// the producer renders a real descriptive name (`"impl Deserialize<'de> for
/// Mutex<T>"`, `impl_display_name` in the Rust producer) — so
/// [`render_address`] always emits it as a quoted segment name, never as an
/// anonymous one; the anonymous form exists purely for a caller composing an
/// address from partial knowledge, and resolves only as far as Stage 3's
/// name+suffix search can get with the literal fallback name below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressSegment {
    /// The segment's name, or `None` for an anonymous segment.
    pub name: Option<String>,
    /// Qualifiers attached to this segment, in the order they were parsed.
    pub qualifiers: Vec<Qualifier>,
}

impl AddressSegment {
    /// The name to use when reconstructing a hash preimage or an index
    /// lookup key: the segment's own name, or the literal string `"impl"`
    /// for an anonymous segment.
    ///
    /// That literal is a legacy placeholder, not a real ancestor name — real
    /// impl blocks are named descriptively (see the struct docs), so an
    /// anonymous segment will not match a real impl ancestor's physical or
    /// public path text. It is kept as the fallback anyway because the
    /// grammar (§4 of docs/MCP-SURFACE-PLAN.md) defines an anonymous segment
    /// as legal syntax independent of what any particular resolver stage can
    /// do with it, and *some* effective name must exist for hashing/suffix
    /// comparison to proceed rather than panic.
    pub fn effective_name(&self) -> &str {
        self.name.as_deref().unwrap_or("impl")
    }

    /// The first `[kind]` qualifier's text, if this segment carries one.
    pub fn kind_qualifier(&self) -> Option<&str> {
        self.qualifiers.iter().find_map(|q| match q {
            Qualifier::Kind(k) => Some(k.as_str()),
            Qualifier::Positional(_) => None,
        })
    }
}

/// The `ecosystem:pkg-name[@version]` half of an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCoord {
    /// Normalized ecosystem tag (`golang`/`clang` already folded to
    /// `go`/`cpp` — see the module docs).
    pub ecosystem: String,
    /// The package name, exactly as written (may itself contain `:` for
    /// Maven's `group:artifact`, or `/` for npm scopes and Go module paths).
    pub name: String,
    /// The version, if the address named one. `None` means "whatever
    /// generation is currently resident" — see [`ResolveOutcome::VersionMismatch`].
    pub version: Option<String>,
}

/// A fully parsed address: `pkg-coord [ "::" sym-path ] [ "#" key ]`.
///
/// Construct via [`Address::parse`]. `path` is empty for the legacy
/// `eco:name#hex` form — that is the grammar's documented degenerate case,
/// not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// The package coordinate.
    pub package: PackageCoord,
    /// The symbol path, root-first. Empty for the legacy hash-only form.
    pub path: Vec<AddressSegment>,
    /// The `#key` half, already decoded. Present or absent independently of
    /// `path` — an address may carry either, both, or (rejected at parse
    /// time) neither.
    pub key: Option<IntroId>,
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.package.ecosystem, self.package.name)?;
        if let Some(v) = &self.package.version {
            write!(f, "@{v}")?;
        }
        if !self.path.is_empty() {
            f.write_str("::")?;
            let style = PathStyle::for_ecosystem(&self.package.ecosystem);
            for (i, seg) in self.path.iter().enumerate() {
                if i > 0 {
                    f.write_str(style.separator())?;
                }
                render_segment(seg, f)?;
            }
        }
        if let Some(key) = &self.key {
            write!(f, "#{}", key.to_hex())?;
        }
        Ok(())
    }
}

fn render_segment(seg: &AddressSegment, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &seg.name {
        Some(n) if is_bare_ident(n) => f.write_str(n)?,
        // Escape `\` and `'` inside a quoted segment. Rust impl names carry
        // lifetimes — `impl Deserialize<'de> for Mutex<T>` — spelled with the
        // same apostrophe that delimits the quote, so emitting the name raw
        // produced an address this crate's own parser rejected, making every
        // impl-member address unusable as input.
        Some(n) => {
            f.write_str("'")?;
            for ch in n.chars() {
                if ch == '\\' || ch == '\'' {
                    f.write_str("\\")?;
                }
                f.write_char(ch)?;
            }
            f.write_str("'")?;
        }
        None => {}
    }
    if !seg.qualifiers.is_empty() {
        f.write_str("[")?;
        for (i, q) in seg.qualifiers.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            match q {
                Qualifier::Kind(k) => f.write_str(k)?,
                Qualifier::Positional(n) => write!(f, "~{n}")?,
            }
        }
        f.write_str("]")?;
    }
    Ok(())
}

/// True if `s` can be rendered bare (unquoted) as an address segment name.
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A malformed address, with the byte offset (into the original input) an
/// agent should look at to fix it — never a panic, per the grammar's own
/// promise that any string either parses or explains exactly where it broke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("address parse error at byte {offset}: {message}")]
pub struct AddressParseError {
    /// Byte offset into the original input string.
    pub offset: usize,
    /// What went wrong, phrased so an agent can self-correct.
    pub message: String,
}

fn perr(offset: usize, message: impl Into<String>) -> AddressParseError {
    AddressParseError {
        offset,
        message: message.into(),
    }
}

fn matching_open(close: char) -> char {
    match close {
        ']' => '[',
        ')' => '(',
        '>' => '<',
        other => other,
    }
}

impl Address {
    /// Parse `input` per the grammar in the module docs.
    ///
    /// Never panics: every rejection is an [`AddressParseError`] carrying the
    /// byte offset of the problem.
    pub fn parse(input: &str) -> Result<Address, AddressParseError> {
        parse_address(input)
    }
}

fn normalize_ecosystem(raw: &str) -> String {
    match raw {
        "golang" => "go".to_owned(),
        "clang" => "cpp".to_owned(),
        other => other.to_owned(),
    }
}

fn parse_address(input: &str) -> Result<Address, AddressParseError> {
    // -- ecosystem ------------------------------------------------------
    let Some(colon) = input.find(':') else {
        return Err(perr(input.len(), "expected ':' after the ecosystem tag"));
    };
    let ecosystem_raw = &input[..colon];
    if ecosystem_raw.is_empty() {
        return Err(perr(0, "ecosystem must not be empty"));
    }
    let ecosystem = normalize_ecosystem(ecosystem_raw);
    let rest = &input[colon + 1..];
    let rest_base = colon + 1;

    // -- bracket/quote-aware scan for the two top-level boundaries ------
    //
    // `sep_pos` is the first depth-0, unquoted "::" — the pkg-coord/sym-path
    // boundary. `hash_pos` is the first depth-0, unquoted '#' *after*
    // `sep_pos` (or anywhere, if there is no `sep_pos`) — the key boundary.
    let mut depth_stack: Vec<(char, usize)> = Vec::new();
    let mut in_quote = false;
    let mut sep_pos: Option<usize> = None;
    let mut hash_pos: Option<usize> = None;

    let bytes = rest.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if in_quote {
            // Same escape rule as `split_top_level` and `parse_segment`: a
            // `\'` inside a quoted name is content, not the closing delimiter.
            // Rust lifetimes make this the common case for impl segments.
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == '\'' {
                in_quote = false;
            }
            i += 1;
            continue;
        }
        match ch {
            '\'' => in_quote = true,
            '[' => depth_stack.push((']', i)),
            '(' => depth_stack.push((')', i)),
            '<' => depth_stack.push(('>', i)),
            ']' | ')' | '>' => match depth_stack.pop() {
                Some((expected, _)) if expected == ch => {}
                Some((expected, open_at)) => {
                    return Err(perr(
                        rest_base + i,
                        format!(
                            "mismatched bracket: '{ch}' does not close the '{}' opened at byte {}",
                            matching_open(expected),
                            rest_base + open_at
                        ),
                    ));
                }
                None => {
                    return Err(perr(
                        rest_base + i,
                        format!("unexpected closing '{ch}' with no matching open"),
                    ));
                }
            },
            ':' if depth_stack.is_empty()
                && sep_pos.is_none()
                && bytes.get(i + 1) == Some(&b':') =>
            {
                sep_pos = Some(i);
                i += 1; // consume the second ':' too
            }
            '#' if depth_stack.is_empty()
                && hash_pos.is_none()
                && sep_pos.is_none_or(|s| i > s) =>
            {
                hash_pos = Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    if in_quote {
        return Err(perr(
            rest_base + rest.len(),
            "unterminated quoted segment: missing closing \"'\"",
        ));
    }
    if let Some((expected, open_at)) = depth_stack.last() {
        return Err(perr(
            rest_base + open_at,
            format!(
                "unbalanced '{}': no matching '{}'",
                matching_open(*expected),
                expected
            ),
        ));
    }

    let coord_end = match (sep_pos, hash_pos) {
        (Some(s), _) => s,
        (None, Some(h)) => h,
        (None, None) => rest.len(),
    };
    let pkg_coord_str = &rest[..coord_end];

    // -- pkg-name / version: version separator is the LAST '@' -----------
    let (pkg_name, version) = match pkg_coord_str.rfind('@') {
        Some(at) => {
            let version = &pkg_coord_str[at + 1..];
            if version.is_empty() {
                return Err(perr(
                    rest_base + coord_end,
                    "version must not be empty after '@'",
                ));
            }
            (&pkg_coord_str[..at], Some(version.to_owned()))
        }
        None => (pkg_coord_str, None),
    };
    if pkg_name.is_empty() {
        return Err(perr(rest_base, "package name must not be empty"));
    }

    // -- sym-path ---------------------------------------------------------
    let path = match sep_pos {
        Some(s) => {
            let path_start = s + 2;
            let path_end = hash_pos.unwrap_or(rest.len());
            parse_sym_path(&rest[path_start..path_end], rest_base + path_start)?
        }
        None => Vec::new(),
    };

    // -- key ----------------------------------------------------------------
    let key = match hash_pos {
        Some(h) => {
            let hex = &rest[h + 1..];
            let intro = super::key::parse_intro_hex(hex).ok_or_else(|| {
                perr(
                    rest_base + h + 1,
                    format!(
                        "key must be exactly 64 hex characters, got {} character(s)",
                        hex.chars().count()
                    ),
                )
            })?;
            Some(intro)
        }
        None => None,
    };

    Ok(Address {
        package: PackageCoord {
            ecosystem,
            name: pkg_name.to_owned(),
            version,
        },
        path,
        key,
    })
}

/// Split `s` at every top-level (bracket-depth 0, outside a `'...'` quote)
/// position where `is_sep` matches, returning the byte ranges between
/// separators. Also validates that every `[`, `(`, `<` in `s` is matched.
///
/// `is_sep(remaining) -> Option<usize>` inspects the slice starting at the
/// current position and returns the separator's byte length if one starts
/// there, so both single-byte separators (`.`, `/`, `,`) and multi-byte ones
/// (`::`) share one scanner.
fn split_top_level(
    s: &str,
    base_offset: usize,
    is_sep: impl Fn(&str) -> Option<usize>,
) -> Result<Vec<(usize, usize)>, AddressParseError> {
    let mut depth_stack: Vec<(char, usize)> = Vec::new();
    let mut in_quote = false;
    let mut ranges = Vec::new();
    let mut seg_start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if in_quote {
            // Honour `\'` and `\\` exactly as `parse_segment` does. Without
            // this, a Rust lifetime inside a quoted impl name (`'impl Foo<\'de>
            // for Bar'`) closes the quote at the escaped apostrophe, and the
            // trailing `>` then reads as an unmatched bracket — so the renderer
            // emits an address its own parser rejects.
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == '\'' {
                in_quote = false;
            }
            i += 1;
            continue;
        }
        if ch == '\'' {
            in_quote = true;
            i += 1;
            continue;
        }
        if depth_stack.is_empty()
            && let Some(len) = is_sep(&s[i..]) {
                ranges.push((seg_start, i));
                i += len;
                seg_start = i;
                continue;
            }
        match ch {
            '[' => depth_stack.push((']', i)),
            '(' => depth_stack.push((')', i)),
            '<' => depth_stack.push(('>', i)),
            ']' | ')' | '>' => match depth_stack.pop() {
                Some((expected, _)) if expected == ch => {}
                Some((expected, open_at)) => {
                    return Err(perr(
                        base_offset + i,
                        format!(
                            "mismatched bracket: '{ch}' does not close the '{}' opened at byte {}",
                            matching_open(expected),
                            base_offset + open_at
                        ),
                    ));
                }
                None => {
                    return Err(perr(
                        base_offset + i,
                        format!("unexpected closing '{ch}' with no matching open"),
                    ));
                }
            },
            _ => {}
        }
        i += 1;
    }
    if in_quote {
        return Err(perr(
            base_offset + s.len(),
            "unterminated quoted segment: missing closing \"'\"",
        ));
    }
    if let Some((expected, open_at)) = depth_stack.last() {
        return Err(perr(
            base_offset + open_at,
            format!(
                "unbalanced '{}': no matching '{}'",
                matching_open(*expected),
                expected
            ),
        ));
    }
    ranges.push((seg_start, s.len()));
    Ok(ranges)
}

fn sym_path_sep(remaining: &str) -> Option<usize> {
    if remaining.starts_with("::") {
        Some(2)
    } else if remaining.starts_with('.') || remaining.starts_with('/') {
        Some(1)
    } else {
        None
    }
}

fn parse_sym_path(s: &str, base_offset: usize) -> Result<Vec<AddressSegment>, AddressParseError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let ranges = split_top_level(s, base_offset, sym_path_sep)?;
    ranges
        .into_iter()
        .map(|(start, end)| parse_segment(&s[start..end], base_offset + start))
        .collect()
}

fn parse_segment(s: &str, base_offset: usize) -> Result<AddressSegment, AddressParseError> {
    // Blank, not merely empty. A whitespace-only segment used to parse into a
    // segment literally named `" "`, which is far worse than a rejection: the
    // space is hashed into the intro preimage verbatim, matches nothing, and
    // the agent is told the symbol does not exist rather than that its address
    // was malformed. Quoted segments (`'a b'`) and anonymous impl blocks
    // (`[impl Display for Vec<T>]`) legitimately contain spaces and are
    // unaffected — neither trims to empty.
    if s.trim().is_empty() {
        return Err(perr(
            base_offset,
            "empty path segment (check for a doubled or trailing separator)",
        ));
    }
    let bytes = s.as_bytes();
    let (name, rest_start): (Option<String>, usize) = if bytes[0] == b'\'' {
        // Honour `\'` and `\\` escapes. A bare "find the next apostrophe" scan
        // closes the quote inside a Rust lifetime — `'impl Foo<'de> for Bar'`
        // would yield the name `impl Foo<` and reinterpret the rest as path
        // structure, which parses and names something else entirely.
        let mut unescaped = String::new();
        let mut chars = s.char_indices().skip(1);
        let mut close = None;
        while let Some((i, ch)) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some((_, escaped)) => unescaped.push(escaped),
                    None => {
                        return Err(perr(
                            base_offset + i,
                            "trailing escape in quoted segment name",
                        ));
                    }
                },
                '\'' => {
                    close = Some(i);
                    break;
                }
                other => unescaped.push(other),
            }
        }
        let Some(close) = close else {
            return Err(perr(
                base_offset,
                "unterminated quoted segment name: missing closing \"'\"",
            ));
        };
        (Some(unescaped), close + 1)
    } else if bytes[0] == b'[' {
        (None, 0)
    } else {
        let ident_end = s.find('[').unwrap_or(s.len());
        (Some(s[..ident_end].to_owned()), ident_end)
    };

    let qualifiers = if rest_start < s.len() {
        let tail = &s[rest_start..];
        if !tail.starts_with('[') || !tail.ends_with(']') {
            return Err(perr(
                base_offset + rest_start,
                "expected a '[...]' qualifier block after the segment name",
            ));
        }
        let inner = &tail[1..tail.len() - 1];
        parse_qual_block(inner, base_offset + rest_start + 1)?
    } else {
        Vec::new()
    };

    Ok(AddressSegment { name, qualifiers })
}

fn parse_qual_block(inner: &str, base_offset: usize) -> Result<Vec<Qualifier>, AddressParseError> {
    if inner.is_empty() {
        return Err(perr(
            base_offset,
            "qualifier block must not be empty (remove the brackets if there is nothing to qualify)",
        ));
    }
    let ranges = split_top_level(inner, base_offset, |remaining| {
        remaining.starts_with(',').then_some(1)
    })?;
    ranges
        .into_iter()
        .map(|(start, end)| parse_qual(&inner[start..end], base_offset + start))
        .collect()
}

fn parse_qual(text: &str, offset: usize) -> Result<Qualifier, AddressParseError> {
    let trimmed = text.trim();
    if let Some(digits) = trimmed.strip_prefix('~') {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(perr(
                offset,
                "'~' positional qualifier must be followed by digits, e.g. '~1'",
            ));
        }
        let n: u32 = digits
            .parse()
            .map_err(|_| perr(offset, "positional qualifier number is too large"))?;
        return Ok(Qualifier::Positional(n));
    }
    if trimmed.is_empty() {
        return Err(perr(offset, "empty qualifier (check for a stray ',')"));
    }
    Ok(Qualifier::Kind(trimmed.to_owned()))
}

// ---------------------------------------------------------------------------
// Kind <-> text
// ---------------------------------------------------------------------------

/// Every registered [`KindDiscriminant`], in declaration order.
///
/// `KindDiscriminant` has no generated `ALL`/iterator (see
/// `nudox_ir::kind`'s `register_kinds!` macro) — `graph::plan`'s
/// `kind_disc_from_str` hand-lists the same 13 for the same reason.
const ALL_KINDS: [KindDiscriminant; 13] = [
    KindDiscriminant::Module,
    KindDiscriminant::Record,
    KindDiscriminant::Field,
    KindDiscriminant::Function,
    KindDiscriminant::Alias,
    KindDiscriminant::Trait,
    KindDiscriminant::Impl,
    KindDiscriminant::Enum,
    KindDiscriminant::Variant,
    KindDiscriminant::Const,
    KindDiscriminant::Static,
    KindDiscriminant::Reexport,
    KindDiscriminant::Param,
];

/// The canonical rendered spelling for a kind — what [`render_address`]
/// emits and what [`kind_from_text`] round-trips.
fn kind_to_text(disc: KindDiscriminant) -> &'static str {
    match disc {
        KindDiscriminant::Module => "module",
        KindDiscriminant::Record => "record",
        KindDiscriminant::Field => "field",
        KindDiscriminant::Function => "function",
        KindDiscriminant::Alias => "alias",
        KindDiscriminant::Trait => "trait",
        KindDiscriminant::Impl => "impl",
        KindDiscriminant::Enum => "enum",
        KindDiscriminant::Variant => "variant",
        KindDiscriminant::Const => "const",
        KindDiscriminant::Static => "static",
        KindDiscriminant::Reexport => "reexport",
        KindDiscriminant::Param => "param",
    }
}

/// Parse a `[kind]` qualifier's text back into a `KindDiscriminant`.
///
/// Accepts the canonical spelling plus a handful of common synonyms an agent
/// composing an address by hand (rather than copying a rendered one) is
/// likely to type — `"method"` for `Function`, `"struct"`/`"class"` for
/// `Record`, and so on. Round-trip fidelity does not depend on these:
/// [`render_address`] only ever emits the canonical spelling.
fn kind_from_text(s: &str) -> Option<KindDiscriminant> {
    match s.to_ascii_lowercase().as_str() {
        "module" => Some(KindDiscriminant::Module),
        "record" | "struct" | "class" => Some(KindDiscriminant::Record),
        "field" | "property" => Some(KindDiscriminant::Field),
        "function" | "method" | "fn" => Some(KindDiscriminant::Function),
        "alias" | "type" | "typedef" => Some(KindDiscriminant::Alias),
        "trait" | "interface" | "protocol" => Some(KindDiscriminant::Trait),
        "impl" => Some(KindDiscriminant::Impl),
        "enum" => Some(KindDiscriminant::Enum),
        "variant" => Some(KindDiscriminant::Variant),
        "const" | "constant" => Some(KindDiscriminant::Const),
        "static" => Some(KindDiscriminant::Static),
        "reexport" | "re-export" => Some(KindDiscriminant::Reexport),
        "param" | "parameter" => Some(KindDiscriminant::Param),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rendering an IntroId back into an Address
// ---------------------------------------------------------------------------

/// Render the address for `intro` in `pkg`, under `lineage`/`version`.
///
/// # The `#key` half is included only when the path alone is not enough
///
/// §4.12's token accounting is explicit that emitting the address *alongside*
/// the full hash is "a wash" — 45 tokens against today's 48, not the ~20 the
/// scheme is sold on. The win requires the hash to actually drop off most
/// rows, which requires knowing, per row, whether the `sym-path` alone
/// already resolves uniquely — and this function is in the one position that
/// can decide that *exactly* rather than estimate it: [`resolve_in_package`]
/// with `key: None` is the real resolver, and Stage 1 is pure hashing (no
/// index), so calling it here costs microseconds, not a guess.
///
/// So: the `sym-path` half is built whenever `intro` has a physical path
/// (declarations with none — §4.8's rare null-`path` case — degrade straight
/// to the legacy `eco:name#hex` form). The `#key` half is appended only if
/// resolving that bare `sym-path` (no key) does *not* come back
/// [`ResolveOutcome::Resolved`] naming this same `intro` — i.e. only when the
/// path alone is ambiguous, not found, or (should not happen for a path this
/// function just derived from the real table, but checked rather than
/// assumed) resolves to something else. "Never remove or weaken the hash"
/// (the scheme's non-negotiable) is preserved because the hash is never
/// *omitted* when it is needed to disambiguate — only when it is provably
/// redundant with what the path already pins down uniquely.
pub fn render_address(
    pkg: &PackageView,
    lineage: &PackageLineageId,
    version: Option<&str>,
    intro: IntroId,
) -> Option<Address> {
    let entry = pkg.view().entry(intro)?;
    let names = moniker_segments(pkg.view().table(), intro);

    let mut full_path: Vec<AddressSegment> = names
        .map(|segs| {
            segs.into_iter()
                .map(|name| AddressSegment {
                    name: Some(name),
                    qualifiers: Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(disc) = entry.kind().discriminant()
        && let Some(last) = full_path.last_mut() {
            last.qualifiers
                .push(Qualifier::Kind(kind_to_text(disc).to_owned()));
        }

    // Elide leading path segments that merely repeat the package name
    // (§9.2's "still repeats the crate root" unclaimed saving). memchr's
    // real physical path is `memchr::memchr::memchr::Memchr` — the crate's
    // own root module name, genuinely nested three deep before the struct —
    // and rendering all three is correct but wasteful on every row of every
    // search response.
    //
    // This is done by *verification*, not by a blanket "strip segments equal
    // to the package name" rule: that blanket rule would silently mis-address
    // a declaration that happens to live in a real, distinct module which is
    // merely same-named as the package (e.g. package `foo` with a genuine
    // submodule literally named `foo`, one level below the elided root).
    // `resolve_in_package` is the real resolver — Stage 1 is pure hashing
    // with no index, and it already tries root-elision variants when
    // resolving typed input, so an elided address here resolves back through
    // the exact same path a caller who typed the short form would take.
    // Trying the most-elided candidate first and stopping at the first
    // verified hit yields the shortest address that still resolves to this
    // declaration; the full path (today's behaviour) is always the fallback.
    let pkg_name = lineage.name.as_str();
    let max_elide = full_path
        .iter()
        .take_while(|seg| seg.name.as_deref() == Some(pkg_name))
        .count()
        .min(full_path.len().saturating_sub(1));

    let mut path = full_path.clone();
    let mut path_alone_resolves_here = false;
    for elide in (0..=max_elide).rev() {
        let candidate = &full_path[elide..];
        if matches!(
            resolve_in_package(pkg, lineage, candidate, None),
            ResolveOutcome::Resolved { key, .. } if key.intro == intro
        ) {
            path = full_path.split_off(elide);
            path_alone_resolves_here = true;
            break;
        }
    }
    let key = if path_alone_resolves_here {
        None
    } else {
        Some(intro)
    };

    Some(Address {
        package: PackageCoord {
            ecosystem: lineage.ecosystem.as_str().to_owned(),
            name: lineage.name.as_str().to_owned(),
            version: version.map(str::to_owned),
        },
        path,
        key,
    })
}

// ---------------------------------------------------------------------------
// Resolution outcomes
// ---------------------------------------------------------------------------

/// Which of the permitted silent-resolution heuristics fired to collapse an
/// otherwise-ambiguous candidate set to one.
///
/// §4's non-negotiable: "Never auto-resolve real ambiguity." These three are
/// the *only* permitted silent resolutions, and every use of one is recorded
/// here — a caller can always see why a choice was made and how many
/// alternatives were suppressed (`ResolveOutcome::Resolved::suppressed_alternatives`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoResolveHeuristic {
    /// Exactly one candidate is an owned definition; every other candidate
    /// is a re-export `Reference` row with no kind of its own. The
    /// definition wins.
    DefinitionPreferredOverReexport,
    /// Exactly one candidate's physical or public path matched the typed
    /// path *in full*; every other candidate matched only as a trailing
    /// suffix of a longer path. The full match wins.
    ExactPathPreferredOverSuffix,
}

/// One candidate in an [`ResolveOutcome::Ambiguous`] report.
///
/// Carries enough for an agent to write a disambiguating follow-up address
/// (add the `kind`, or copy `physical_path`/`public_path` verbatim) without
/// another search round trip.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// This candidate's identity.
    pub intro: IntroId,
    /// The physical (hash-preimage) path, if resolvable.
    pub physical_path: Option<String>,
    /// The shortest public (re-export) path, if one is shorter than
    /// `physical_path`. `None` means "same as physical", not "unknown".
    pub public_path: Option<String>,
    /// This candidate's kind, rendered the same way [`render_address`] would.
    /// `None` for a `Reference` (re-export) row, which has no kind of its own.
    pub kind: Option<String>,
}

/// The outcome of resolving an [`Address`] — never a bare `Option`.
///
/// §4's top-severity finding about the *existing* surface was "empty means
/// nonexistent": a caller cannot tell "this package was never indexed" from
/// "this symbol does not exist" from "you asked for a version we don't have"
/// from a `None`. Every one of those is a distinct, actionable situation
/// here.
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// Resolved to exactly one declaration.
    Resolved {
        /// The resolved key.
        key: SymbolKey,
        /// Which heuristic collapsed a multi-candidate set to this one, if
        /// any — `None` means a single stage returned exactly one hit with
        /// no collapsing needed.
        heuristic: Option<AutoResolveHeuristic>,
        /// How many alternative candidates were suppressed by `heuristic`.
        /// `0` when `heuristic` is `None`.
        suppressed_alternatives: usize,
    },
    /// More than one declaration matched and none of the permitted silent
    /// resolutions applied. The caller must refine, never guess.
    Ambiguous {
        /// Every surviving candidate.
        candidates: Vec<Candidate>,
        /// A short, actionable suggestion for how to disambiguate.
        refine_hint: String,
    },
    /// The package is indexed and the address parsed, but nothing matched.
    NotFound {
        /// Names near the leaf segment, for a "did you mean" nudge.
        near_misses: Vec<String>,
    },
    /// The requested package lineage is not currently loaded at all.
    PackageNotIndexed {
        /// Loaded package names that resemble the requested one.
        resident_similar: Vec<String>,
    },
    /// The address named a version this package does not have loaded.
    VersionMismatch {
        /// The version the address requested.
        requested: String,
        /// Versions actually resident for this lineage.
        resident: Vec<String>,
    },
    /// The address carried a `#key` that decoded but does not name a live
    /// declaration in the resolved package/version.
    StaleKey,
    /// The address carried both a `sym-path` and a `#key`, and they disagree
    /// about which declaration is meant. Neither is silently preferred.
    AddressConflict {
        /// What disagreed and how.
        message: String,
    },
    /// The address text itself did not parse.
    ParseError {
        /// Byte offset into the original input.
        offset: usize,
        /// What went wrong.
        message: String,
    },
}

impl From<AddressParseError> for ResolveOutcome {
    fn from(e: AddressParseError) -> Self {
        ResolveOutcome::ParseError {
            offset: e.offset,
            message: e.message,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal candidate plumbing
// ---------------------------------------------------------------------------

/// One hit from a resolver stage, before ambiguity collapsing.
#[derive(Debug, Clone, Copy)]
struct RawCandidate {
    intro: IntroId,
    /// True when the typed path matched this candidate's *full* physical or
    /// public path (not merely a trailing suffix of a longer one). Stage 1
    /// and Stage 2 hits are always `true` — they matched by direct
    /// reconstruction, not by suffix search. Only Stage 3 computes this
    /// per-candidate (see [`AutoResolveHeuristic::ExactPathPreferredOverSuffix`]).
    exact: bool,
}

/// The outcome of resolving just the `sym-path` half, before Stage 0's key
/// cross-check is applied (see [`resolve_in_package`]).
enum PathResolution {
    Found {
        intro: IntroId,
        heuristic: Option<AutoResolveHeuristic>,
        suppressed: usize,
    },
    Ambiguous {
        candidates: Vec<Candidate>,
    },
    NotFound {
        near_misses: Vec<String>,
    },
}

fn is_suffix(haystack: &[String], needle: &[&str]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    let start = haystack.len() - needle.len();
    haystack[start..]
        .iter()
        .map(String::as_str)
        .eq(needle.iter().copied())
}

// -- Stage 1: physical exact, pure hashing, no index -------------------------

fn stage1_physical_exact(
    pkg: &PackageView,
    lineage: &PackageLineageId,
    path: &[AddressSegment],
) -> Vec<RawCandidate> {
    let Some(leaf) = path.last() else {
        return Vec::new();
    };
    let leaf_name = leaf.effective_name();
    let ancestor_names: Vec<&str> = path[..path.len() - 1]
        .iter()
        .map(AddressSegment::effective_name)
        .collect();
    // `Reexport` is excluded even from an explicit `[reexport]` qualifier:
    // `package/seal.rs` mints a `Reference` row's hash using
    // `KindDiscriminant::Reexport` as the kind component, so a bare-name
    // brute-force guess with that kind can find a live hash that belongs to
    // a re-export shim row rather than a physical declaration — and because
    // it is the *only* Stage 1 hit, the single-candidate shortcut in
    // `ambiguous_or_collapsed` would return it directly, before heuristic (b)
    // ever gets a chance to prefer the real definition over it (found via a
    // real memchr `Memchr` query: Stage 1 silently resolved to the crate-root
    // re-export row instead of the struct). `sym-path` means the *physical*
    // path (module docs); a re-export row is never one, so Stage 1 must never
    // target it — Stage 2's alias index and Stage 3's definition-preferred
    // heuristic are what re-export queries are for.
    let kinds: Vec<KindDiscriminant> = match leaf.kind_qualifier().and_then(kind_from_text) {
        Some(KindDiscriminant::Reexport) => return Vec::new(),
        Some(k) => vec![k],
        None => ALL_KINDS
            .iter()
            .copied()
            .filter(|k| *k != KindDiscriminant::Reexport)
            .collect(),
    };
    let pkg_name = lineage.name.as_str();

    // Root-elision variants: the real memchr fixture's physical path is
    // `memchr::memchr::memchr::Memchr` (§4 worked examples) — an agent typing
    // the idiomatic `memchr::Memchr` does not know how many times the
    // crate's own root module name is implicitly repeated ahead of what it
    // typed, so try 0/1/2 leading prepends of the package name.
    let mut variants: Vec<Vec<&str>> = vec![ancestor_names.clone()];
    let mut v1 = vec![pkg_name];
    v1.extend(ancestor_names.iter().copied());
    variants.push(v1);
    let mut v2 = vec![pkg_name, pkg_name];
    v2.extend(ancestor_names.iter().copied());
    variants.push(v2);

    let mut hits = Vec::new();
    for segs in &variants {
        for kind in &kinds {
            let candidate =
                bootstrap_intro_id(lineage, *kind, segs, leaf_name, &Disambiguator::None);
            if pkg.view().is_live(candidate) {
                hits.push(RawCandidate {
                    intro: candidate,
                    exact: true,
                });
            }
        }
    }
    hits
}

// -- Stage 2: alias index -----------------------------------------------------

fn stage2_alias_exact(pkg: &PackageView, path: &[AddressSegment]) -> Vec<RawCandidate> {
    if path.is_empty() {
        return Vec::new();
    }
    let joined = path
        .iter()
        .map(AddressSegment::effective_name)
        .collect::<Vec<_>>()
        .join("::");
    let kind_filter = path
        .last()
        .and_then(|s| s.kind_qualifier())
        .and_then(kind_from_text);
    pkg.indexes()
        .by_alias
        .get_exact(&joined)
        .iter()
        .filter(|&&intro| {
            kind_filter.is_none_or(|k| {
                pkg.view()
                    .entry(intro)
                    .and_then(|e| e.kind().discriminant())
                    == Some(k)
            })
        })
        .map(|&intro| RawCandidate { intro, exact: true })
        .collect()
}

// -- Stage 3: name-plan + path-suffix filter ----------------------------------

fn stage3_name_suffix(pkg: &PackageView, path: &[AddressSegment]) -> Vec<RawCandidate> {
    let Some(leaf) = path.last() else {
        return Vec::new();
    };
    let leaf_lower = leaf.effective_name().to_lowercase();
    let kind_filter = leaf.kind_qualifier().and_then(kind_from_text);
    let query_suffix: Vec<&str> = path.iter().map(AddressSegment::effective_name).collect();
    let query_len = query_suffix.len();

    pkg.indexes()
        .by_name
        .get_exact(&leaf_lower)
        .iter()
        .filter_map(|name_entry| {
            let intro = name_entry.intro;
            let entry = pkg.view().entry(intro)?;
            if let Some(k) = kind_filter
                && entry.kind().discriminant() != Some(k) {
                    return None;
                }
            let physical = moniker_segments(pkg.view().table(), intro);
            let public =
                crate::chunk::head::public_path(intro, entry, pkg.view(), PathStyle::DoubleColon)
                    .map(|s| s.split("::").map(str::to_owned).collect::<Vec<_>>());

            let physical_ok = physical
                .as_deref()
                .is_some_and(|segs| is_suffix(segs, &query_suffix));
            let public_ok = public
                .as_deref()
                .is_some_and(|segs| is_suffix(segs, &query_suffix));
            if !physical_ok && !public_ok {
                return None;
            }
            let exact = physical.as_ref().is_some_and(|s| s.len() == query_len)
                || public.as_ref().is_some_and(|s| s.len() == query_len);
            Some(RawCandidate { intro, exact })
        })
        .collect()
}

// -- Ambiguity collapsing (§4's three permitted heuristics) ------------------

fn build_candidate_row(pkg: &PackageView, style: PathStyle, intro: IntroId) -> Candidate {
    let entry = pkg.view().entry(intro);
    let physical_path = moniker_path_styled(pkg.view().table(), intro, style);
    let public_path =
        entry.and_then(|e| crate::chunk::head::public_path(intro, e, pkg.view(), style));
    let kind = entry
        .and_then(|e| e.kind().discriminant())
        .map(|d| kind_to_text(d).to_owned());
    Candidate {
        intro,
        physical_path,
        public_path,
        kind,
    }
}

fn refine_hint(candidates: &[Candidate]) -> String {
    format!(
        "{} candidates share this path. Add a `[kind]` qualifier (e.g. `[method]`) to the last \
         segment, or resolve via search_symbols and address the exact `#hash` instead.",
        candidates.len()
    )
}

fn ambiguous_or_collapsed(
    pkg: &PackageView,
    lineage: &PackageLineageId,
    candidates: Vec<RawCandidate>,
) -> PathResolution {
    // Merge duplicate intros (possible across root-elision variants / kinds
    // in Stage 1), keeping `exact = true` if any occurrence claimed it.
    let mut merged: BTreeMap<IntroId, bool> = BTreeMap::new();
    for c in candidates {
        let slot = merged.entry(c.intro).or_insert(false);
        *slot = *slot || c.exact;
    }
    let candidates: Vec<RawCandidate> = merged
        .into_iter()
        .map(|(intro, exact)| RawCandidate { intro, exact })
        .collect();

    if candidates.len() == 1 {
        return PathResolution::Found {
            intro: candidates[0].intro,
            heuristic: None,
            suppressed: 0,
        };
    }

    let mut definitions = Vec::new();
    let mut references = Vec::new();
    for c in &candidates {
        let is_definition = pkg
            .view()
            .entry(c.intro)
            .and_then(|e| e.kind().discriminant())
            .is_some();
        if is_definition {
            definitions.push(*c);
        } else {
            references.push(*c);
        }
    }

    // Heuristic (b): exactly one definition, the rest are re-export rows.
    if definitions.len() == 1 && !references.is_empty() {
        return PathResolution::Found {
            intro: definitions[0].intro,
            heuristic: Some(AutoResolveHeuristic::DefinitionPreferredOverReexport),
            suppressed: references.len(),
        };
    }

    // Heuristic (c): exactly one candidate matched the typed path in full,
    // the rest matched only as a suffix.
    let exact_ones: Vec<&RawCandidate> = candidates.iter().filter(|c| c.exact).collect();
    let suffix_ones: Vec<&RawCandidate> = candidates.iter().filter(|c| !c.exact).collect();
    if exact_ones.len() == 1 && !suffix_ones.is_empty() {
        return PathResolution::Found {
            intro: exact_ones[0].intro,
            heuristic: Some(AutoResolveHeuristic::ExactPathPreferredOverSuffix),
            suppressed: suffix_ones.len(),
        };
    }

    // Heuristic (a) ("all candidates collapse to one terminal definition via
    // re-export") was implemented and measured against 5 real packages
    // (memchr, serde, syn, regex, indexmap — 31,163 lowered entries) and
    // found to fire on none of them: `reexport_convergence_and_chain_depth_census`
    // in `tests/mcp/address_resolution.rs` found zero name-groups matching its
    // target shape and a maximum real `Reference` chain depth of 1 hop. Not
    // shipped — see the module docs' "What this module deliberately does not
    // do" and that test's doc comment for the measurement this decision rests
    // on. Reinstate from git history if a future corpus shows the shape.

    // Real ambiguity — never guess.
    let style = PathStyle::for_ecosystem(lineage.ecosystem.as_str());
    let rows: Vec<Candidate> = candidates
        .iter()
        .map(|c| build_candidate_row(pkg, style, c.intro))
        .collect();
    PathResolution::Ambiguous { candidates: rows }
}

fn near_misses(pkg: &PackageView, path: &[AddressSegment]) -> Vec<String> {
    let Some(leaf) = path.last() else {
        return Vec::new();
    };
    let leaf_lower = leaf.effective_name().to_lowercase();
    let prefix: String = leaf_lower.chars().take(3).collect();
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut names: Vec<String> = pkg
        .indexes()
        .by_name
        .prefix(&prefix)
        .map(|e| e.display.clone())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.truncate(8);
    names
}

fn resolve_path(
    pkg: &PackageView,
    lineage: &PackageLineageId,
    path: &[AddressSegment],
) -> PathResolution {
    if path.is_empty() {
        return PathResolution::NotFound {
            near_misses: Vec::new(),
        };
    }

    let stage1 = stage1_physical_exact(pkg, lineage, path);
    if !stage1.is_empty() {
        return ambiguous_or_collapsed(pkg, lineage, stage1);
    }

    let stage2 = stage2_alias_exact(pkg, path);
    if !stage2.is_empty() {
        return ambiguous_or_collapsed(pkg, lineage, stage2);
    }

    let stage3 = stage3_name_suffix(pkg, path);
    if stage3.is_empty() {
        return PathResolution::NotFound {
            near_misses: near_misses(pkg, path),
        };
    }
    ambiguous_or_collapsed(pkg, lineage, stage3)
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Resolve `path`/`key` against one already-selected package generation.
///
/// This is the pure, synchronous core: no corpus, no version lookup, no I/O
/// — just the three stages plus Stage 0's key handling. [`resolve`] is the
/// thin async wrapper that selects `pkg` from a [`Corpus`]/[`VersionRegistry`]
/// first.
///
/// # Stage 0 — the key, when present
///
/// A `#key` is never silently overridden by what the `sym-path` resolves to,
/// and never silently overrides it either:
/// * no path: the key alone decides ([`ResolveOutcome::Resolved`] or
///   [`ResolveOutcome::StaleKey`]).
/// * path resolves to the same declaration as the key: resolved, corroborated.
/// * path resolves to a *different* declaration (or an ambiguous set that
///   does not include the key's declaration): [`ResolveOutcome::AddressConflict`]
///   — neither signal is trusted over the other.
/// * path resolves to nothing: the hash is still authoritative (§4.14's
///   closure property — never remove or weaken the hash), so this resolves.
pub fn resolve_in_package(
    pkg: &PackageView,
    lineage: &PackageLineageId,
    path: &[AddressSegment],
    key: Option<IntroId>,
) -> ResolveOutcome {
    if let Some(key_intro) = key {
        if !pkg.view().is_live(key_intro) {
            return ResolveOutcome::StaleKey;
        }
        if path.is_empty() {
            return ResolveOutcome::Resolved {
                key: SymbolKey::new(lineage.clone(), key_intro),
                heuristic: None,
                suppressed_alternatives: 0,
            };
        }
        return match resolve_path(pkg, lineage, path) {
            PathResolution::Found {
                intro,
                heuristic,
                suppressed,
            } if intro == key_intro => ResolveOutcome::Resolved {
                key: SymbolKey::new(lineage.clone(), key_intro),
                heuristic,
                suppressed_alternatives: suppressed,
            },
            PathResolution::Found { .. } => ResolveOutcome::AddressConflict {
                message: format!(
                    "the symbol path resolved to a different declaration than #{}",
                    key_intro.to_hex()
                ),
            },
            PathResolution::Ambiguous { candidates }
                if candidates.iter().any(|c| c.intro == key_intro) =>
            {
                ResolveOutcome::Resolved {
                    key: SymbolKey::new(lineage.clone(), key_intro),
                    heuristic: None,
                    suppressed_alternatives: candidates.len().saturating_sub(1),
                }
            }
            PathResolution::Ambiguous { .. } => ResolveOutcome::AddressConflict {
                message:
                    "the symbol path's candidates do not include the declaration named by #hash"
                        .to_owned(),
            },
            PathResolution::NotFound { .. } => ResolveOutcome::Resolved {
                key: SymbolKey::new(lineage.clone(), key_intro),
                heuristic: None,
                suppressed_alternatives: 0,
            },
        };
    }

    match resolve_path(pkg, lineage, path) {
        PathResolution::Found {
            intro,
            heuristic,
            suppressed,
        } => ResolveOutcome::Resolved {
            key: SymbolKey::new(lineage.clone(), intro),
            heuristic,
            suppressed_alternatives: suppressed,
        },
        PathResolution::Ambiguous { candidates } => {
            let hint = refine_hint(&candidates);
            ResolveOutcome::Ambiguous {
                candidates,
                refine_hint: hint,
            }
        }
        PathResolution::NotFound { near_misses } => ResolveOutcome::NotFound { near_misses },
    }
}

async fn similar_package_names(corpus: &Corpus, requested: &str) -> Vec<String> {
    let requested_lower = requested.to_lowercase();
    let mut names: Vec<String> = corpus
        .packages()
        .await
        .into_iter()
        .map(|p| p.lineage().name.as_str().to_owned())
        .filter(|name| {
            let lower = name.to_lowercase();
            lower.contains(&requested_lower) || requested_lower.contains(&lower)
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names.truncate(8);
    names
}

/// Resolve a parsed [`Address`] against the live corpus.
///
/// Selects the right package generation first (this package's current
/// resident version, or a specific requested version via `versions`), then
/// delegates to [`resolve_in_package`]. See that function's docs for Stage 0
/// (`#key`) semantics.
///
/// `pub(crate)`, not `pub`: [`VersionRegistry`] is itself `pub(crate)`
/// (`crate::versions`), so no caller outside this crate could construct the
/// argument this needs anyway. External callers — including this crate's own
/// integration tests — exercise the resolver through [`resolve_in_package`],
/// which takes only public types and is the pure core this function wraps.
pub(crate) async fn resolve(
    corpus: &Corpus,
    versions: &VersionRegistry,
    address: &Address,
) -> ResolveOutcome {
    let lineage = PackageLineageId::new(
        EcosystemId::new(address.package.ecosystem.as_str()),
        PackageName::new(address.package.name.as_str()),
    );

    let selected: Option<Arc<PackageView>> = match &address.package.version {
        Some(requested) => {
            let slices = versions.slices(&lineage);
            match slices.iter().find(|s| &*s.version == requested.as_str()) {
                Some(s) => Some(Arc::clone(&s.package)),
                None if slices.is_empty() => None,
                None => {
                    let resident = slices.iter().map(|s| s.version.to_string()).collect();
                    return ResolveOutcome::VersionMismatch {
                        requested: requested.clone(),
                        resident,
                    };
                }
            }
        }
        None => corpus.package(&lineage).await,
    };

    let Some(pkg) = selected else {
        return ResolveOutcome::PackageNotIndexed {
            resident_similar: similar_package_names(corpus, &address.package.name).await,
        };
    };

    resolve_in_package(&pkg, &lineage, &address.path, address.key)
}

/// Parse `input` and resolve it against the live corpus in one call — the
/// convenience entry point most callers want.
///
/// `pub(crate)` for the same reason as [`resolve`].
pub(crate) async fn resolve_str(
    corpus: &Corpus,
    versions: &VersionRegistry,
    input: &str,
) -> ResolveOutcome {
    match Address::parse(input) {
        Ok(address) => resolve(corpus, versions, &address).await,
        Err(e) => e.into(),
    }
}

// ---------------------------------------------------------------------------
// Tests — parser/renderer unit coverage (no corpus needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str) -> AddressSegment {
        AddressSegment {
            name: Some(name.to_owned()),
            qualifiers: Vec::new(),
        }
    }

    #[test]
    fn legacy_form_parses_as_empty_path() {
        let hex = "a".repeat(64);
        let input = format!("cargo:serde#{hex}");
        let addr = Address::parse(&input).expect("legacy form must parse");
        assert_eq!(addr.package.ecosystem, "cargo");
        assert_eq!(addr.package.name, "serde");
        assert!(addr.package.version.is_none());
        assert!(addr.path.is_empty());
        assert_eq!(addr.key.map(|k| k.to_hex()), Some(hex));
    }

    #[test]
    fn full_address_round_trips_through_display() {
        let hex = "b".repeat(64);
        let input =
            format!("cargo:serde@1.0.196::serde::de::Deserializer::deserialize_map[method]#{hex}");
        let addr = Address::parse(&input).expect("full address must parse");
        assert_eq!(addr.package.name, "serde");
        assert_eq!(addr.package.version.as_deref(), Some("1.0.196"));
        assert_eq!(addr.path.len(), 4);
        assert_eq!(addr.path[0].name.as_deref(), Some("serde"));
        assert_eq!(addr.path[3].name.as_deref(), Some("deserialize_map"));
        assert_eq!(addr.path[3].kind_qualifier(), Some("method"));
        assert_eq!(addr.to_string(), input);
    }

    #[test]
    fn npm_scoped_package_uses_the_last_at_as_version_separator() {
        let addr = Address::parse("npm:@types/node@20.11.0").expect("must parse");
        assert_eq!(addr.package.name, "@types/node");
        assert_eq!(addr.package.version.as_deref(), Some("20.11.0"));
    }

    #[test]
    fn maven_group_colon_artifact_survives_the_single_ecosystem_split() {
        let addr = Address::parse(
            "maven:com.google.code.gson:gson@2.11.0::com.google.gson.Gson.toJson[method]",
        )
        .expect("must parse");
        assert_eq!(addr.package.name, "com.google.code.gson:gson");
        assert_eq!(addr.package.version.as_deref(), Some("2.11.0"));
        // "." is a segment separator (grammar's `sep`), so the dotted FQN
        // splits into five segments: com, google, gson, Gson, toJson.
        assert_eq!(addr.path.len(), 5);
        assert_eq!(addr.path.last().unwrap().name.as_deref(), Some("toJson"));
    }

    #[test]
    fn nested_brackets_and_generics_parse_as_one_opaque_qualifier() {
        let addr =
            Address::parse("java:foo::readValue[method(byte[],Class<?>)]").expect("must parse");
        let last = addr.path.last().unwrap();
        assert_eq!(last.name.as_deref(), Some("readValue"));
        assert_eq!(last.qualifiers.len(), 1);
        assert_eq!(last.kind_qualifier(), Some("method(byte[],Class<?>)"));
    }

    #[test]
    fn quoted_non_identifier_segment_parses() {
        let addr = Address::parse("nuget:Dapper@2.1.35::Dapper.DapperRow.'this[]'[property]")
            .expect("must parse");
        let indexer = &addr.path[2];
        assert_eq!(indexer.name.as_deref(), Some("this[]"));
        assert_eq!(indexer.kind_qualifier(), Some("property"));
    }

    #[test]
    fn anonymous_impl_segment_has_no_name_but_effective_name_is_impl() {
        let addr = Address::parse(
            "cargo:serde_json::serde_json::value::de::[impl Deserializer for Value]::deserialize_str[method]",
        )
        .expect("must parse");
        let anon = &addr.path[3];
        assert!(anon.name.is_none());
        assert_eq!(anon.effective_name(), "impl");
        assert_eq!(anon.kind_qualifier(), Some("impl Deserializer for Value"));
    }

    #[test]
    fn dot_and_slash_separators_normalize_like_double_colon() {
        let a = Address::parse("cargo:serde::de.Deserializer/deserialize_map[method]")
            .expect("must parse");
        assert_eq!(
            a.path
                .iter()
                .map(|s| s.name.clone().unwrap())
                .collect::<Vec<_>>(),
            vec!["de", "Deserializer", "deserialize_map"]
        );
    }

    #[test]
    fn purl_ecosystem_tags_normalize_on_input_never_on_output() {
        let a = Address::parse("golang:std::fmt::Stringer[trait]").expect("must parse");
        assert_eq!(a.package.ecosystem, "go");
        assert_eq!(a.package.name, "std");
        // Go's path style is `.`, so the two sym-path segments ("fmt",
        // "Stringer[trait]") render dot-joined; only the fixed pkg-coord/
        // sym-path boundary itself is always "::".
        assert_eq!(a.to_string(), "go:std::fmt.Stringer[trait]");

        let b = Address::parse("clang:zlib::deflate[function]").expect("must parse");
        assert_eq!(b.package.ecosystem, "cpp");
    }

    #[test]
    fn unbalanced_bracket_is_rejected_with_a_useful_offset() {
        let err = Address::parse("cargo:serde::Deserializer[trait").expect_err("must reject");
        // Offset points at the unmatched '[' itself.
        let open_at = "cargo:serde::Deserializer[trait".find('[').unwrap();
        assert_eq!(err.offset, open_at);
        assert!(err.message.contains("unbalanced"), "got {:?}", err.message);
    }

    #[test]
    fn mismatched_bracket_type_is_rejected() {
        let err = Address::parse("cargo:serde::Foo[trait)").expect_err("must reject");
        assert!(err.message.contains("mismatched"), "got {:?}", err.message);
    }

    #[test]
    fn missing_ecosystem_colon_is_a_parse_error_not_a_panic() {
        let err = Address::parse("not-an-address-at-all").expect_err("must reject");
        assert!(err.message.contains("':'"), "got {:?}", err.message);
    }

    #[test]
    fn empty_ecosystem_is_rejected() {
        let err = Address::parse(":serde").expect_err("must reject");
        assert_eq!(err.offset, 0);
    }

    #[test]
    fn short_key_is_rejected_not_silently_truncated() {
        let err = Address::parse("cargo:serde#abcd").expect_err("must reject");
        assert!(err.message.contains("64 hex"), "got {:?}", err.message);
    }

    #[test]
    fn empty_qualifier_block_is_rejected() {
        let err = Address::parse("cargo:serde::Foo[]").expect_err("must reject");
        assert!(err.message.contains("empty"), "got {:?}", err.message);
    }

    #[test]
    fn empty_version_after_at_is_rejected() {
        let err = Address::parse("npm:left-pad@").expect_err("must reject");
        assert!(err.message.contains("version"), "got {:?}", err.message);
    }

    #[test]
    fn path_style_matches_ecosystem() {
        assert_eq!(PathStyle::for_ecosystem("cargo").separator(), "::");
        assert_eq!(PathStyle::for_ecosystem("cpp").separator(), "::");
        assert_eq!(PathStyle::for_ecosystem("maven").separator(), ".");
        assert_eq!(PathStyle::for_ecosystem("pypi").separator(), ".");
        assert_eq!(PathStyle::for_ecosystem("npm").separator(), ".");
        assert_eq!(PathStyle::for_ecosystem("nuget").separator(), ".");
        assert_eq!(PathStyle::for_ecosystem("go").separator(), ".");
    }

    #[test]
    fn display_uses_ecosystem_correct_separator() {
        let rust = Address {
            package: PackageCoord {
                ecosystem: "cargo".to_owned(),
                name: "serde".to_owned(),
                version: None,
            },
            path: vec![seg("de"), seg("Deserializer")],
            key: None,
        };
        assert_eq!(rust.to_string(), "cargo:serde::de::Deserializer");

        let java = Address {
            package: PackageCoord {
                ecosystem: "maven".to_owned(),
                name: "com.google.code.gson:gson".to_owned(),
                version: None,
            },
            path: vec![seg("com"), seg("google"), seg("gson"), seg("Gson")],
            key: None,
        };
        assert_eq!(
            java.to_string(),
            "maven:com.google.code.gson:gson::com.google.gson.Gson"
        );
    }
}
