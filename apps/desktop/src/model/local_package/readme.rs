//! README projection into structural blocks.
//!
//! The dossier renders headings, prose, bullets, and fenced code. Inline
//! markup is kept verbatim; the projection only recovers document structure.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

/// Largest README prefix read from disk.
const MAX_README_BYTES: u64 = 512 * 1024;
/// Largest number of blocks retained for one README.
const MAX_BLOCKS: usize = 1_024;

/// One structural README block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadmeBlock {
    /// An ATX heading (`#` through `######`).
    Heading {
        /// Heading depth, 1–6.
        level: u8,
        /// Heading text without its markers.
        text: Arc<str>,
    },
    /// Consecutive prose lines joined with spaces.
    Paragraph(Arc<str>),
    /// One `-` or `*` list item.
    Bullet(Arc<str>),
    /// A fenced code block.
    Code {
        /// Fence info string, when one was given.
        language: Option<Arc<str>>,
        /// Code lines joined with newlines.
        text: Arc<str>,
    },
}

/// Reads at most [`MAX_README_BYTES`] of a README, lossily decoded.
pub(super) fn read(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file.take(MAX_README_BYTES).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Returns the first prose paragraph, skipping headings and fences.
pub(super) fn first_paragraph(readme: &str) -> String {
    let mut paragraph = Vec::new();
    let mut in_code = false;
    for line in readme.lines().map(str::trim) {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
        } else {
            paragraph.push(line);
        }
    }
    paragraph.join(" ")
}

/// Derives a package-like name from the README's first level-one heading.
///
/// `# Forge Project v2` becomes `forge-project`: a trailing version word is
/// not part of the name.
pub(super) fn title(readme: &str) -> Option<String> {
    let title = readme
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))?;
    let name = title
        .split_whitespace()
        .take_while(|part| !is_version_word(part))
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    (!name.is_empty()).then_some(name)
}

fn is_version_word(word: &str) -> bool {
    word.strip_prefix('v')
        .is_some_and(|rest| rest.starts_with(|character: char| character.is_ascii_digit()))
}

/// Projects Markdown into [`ReadmeBlock`]s.
pub(super) fn parse(readme: &str) -> Vec<ReadmeBlock> {
    let mut parser = Parser::default();
    for line in readme.lines() {
        if parser.blocks.len() >= MAX_BLOCKS {
            break;
        }
        parser.line(line);
    }
    parser.finish()
}

/// An open fenced code block.
struct Fence<'source> {
    language: Option<Arc<str>>,
    lines: Vec<&'source str>,
}

impl Fence<'_> {
    fn close(self) -> ReadmeBlock {
        ReadmeBlock::Code {
            language: self.language,
            text: Arc::from(self.lines.join("\n")),
        }
    }
}

#[derive(Default)]
struct Parser<'source> {
    blocks: Vec<ReadmeBlock>,
    paragraph: Vec<&'source str>,
    fence: Option<Fence<'source>>,
}

impl<'source> Parser<'source> {
    fn line(&mut self, line: &'source str) {
        let trimmed = line.trim();
        if let Some(info) = trimmed.strip_prefix("```") {
            self.flush_paragraph();
            if let Some(fence) = self.fence.take() {
                self.blocks.push(fence.close());
            } else {
                let info = info.trim();
                self.fence = Some(Fence {
                    language: (!info.is_empty()).then(|| Arc::from(info)),
                    lines: Vec::new(),
                });
            }
        } else if let Some(fence) = self.fence.as_mut() {
            fence.lines.push(line);
        } else if trimmed.starts_with('#') {
            self.flush_paragraph();
            let text = trimmed.trim_start_matches('#');
            let depth = trimmed.len() - text.len();
            self.blocks.push(ReadmeBlock::Heading {
                level: u8::try_from(depth.min(6)).unwrap_or(6),
                text: Arc::from(text.trim()),
            });
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            self.flush_paragraph();
            self.blocks
                .push(ReadmeBlock::Bullet(Arc::from(item.trim())));
        } else if trimmed.is_empty() {
            self.flush_paragraph();
        } else {
            self.paragraph.push(trimmed);
        }
    }

    fn flush_paragraph(&mut self) {
        if !self.paragraph.is_empty() {
            self.blocks
                .push(ReadmeBlock::Paragraph(Arc::from(self.paragraph.join(" "))));
            self.paragraph.clear();
        }
    }

    fn finish(mut self) -> Vec<ReadmeBlock> {
        self.flush_paragraph();
        // An unterminated fence still shows the code the author wrote.
        if let Some(fence) = self.fence.take().filter(|fence| !fence.lines.is_empty()) {
            self.blocks.push(fence.close());
        }
        self.blocks.truncate(MAX_BLOCKS);
        self.blocks
    }
}
