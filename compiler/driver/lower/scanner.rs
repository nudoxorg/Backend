//! Defines lower scanner behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower scanner invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
/// ASCII lexical token used only to reject comments, quoted literals, and nested declarations
/// before the closed lowering subset examines exact declaration words.
#[derive(Clone, Copy)]
pub(super) enum SyntaxToken<'source> {
    Word(&'source [u8]),
    Symbol(u8),
    Literal,
}

/// A token with its enclosing brace depth before that token changes the depth.
#[derive(Clone, Copy)]
pub(super) struct ScannedToken<'source> {
    pub(super) token: SyntaxToken<'source>,
    pub(super) brace_depth: u32,
}

/// Small non-semantic scanner shared by the TypeScript, C#, Go, and Java declaration subsets.
pub(super) struct DeclarationScanner<'source> {
    source: &'source [u8],
    offset: usize,
    brace_depth: u32,
}

impl<'source> DeclarationScanner<'source> {
    pub(super) const fn new(source: &'source [u8]) -> Self {
        Self {
            source,
            offset: 0,
            brace_depth: 0,
        }
    }

    pub(super) fn next(&mut self) -> Option<ScannedToken<'source>> {
        self.skip_whitespace_and_comments();
        let byte = *self.source.get(self.offset)?;
        let brace_depth = self.brace_depth;
        if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' {
            let start = self.offset;
            self.offset += 1;
            while self
                .source
                .get(self.offset)
                .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_' || *next == b'$')
            {
                self.offset += 1;
            }
            return Some(ScannedToken {
                token: SyntaxToken::Word(&self.source[start..self.offset]),
                brace_depth,
            });
        }
        if byte == b'\'' || byte == b'"' || byte == b'`' {
            self.skip_quoted(byte);
            return Some(ScannedToken {
                token: SyntaxToken::Literal,
                brace_depth,
            });
        }
        self.offset += 1;
        if byte == b'{' {
            self.brace_depth = self.brace_depth.saturating_add(1);
        } else if byte == b'}' {
            self.brace_depth = self.brace_depth.saturating_sub(1);
        }
        Some(ScannedToken {
            token: SyntaxToken::Symbol(byte),
            brace_depth,
        })
    }

    /// Skips one balanced parenthesized group, including any quoted bytes
    /// inside it, and returns whether a complete group closed before the
    /// source ended.
    pub(super) fn skip_balanced_parens(&mut self) -> bool {
        let mut depth = 1_u32;
        while let Some(scanned) = self.next() {
            match scanned.token {
                SyntaxToken::Symbol(b'(') => depth = depth.saturating_add(1),
                SyntaxToken::Symbol(b')') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while self
                .source
                .get(self.offset)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.offset += 1;
            }
            let Some(rest) = self.source.get(self.offset..) else {
                return;
            };
            if rest.starts_with(b"//") {
                self.offset += 2;
                while self
                    .source
                    .get(self.offset)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.offset += 1;
                }
                continue;
            }
            if rest.starts_with(b"/*") {
                self.offset += 2;
                while self.offset + 1 < self.source.len()
                    && &self.source[self.offset..self.offset + 2] != b"*/"
                {
                    self.offset += 1;
                }
                self.offset = self.offset.saturating_add(2).min(self.source.len());
                continue;
            }
            return;
        }
    }

    fn skip_quoted(&mut self, quote: u8) {
        if quote == b'"'
            && self
                .source
                .get(self.offset..)
                .is_some_and(|rest| rest.starts_with(b"\"\"\""))
        {
            self.offset += 3;
            while self.offset + 2 < self.source.len()
                && !self.source[self.offset..].starts_with(b"\"\"\"")
            {
                self.offset += 1;
            }
            self.offset = self.offset.saturating_add(3).min(self.source.len());
            return;
        }
        self.offset += 1;
        while let Some(byte) = self.source.get(self.offset).copied() {
            self.offset += 1;
            if byte == b'\\' {
                self.offset = self.offset.saturating_add(1).min(self.source.len());
            } else if byte == quote {
                break;
            }
        }
    }
}

/// Returns the first top-level Java type name while sharing the scanner's comment/literal rules.
///
/// The native javac adapter needs the public type's exact name for its
/// source-file artifact; this lookup performs no lowering.
pub(crate) fn java_top_level_type_name(source: &[u8]) -> Option<&[u8]> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0
            || !matches!(scanned.token, SyntaxToken::Word(word) if is_java_type_keyword(word))
        {
            continue;
        }
        return match scanner.next() {
            Some(next) if next.brace_depth == 0 => match next.token {
                SyntaxToken::Word(name) => Some(name),
                SyntaxToken::Symbol(_) | SyntaxToken::Literal => None,
            },
            _ => None,
        };
    }
    None
}

fn is_java_type_keyword(word: &[u8]) -> bool {
    matches!(word, b"class" | b"record" | b"interface" | b"enum")
}
