//! An identifier-aware tokenizer for symbol search.
//!
//! The symbol index keys everything on raw `STRING` fields plus lowercased shadow
//! fields matched with `.*needle.*` regexes ([`super::index`]). That finds exact
//! and substring matches, but it cannot match *across word boundaries the way
//! programmers think of names*: `user` does not find `getUserById`, `http` does
//! not find `HTTPServer`, and `read_to_string` is one opaque blob. Programmers
//! search by the *parts* of a name, so the index has to know the parts.
//!
//! This tokenizer splits an identifier into its component words — on
//! `snake_case`, `kebab-case`, path separators (`::`, `.`), camelCase/PascalCase
//! humps, acronym runs (`HTTPServer` → `http`, `server`), and digit boundaries
//! (`utf8` → `utf`, `8`). A single-word query (`user`) then matches the matching
//! subtoken of `getUserById`, and a multi-word query (`get user`) matches in
//! order via a phrase query. Downstream `LowerCaser` makes matching
//! case-insensitive.
//!
//! It deliberately does **not** re-emit the whole original identifier as an extra
//! token: doing so makes a one-word query tokenize to two identical terms, which
//! the query parser turns into an unsatisfiable phrase. Whole-name exact matching
//! is the job of the raw `STRING` name field (the top tier of the symbol query),
//! not of this analyzer.
//!
//! It is deliberately language-agnostic: every non-alphanumeric char is a break,
//! so Rust's `::`, Python/Go/Java/TS `.`, TS `#private`, and `$` all split
//! uniformly (see the cross-language notes in the search research). Register it
//! on an [`Index`] with [`register`] — tantivy tokenizers live in a per-index
//! manager and must be re-registered on every open, which is why this is a
//! function called from `open_or_create`, not global state.
//!
//! **C# / .NET note**: the CLR metadata format encodes generic arity with a
//! backtick suffix (e.g. `` List`1 ``, `` Dictionary`2 ``). The tokenizer strips
//! the `` `<digits> `` suffix before splitting so `` List`1 `` tokenizes as
//! `list`, not `list` + `1`. The digit following the backtick is suppressed
//! entirely; the meaningful subword is the base name only.

use tantivy::{
    Index,
    tokenizer::{LowerCaser, RemoveLongFilter, TextAnalyzer, Token, TokenStream, Tokenizer},
};

/// The registered name of the identifier analyzer. A `TEXT` field wanting
/// subtoken search sets its indexing tokenizer to this.
pub const IDENT_TOKENIZER: &str = "ident";

/// Longest single token we keep — guards against a pathological mangled
/// identifier blowing up the term dictionary. Real symbol names are far shorter.
const MAX_TOKEN_CHARS: usize = 64;

/// Strip the CLR generic-arity suffix `` `<digits> `` from a word if present.
///
/// C# / .NET metadata names encode the number of type parameters as a trailing
/// `` `N `` (e.g. `` List`1 ``, `` Dictionary`2 ``). The digits carry no search
/// value — the base name is what users query — so we drop the suffix before
/// handing the word to [`split_identifier`].
///
/// Only strips when the pattern is `` ` `` followed by one or more ASCII digits
/// at the *end* of the word; mid-word backticks are left for the generic
/// separator logic.
#[inline]
fn strip_arity_suffix(word: &str) -> &str {
    // Fast path: backtick must appear for any stripping to occur.
    if let Some(tick) = word.rfind('`') {
        let tail = &word[tick + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return &word[..tick];
        }
    }
    word
}

/// Byte spans of the sub-words inside one identifier `word`.
///
/// Boundaries: any non-alphanumeric char (dropped as a separator), a
/// lower→Upper camel hump, the tail of an acronym run (`UpperUpperlower`), and a
/// digit↔letter transition. Empty spans (runs of separators) are skipped.
///
/// The input is pre-processed by [`strip_arity_suffix`] so C# generic names
/// (`` List`1 ``, `` Dictionary`2 ``) lose their arity before splitting.
pub(crate) fn split_identifier(word: &str) -> Vec<(usize, usize)> {
    let word = strip_arity_suffix(word);
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let byte_end = word.len();

    let mut spans = Vec::new();
    let mut start: Option<usize> = None; // byte offset of the open span, if any

    for i in 0..chars.len() {
        let (byte, ch) = chars[i];

        if !ch.is_alphanumeric() {
            // Separator: close any open span, and stay closed until the next alnum.
            if let Some(s) = start.take() {
                spans.push((s, byte));
            }
            continue;
        }

        if start.is_none() {
            start = Some(byte);
            continue;
        }

        let (_, prev) = chars[i - 1];
        let hump = ch.is_uppercase() && prev.is_lowercase();
        let acronym_tail = prev.is_uppercase()
            && ch.is_uppercase()
            && chars.get(i + 1).is_some_and(|&(_, n)| n.is_lowercase());
        let digit_edge = ch.is_ascii_digit() != prev.is_ascii_digit();

        if hump || acronym_tail || digit_edge {
            let s = start.replace(byte).unwrap();
            spans.push((s, byte));
        }
    }
    if let Some(s) = start {
        spans.push((s, byte_end));
    }
    spans
}

/// The lowercased sub-words of a query string — the query-side counterpart of
/// what [`IdentifierTokenizer`] indexes, so a query term is split the same way a
/// stored name was. Splitting the *original-case* input is essential: a query
/// like `getUser` must break on its camel hump before being lowercased.
pub(crate) fn subtokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .flat_map(|word| {
            split_identifier(word)
                .into_iter()
                .map(move |(s, e)| word[s..e].to_lowercase())
        })
        .collect()
}

/// Tokenizer that yields, for each whitespace-delimited word, its sub-words (see
/// [`split_identifier`]).
#[derive(Clone, Default)]
pub struct IdentifierTokenizer;

/// The precomputed token stream — building the full `Vec<Token>` up front keeps
/// the `advance`/`token` state machine trivial and side-steps the borrow dance
/// of a lazy splitter.
pub struct IdentifierTokenStream {
    tokens: std::vec::IntoIter<Token>,
    current: Token,
}

impl Tokenizer for IdentifierTokenizer {
    type TokenStream<'a> = IdentifierTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let mut tokens = Vec::new();
        let mut position = 0usize;

        let mut push = |from: usize, to: usize, pos: &mut usize| {
            if from >= to {
                return;
            }
            tokens.push(Token {
                offset_from: from,
                offset_to: to,
                position: *pos,
                text: text[from..to].to_string(),
                position_length: 1,
            });
            *pos += 1;
        };

        // Walk whitespace-delimited words, tracking byte offsets (not str::find,
        // which would mis-locate a repeated word).
        let mut word_start: Option<usize> = None;
        let mut emit_word = |start: usize, end: usize, position: &mut usize| {
            let word = &text[start..end];
            // Only the sub-words: a whole-identifier match is served by the raw
            // STRING name field, and re-emitting the original here would break
            // single-word queries (see the module docs).
            for (s, e) in split_identifier(word) {
                push(start + s, start + e, position);
            }
        };

        for (byte, ch) in text.char_indices() {
            if ch.is_whitespace() {
                if let Some(s) = word_start.take() {
                    emit_word(s, byte, &mut position);
                }
            } else if word_start.is_none() {
                word_start = Some(byte);
            }
        }
        if let Some(s) = word_start {
            emit_word(s, text.len(), &mut position);
        }

        IdentifierTokenStream {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

impl TokenStream for IdentifierTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(token) => {
                self.current = token;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

/// Register the [`IDENT_TOKENIZER`] analyzer on `index`.
///
/// Must be called after every [`Index::open`]/`open_or_create`, before the first
/// search or write that touches an `ident`-tokenized field, because tantivy's
/// tokenizer manager is per-`Index` in-memory state, not persisted with the
/// segments.
pub fn register(index: &Index) {
    let analyzer = TextAnalyzer::builder(IdentifierTokenizer)
        .filter(RemoveLongFilter::limit(MAX_TOKEN_CHARS))
        .filter(LowerCaser)
        .build();
    index.tokenizers().register(IDENT_TOKENIZER, analyzer);
}
