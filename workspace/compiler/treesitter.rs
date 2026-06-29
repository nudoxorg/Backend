//! Production tree-sitter CST extraction.
//!
//! Given a symbol's source and span, finds the enclosing function, extracts a
//! self-contained snippet, re-parses it to an s-expression, and resolves the
//! references within it. Falls back gracefully (full text, no CST) on an
//! unsupported language or a parse failure.
