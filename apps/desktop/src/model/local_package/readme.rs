//! README projection into structural blocks.
//!
//! The dossier renders headings, prose, bullets, and fenced code. Inline
//! markup is kept verbatim; the projection only recovers document structure.

use super::files::{
    editor_hint_for_opened_file, open_relative_source, read_bytes_under, read_text_under,
};
use backend_platform::directory::DirectoryCapability;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

/// One link parsed from a README on its bounded local-read worker.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReadmeLink {
    /// Visible label, with inline Markdown formatting removed.
    pub label: Arc<str>,
    /// The exact destination from the Markdown AST.
    pub destination: Arc<str>,
    /// A canonical editor/display path hint beneath the local project root,
    /// when the README target named a readable file at read time. This is not
    /// filesystem authority; source reads still use a held directory handle.
    pub local_file: Option<Arc<str>>,
    /// One-based source line from a `#L…` or `#L…-L…` fragment.
    pub line: Option<u32>,
}

/// A heading target suitable for resolving a Markdown fragment.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReadmeHeading {
    /// GitHub-style fragment spelling, including a duplicate suffix.
    pub slug: Arc<str>,
    /// Stable element identity for this source offset.
    pub element_id: Arc<str>,
    /// Plain heading words for accessibility and link lists.
    pub title: Arc<str>,
    /// Markdown heading depth, from 1 through 6.
    pub level: u8,
}

const MAX_LINKS: usize = 512;
const MAX_HEADINGS: usize = 512;
const MAX_LINK_TEXT_BYTES: usize = 4 * 1024;

/// Largest README prefix read from disk.
const MAX_README_BYTES: u64 = 512 * 1024;
/// Largest number of blocks retained for one README.
const MAX_BLOCKS: usize = 1_024;

/// One structural README block.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
pub(super) fn read(root: &Path, path: &Path) -> String {
    read_bytes_under(root, path, MAX_README_BYTES)
        .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
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

/// Projects a conventional README at the project root.
///
/// Package identity stays with the canonical manifest reader. This only
/// opens a README file the dossier can render.
pub(super) fn project_readme(root: &Path) -> Arc<[ReadmeBlock]> {
    project_readme_source(root)
        .as_deref()
        .map_or_else(|| Arc::from([]), |source| parse(source).into())
}

/// Reads the first conventional README source without lowering its Markdown.
pub(super) fn project_readme_source(root: &Path) -> Option<Arc<str>> {
    project_readme_file(root).map(|(_, source)| source)
}

/// Reads the first conventional README and retains the exact file it came
/// from so relative links resolve against its directory.
pub(super) fn project_readme_file(root: &Path) -> Option<(std::path::PathBuf, Arc<str>)> {
    for name in ["README.md", "README.markdown", "README", "readme.md"] {
        let path = root.join(name);
        let text = read(root, &path);
        if text.is_empty() {
            continue;
        }
        return Some((path, Arc::from(text)));
    }
    None
}

/// Extracts the bounded link and heading index used by the reader.
///
/// This runs in the local-read worker alongside the README read. Rendering
/// uses `gpui_component`'s Markdown TextView; this index exists only to wire
/// its destinations into typed shell actions and keyboard targets.
pub(super) fn navigation_index(
    source: &str,
    project_root: &Path,
    readme_path: &Path,
) -> (Arc<[ReadmeLink]>, Arc<[ReadmeHeading]>) {
    let Ok(ast) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
        return (Arc::from([]), Arc::from([]));
    };
    let mut definitions = std::collections::BTreeMap::new();
    collect_definitions(&ast, &mut definitions);
    let mut links = Vec::new();
    let mut headings = Vec::new();
    let mut slug_counts = std::collections::BTreeMap::<String, usize>::new();
    let mut used_slugs = BTreeSet::<String>::new();
    collect_navigation(
        &ast,
        &definitions,
        &mut slug_counts,
        &mut used_slugs,
        &mut links,
        &mut headings,
    );
    let Ok(root_capability) = DirectoryCapability::open_read_only_source(project_root) else {
        return (links.into(), headings.into());
    };
    let readme_relative = match readme_path.strip_prefix(project_root) {
        Ok(relative) => relative,
        Err(_) => return (links.into(), headings.into()),
    };
    let readme_directory = readme_relative.parent().unwrap_or_else(|| Path::new(""));
    for link in &mut links {
        let (local_file, line) = resolve_local_file(
            &link.destination,
            project_root,
            readme_directory,
            &root_capability,
        );
        link.local_file = local_file.map(Arc::from);
        link.line = line;
    }
    (links.into(), headings.into())
}

fn resolve_local_file(
    destination: &str,
    project_root: &Path,
    readme_directory: &Path,
    root_capability: &DirectoryCapability,
) -> (Option<String>, Option<u32>) {
    let destination = destination.trim();
    if destination.is_empty()
        || destination.starts_with('#')
        || destination.starts_with('/')
        || destination.starts_with("\\\\")
        || destination.bytes().any(|byte| byte.is_ascii_control())
        || has_uri_scheme(destination)
    {
        return (None, None);
    }
    let (path_and_query, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    let path = path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path);
    let Ok(decoded) = percent_decode_path(path) else {
        return (None, None);
    };
    if decoded.is_empty() {
        return (None, fragment.and_then(source_line_fragment));
    }
    let Some(relative) = project_relative_target(readme_directory, &decoded) else {
        return (None, None);
    };
    let Ok(file) = open_relative_source(root_capability, &relative) else {
        return (None, None);
    };
    let Ok(metadata) = file.metadata() else {
        return (None, None);
    };
    // This path is for a later, explicit editor handoff only. The no-follow
    // descriptor above is the evidence used to read the file.
    let path = editor_hint_for_opened_file(project_root, &relative, &metadata)
        .and_then(|candidate| candidate.to_str().map(str::to_owned));
    (path, fragment.and_then(source_line_fragment))
}

fn project_relative_target(readme_directory: &Path, target: &str) -> Option<std::path::PathBuf> {
    let mut components = readme_directory
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.len() != readme_directory.components().count() {
        return None;
    }
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value.to_owned()),
        }
    }
    (!components.is_empty()).then(|| components.into_iter().collect())
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character.is_ascii_alphabetic()
            } else {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
            }
        })
}

fn percent_decode_path(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (bytes.get(index + 1), bytes.get(index + 2)) else {
                return Err(());
            };
            let (Some(high), Some(low)) = (hex(*high), hex(*low)) else {
                return Err(());
            };
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
        || decoded.contains('\\')
    {
        return Err(());
    }
    Ok(decoded)
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn source_line_fragment(fragment: &str) -> Option<u32> {
    let first = fragment
        .strip_prefix('L')?
        .split_once('-')
        .map_or(fragment.strip_prefix('L')?, |(first, _)| first);
    let line = first.parse::<u32>().ok()?;
    (line > 0).then_some(line)
}

fn collect_definitions(
    node: &markdown::mdast::Node,
    definitions: &mut std::collections::BTreeMap<String, String>,
) {
    if let markdown::mdast::Node::Definition(definition) = node {
        definitions
            .entry(definition.identifier.clone())
            .or_insert_with(|| definition.url.clone());
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_definitions(child, definitions);
        }
    }
}

fn collect_navigation(
    node: &markdown::mdast::Node,
    definitions: &std::collections::BTreeMap<String, String>,
    slug_counts: &mut std::collections::BTreeMap<String, usize>,
    used_slugs: &mut BTreeSet<String>,
    links: &mut Vec<ReadmeLink>,
    headings: &mut Vec<ReadmeHeading>,
) {
    use markdown::mdast::Node;

    match node {
        Node::Link(link) if links.len() < MAX_LINKS => {
            push_link(links, plain_children(&link.children), &link.url);
        }
        Node::LinkReference(link) if links.len() < MAX_LINKS => {
            if let Some(destination) = definitions.get(&link.identifier) {
                push_link(links, plain_children(&link.children), destination);
            }
        }
        Node::Heading(heading) if headings.len() < MAX_HEADINGS => {
            let title = plain_children(&heading.children);
            let base = heading_slug(&title);
            if !base.is_empty() {
                let occurrence = slug_counts.entry(base.clone()).or_default();
                let mut slug = if *occurrence == 0 {
                    base.clone()
                } else {
                    format!("{base}-{}", *occurrence)
                };
                while used_slugs.contains(&slug) {
                    *occurrence = occurrence.saturating_add(1);
                    slug = format!("{base}-{}", *occurrence);
                }
                *occurrence = occurrence.saturating_add(1);
                used_slugs.insert(slug.clone());
                let offset = heading
                    .position
                    .as_ref()
                    .map_or(0, |position| position.start.offset);
                headings.push(ReadmeHeading {
                    slug: Arc::from(slug),
                    element_id: Arc::from(format!("readme-heading-{offset}")),
                    title: bounded(&title),
                    level: heading.depth,
                });
            }
        }
        _ => {}
    }
    // Code and inline-code nodes have no children and are never scanned for
    // apparent links. Link nodes recurse only after their own destination is
    // recorded, preserving nested formatting while avoiding duplicate links.
    if let Some(children) = node.children() {
        for child in children {
            collect_navigation(child, definitions, slug_counts, used_slugs, links, headings);
        }
    }
}

fn push_link(links: &mut Vec<ReadmeLink>, label: String, destination: &str) {
    if destination.is_empty()
        || destination.len() > super::MAX_README_LINK_DESTINATION_BYTES
    {
        return;
    }
    links.push(ReadmeLink {
        label: bounded(&label),
        destination: Arc::from(destination),
        local_file: None,
        line: None,
    });
}

fn bounded(value: &str) -> Arc<str> {
    if value.len() <= MAX_LINK_TEXT_BYTES {
        return Arc::from(value);
    }
    let mut end = MAX_LINK_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Arc::from(&value[..end])
}

fn plain_children(children: &[markdown::mdast::Node]) -> String {
    use markdown::mdast::Node;
    let mut out = String::new();
    for node in children {
        match node {
            Node::Text(text) => out.push_str(&text.value),
            Node::InlineCode(code) => out.push_str(&code.value),
            Node::Code(code) => out.push_str(&code.value),
            Node::Break(_) => out.push(' '),
            _ => {
                if let Some(children) = node.children() {
                    out.push_str(&plain_children(children));
                }
            }
        }
    }
    out
}

pub(super) fn heading_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            if pending_dash && !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else if character.is_whitespace() {
            pending_dash = true;
        }
    }
    slug.trim_matches('-').to_owned()
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
