//! Documentation fragments folded into renderable prose blocks.
//! A producer emits a flat fragment stream; a reader needs paragraphs, code
//! blocks, and inline links. This module performs that fold and nothing else.
//!
//! Links are the point. A [`backend_library::Fragment::Link`] already carries a
//! stable declaration target, so documentation in this application is
//! hyperlinked end to end without any text scraping: the link is data the
//! compiler produced, not a pattern matched out of prose.

use backend_library::{Fragment, SymbolKey};

/// One run of prose, optionally pointing at a declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    text: String,
    link: Option<SymbolKey>,
}

impl Span {
    /// Returns the run's text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the declaration this run opens, when it has one.
    pub(crate) const fn link(&self) -> Option<SymbolKey> {
        self.link
    }
}

/// One block of documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Block {
    /// A paragraph of runs, some of which may be links.
    Paragraph(Vec<Span>),
    /// A code example, rendered as a specimen.
    Code(String),
}

impl Block {
    /// Returns the plain text of this block, for summaries and tooltips.
    pub(crate) fn plain(&self) -> String {
        match self {
            Self::Paragraph(spans) => spans.iter().map(Span::text).collect(),
            Self::Code(code) => code.clone(),
        }
    }
}

/// Folds a fragment stream into prose blocks.
pub(crate) fn fold(fragments: &[Fragment]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<Span> = Vec::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(text) => push_text(&mut paragraph, text),
            Fragment::Link { label, target } => paragraph.push(Span {
                text: label.clone(),
                link: Some(*target),
            }),
            Fragment::Code(code) => {
                flush(&mut blocks, &mut paragraph);
                blocks.push(Block::Code(code.clone()));
            }
            Fragment::Break => flush(&mut blocks, &mut paragraph),
        }
    }
    flush(&mut blocks, &mut paragraph);
    blocks
}

/// Returns the first sentence of the first paragraph, for one-line summaries.
pub(crate) fn summary(blocks: &[Block]) -> Option<String> {
    let paragraph = blocks.iter().find_map(|block| match block {
        Block::Paragraph(spans) if !spans.is_empty() => Some(block_text(spans)),
        _ => None,
    })?;
    let trimmed = paragraph.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(". ")
        .map_or(trimmed.len(), |at| at.saturating_add(1));
    Some(trimmed.get(..end).unwrap_or(trimmed).trim().to_owned())
}

fn block_text(spans: &[Span]) -> String {
    spans.iter().map(Span::text).collect()
}

fn push_text(paragraph: &mut Vec<Span>, text: &str) {
    if text.is_empty() {
        return;
    }
    match paragraph.last_mut() {
        Some(last) if last.link.is_none() => last.text.push_str(text),
        _ => paragraph.push(Span {
            text: text.to_owned(),
            link: None,
        }),
    }
}

fn flush(blocks: &mut Vec<Block>, paragraph: &mut Vec<Span>) {
    if paragraph.is_empty() {
        return;
    }
    let spans = core::mem::take(paragraph);
    if spans.iter().all(|span| span.text.trim().is_empty() && span.link.is_none()) {
        return;
    }
    blocks.push(Block::Paragraph(spans));
}
