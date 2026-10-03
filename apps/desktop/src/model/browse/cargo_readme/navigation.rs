//! Complete authored endpoints, prepared off the UI thread. Rows refer to a
//! checked byte arena; repeated reference links share one destination.

use super::{CargoReadmeDestination, CargoReadmeFocus, CargoReadmeLink, endpoint_digest};
use crate::model::document_identity::{DocumentIdentity, DocumentPaintIdentity};
use crate::model::local_package::{ReadmeHeading, readme_external_address};
use crate::navigation::CargoReadmeLinkAddress;
use backend_library::{CargoPackageReadmeLinkTargetV1, CargoPackageReadmeOriginV1};
use markdown::mdast::Node;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A failed index is never published as a successfully shortened document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparationError {
    Cancelled,
    Parse,
    Capacity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Text {
    start: u32,
    len: u32,
    previous: Option<u32>,
}

#[derive(Default, Debug, Eq, PartialEq)]
struct Arena {
    bytes: Vec<u8>,
    texts: Vec<Text>,
}

impl Arena {
    fn text(&self, id: u32) -> &str {
        self.texts
            .get(id as usize)
            .and_then(|text| {
                let start = text.start as usize;
                self.bytes.get(start..start + text.len as usize)
            })
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }
}

struct Interner {
    arena: Arena,
    heads: HashMap<[u8; 16], u32>,
    // Decoding, heading lowercase expansion, and duplicate suffixes are
    // bounded by actual input bytes. This is not an endpoint-count quota.
    byte_limit: usize,
}

impl Interner {
    fn new(source_bytes: usize) -> Result<Self, PreparationError> {
        let byte_limit = source_bytes
            .checked_mul(16)
            .and_then(|value| value.checked_add(512))
            .ok_or(PreparationError::Capacity)?;
        Ok(Self {
            arena: Arena::default(),
            heads: HashMap::new(),
            byte_limit,
        })
    }

    fn intern(&mut self, value: &str) -> Result<u32, PreparationError> {
        let hash = blake3::hash(value.as_bytes());
        let mut key = [0; 16];
        key.copy_from_slice(&hash.as_bytes()[..16]);
        let head = self.heads.get(&key).copied();
        let mut at = head;
        while let Some(id) = at {
            if self.arena.text(id) == value {
                return Ok(id);
            }
            at = self
                .arena
                .texts
                .get(id as usize)
                .and_then(|text| text.previous);
        }
        let end = self
            .arena
            .bytes
            .len()
            .checked_add(value.len())
            .ok_or(PreparationError::Capacity)?;
        if end > self.byte_limit {
            return Err(PreparationError::Capacity);
        }
        let start = narrow(self.arena.bytes.len())?;
        let len = narrow(value.len())?;
        let id = narrow(self.arena.texts.len())?;
        reserve(&mut self.arena.bytes, value.len())?;
        reserve(&mut self.arena.texts, 1)?;
        self.heads
            .try_reserve(1)
            .map_err(|_| PreparationError::Capacity)?;
        self.arena.bytes.extend_from_slice(value.as_bytes());
        self.arena.texts.push(Text {
            start,
            len,
            previous: head,
        });
        self.heads.insert(key, id);
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Destination {
    External,
    AnchorSlug(u32),
    Anchor(u32),
    Source,
    Unavailable(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Href {
    text: u32,
    destination: Destination,
    focus: [u8; 32],
    count: u32,
    focus_start: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Link {
    label: u32,
    href: u32,
    occurrence: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Heading {
    title: u32,
    slug: u32,
    offset: u32,
    level: u8,
    focus: [u8; 32],
}

/// The immutable full index has no owner capability. Its projections still
/// require the current origin, attachment, and selected resource lease.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct NavigationIndex {
    arena: Arena,
    links: Vec<Link>,
    hrefs: Vec<Href>,
    headings: Vec<Heading>,
    href_order: Vec<u32>,
    href_focus_order: Vec<u32>,
    heading_focus_order: Vec<u32>,
    focus_rows: Vec<u32>,
}

impl NavigationIndex {
    pub(super) fn prepare(
        origin: &CargoPackageReadmeOriginV1,
        identity: DocumentIdentity,
        source: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, PreparationError> {
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        if source.len() > backend_library::MAX_CARGO_PACKAGE_README_BYTES {
            return Err(PreparationError::Capacity);
        }
        let ast = markdown::to_mdast(source, &markdown::ParseOptions::gfm())
            .map_err(|_| PreparationError::Parse)?;
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let mut builder = Builder::new(origin, identity, source.len())?;
        let mut definitions = HashMap::new();
        walk(&ast, source.len(), cancelled, |node| {
            if let Node::Definition(definition) = node {
                if !definitions.contains_key(definition.identifier.as_str()) {
                    definitions
                        .try_reserve(1)
                        .map_err(|_| PreparationError::Capacity)?;
                    let href = builder.strings.intern(&definition.url)?;
                    definitions.insert(definition.identifier.as_str(), href);
                }
            }
            Ok(())
        })?;
        walk(&ast, source.len(), cancelled, |node| {
            match node {
                Node::Link(link) => {
                    let href = builder.strings.intern(&link.url)?;
                    builder.link(&link.children, href, cancelled)?;
                }
                Node::LinkReference(link) => {
                    if let Some(href) = definitions.get(link.identifier.as_str()) {
                        builder.link(&link.children, *href, cancelled)?;
                    }
                }
                Node::Heading(heading) => builder.heading(heading, cancelled)?,
                _ => {}
            }
            Ok(())
        })?;
        builder.finish(cancelled)
    }

    pub(super) fn link_count(&self) -> usize {
        self.links.len()
    }
    pub(super) fn heading_count(&self) -> usize {
        self.headings.len()
    }

    pub(super) fn link(
        &self,
        index: usize,
        origin: &CargoPackageReadmeOriginV1,
        paint: Option<DocumentPaintIdentity>,
    ) -> Option<CargoReadmeLink> {
        let link = self.links.get(index)?;
        let href = self.hrefs.get(link.href as usize)?;
        Some(CargoReadmeLink {
            id: link_id(&href.focus, link.occurrence),
            label: Arc::from(self.arena.text(link.label)),
            href: Arc::from(self.arena.text(href.text)),
            destination: self.project(href, origin, paint),
        })
    }

    pub(super) fn heading(
        &self,
        index: usize,
        paint: Option<DocumentPaintIdentity>,
    ) -> Option<ReadmeHeading> {
        let heading = self.headings.get(index)?;
        Some(ReadmeHeading {
            slug: Arc::from(self.arena.text(heading.slug)),
            element_id: paint.map_or_else(
                || Arc::from(format!("readme-heading-{}", heading.offset)),
                |identity| identity.heading_id(heading.offset as usize),
            ),
            title: Arc::from(self.arena.text(heading.title)),
            level: heading.level,
        })
    }

    pub(super) fn heading_id(&self, index: usize) -> Option<Arc<str>> {
        self.headings
            .get(index)
            .map(|heading| heading_id(&heading.focus))
    }

    pub(super) fn destination(
        &self,
        href: &str,
        origin: &CargoPackageReadmeOriginV1,
        paint: Option<DocumentPaintIdentity>,
    ) -> CargoReadmeDestination {
        self.find_href(href).map_or(
            CargoReadmeDestination::Unavailable(
                "This destination is not an authored link in the current README.",
            ),
            |found| self.project(found, origin, paint),
        )
    }

    pub(super) fn inline_focus_id(&self, href: &str) -> Option<Arc<str>> {
        let found = self.find_href(href)?;
        (!matches!(found.destination, Destination::Unavailable(_)))
            .then(|| link_id(&found.focus, 0))
    }

    pub(super) fn restore_focus(&self, id: &str) -> Option<CargoReadmeFocus> {
        if let Some(tail) = id.strip_prefix("cargo-readme-link-") {
            let (digest, occurrence) = tail.split_once('-')?;
            let digest = parse_digest(digest)?;
            let occurrence = u32::from_str_radix(occurrence, 16).ok()?;
            let at = self
                .href_focus_order
                .binary_search_by_key(&digest, |index| self.hrefs[*index as usize].focus)
                .ok()?;
            let index = unique_focus(&self.href_focus_order, at, |index| {
                self.hrefs[index as usize].focus
            })?;
            let href = self.hrefs.get(index as usize)?;
            if occurrence >= href.count
                || matches!(href.destination, Destination::Unavailable(_))
                || link_id(&href.focus, occurrence).as_ref() != id
            {
                return None;
            }
            let row = self
                .focus_rows
                .get((href.focus_start + occurrence) as usize)?;
            return Some(CargoReadmeFocus::Link(*row as usize));
        }
        let digest = parse_digest(id.strip_prefix("cargo-readme-heading-")?)?;
        let at = self
            .heading_focus_order
            .binary_search_by_key(&digest, |index| self.headings[*index as usize].focus)
            .ok()?;
        let index = unique_focus(&self.heading_focus_order, at, |index| {
            self.headings[index as usize].focus
        })?;
        (heading_id(&self.headings[index as usize].focus).as_ref() == id)
            .then_some(CargoReadmeFocus::Heading(index as usize))
    }

    /// Actual retained vector capacities, excluding the separately held
    /// source/origin. The result mailbox can account for this payload.
    pub(super) fn storage_bytes(&self) -> usize {
        self.arena.bytes.capacity()
            + self.arena.texts.capacity() * std::mem::size_of::<Text>()
            + self.links.capacity() * std::mem::size_of::<Link>()
            + self.hrefs.capacity() * std::mem::size_of::<Href>()
            + self.headings.capacity() * std::mem::size_of::<Heading>()
            + (self.href_order.capacity()
                + self.href_focus_order.capacity()
                + self.heading_focus_order.capacity()
                + self.focus_rows.capacity())
                * std::mem::size_of::<u32>()
    }

    fn find_href(&self, href: &str) -> Option<&Href> {
        let at = self
            .href_order
            .binary_search_by(|index| self.arena.text(self.hrefs[*index as usize].text).cmp(href))
            .ok()?;
        self.hrefs.get(*self.href_order.get(at)? as usize)
    }

    fn project(
        &self,
        href: &Href,
        origin: &CargoPackageReadmeOriginV1,
        paint: Option<DocumentPaintIdentity>,
    ) -> CargoReadmeDestination {
        match href.destination {
            Destination::External => {
                CargoReadmeDestination::External(Arc::from(self.arena.text(href.text)))
            }
            Destination::Anchor(index) => self.heading(index as usize, paint).map_or(
                CargoReadmeDestination::Unavailable(
                    "This heading is unavailable in the current README.",
                ),
                CargoReadmeDestination::Anchor,
            ),
            Destination::Source => {
                CargoReadmeLinkAddress::new(origin.clone(), self.arena.text(href.text)).map_or(
                    CargoReadmeDestination::Unavailable(
                        "This README link has no supported relative address.",
                    ),
                    CargoReadmeDestination::Source,
                )
            }
            Destination::Unavailable(reason) => CargoReadmeDestination::Unavailable(reason),
            Destination::AnchorSlug(_) => CargoReadmeDestination::Unavailable(
                "This heading could not be resolved in the current README.",
            ),
        }
    }
}

struct Builder<'a> {
    origin: &'a CargoPackageReadmeOriginV1,
    origin_key: DocumentIdentity,
    strings: Interner,
    links: Vec<Link>,
    hrefs: Vec<Href>,
    headings: Vec<Heading>,
    href_by_text: HashMap<u32, u32>,
    slug_counts: HashMap<u32, u32>,
    used_slugs: HashSet<u32>,
}

impl<'a> Builder<'a> {
    fn new(
        origin: &'a CargoPackageReadmeOriginV1,
        identity: DocumentIdentity,
        source_bytes: usize,
    ) -> Result<Self, PreparationError> {
        Ok(Self {
            origin,
            origin_key: identity,
            strings: Interner::new(source_bytes)?,
            links: Vec::new(),
            hrefs: Vec::new(),
            headings: Vec::new(),
            href_by_text: HashMap::new(),
            slug_counts: HashMap::new(),
            used_slugs: HashSet::new(),
        })
    }

    fn link(
        &mut self,
        children: &[Node],
        text: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), PreparationError> {
        let href = if let Some(index) = self.href_by_text.get(&text) {
            *index
        } else {
            let authored = self.strings.arena.text(text);
            let focus = *endpoint_digest(&self.origin_key, "link", authored).as_bytes();
            let destination = if readme_external_address(authored).is_some() {
                Destination::External
            } else {
                match self.origin.resolve_relative_href(authored) {
                    Ok(CargoPackageReadmeLinkTargetV1::Anchor { fragment }) => {
                        let slug = slug(&fragment)?;
                        Destination::AnchorSlug(self.strings.intern(&slug)?)
                    }
                    Ok(CargoPackageReadmeLinkTargetV1::File { .. }) => Destination::Source,
                    Err(_) => Destination::Unavailable(
                        "This README link is outside its supported package or workspace scope.",
                    ),
                }
            };
            let index = narrow(self.hrefs.len())?;
            reserve(&mut self.hrefs, 1)?;
            self.href_by_text
                .try_reserve(1)
                .map_err(|_| PreparationError::Capacity)?;
            self.hrefs.push(Href {
                text,
                destination,
                focus,
                count: 0,
                focus_start: 0,
            });
            self.href_by_text.insert(text, index);
            index
        };
        let label = plain(children, cancelled)?;
        let label = self.strings.intern(bounded_label(&label))?;
        let record = &mut self.hrefs[href as usize];
        let occurrence = record.count;
        record.count = record
            .count
            .checked_add(1)
            .ok_or(PreparationError::Capacity)?;
        reserve(&mut self.links, 1)?;
        self.links.push(Link {
            label,
            href,
            occurrence,
        });
        Ok(())
    }

    fn heading(
        &mut self,
        heading: &markdown::mdast::Heading,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), PreparationError> {
        let title = plain(&heading.children, cancelled)?;
        let base = slug(&title)?;
        let base_id = self.strings.intern(&base)?;
        let mut occurrence = self.slug_counts.get(&base_id).copied().unwrap_or_default();
        let slug_id = loop {
            if cancelled() {
                return Err(PreparationError::Cancelled);
            }
            let candidate = if occurrence == 0 {
                base_id
            } else {
                let mut candidate = String::new();
                candidate
                    .try_reserve(
                        base.len()
                            .checked_add(12)
                            .ok_or(PreparationError::Capacity)?,
                    )
                    .map_err(|_| PreparationError::Capacity)?;
                use std::fmt::Write as _;
                write!(&mut candidate, "{base}-{occurrence}")
                    .map_err(|_| PreparationError::Capacity)?;
                self.strings.intern(&candidate)?
            };
            occurrence = occurrence
                .checked_add(1)
                .ok_or(PreparationError::Capacity)?;
            if !self.used_slugs.contains(&candidate) {
                break candidate;
            }
        };
        self.slug_counts
            .try_reserve(1)
            .map_err(|_| PreparationError::Capacity)?;
        self.used_slugs
            .try_reserve(1)
            .map_err(|_| PreparationError::Capacity)?;
        self.slug_counts.insert(base_id, occurrence);
        self.used_slugs.insert(slug_id);
        let focus = *endpoint_digest(
            &self.origin_key,
            "heading",
            self.strings.arena.text(slug_id),
        )
        .as_bytes();
        let title = self.strings.intern(bounded_label(&title))?;
        let offset = narrow(
            heading
                .position
                .as_ref()
                .map_or(0, |position| position.start.offset),
        )?;
        reserve(&mut self.headings, 1)?;
        self.headings.push(Heading {
            title,
            slug: slug_id,
            offset,
            level: heading.depth,
            focus,
        });
        Ok(())
    }

    fn finish(mut self, cancelled: &dyn Fn() -> bool) -> Result<NavigationIndex, PreparationError> {
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let arena = &self.strings.arena;
        let mut heading_order = order(self.headings.len())?;
        heading_order.sort_unstable_by(|a, b| {
            arena
                .text(self.headings[*a as usize].slug)
                .cmp(arena.text(self.headings[*b as usize].slug))
        });
        for href in &mut self.hrefs {
            if cancelled() {
                return Err(PreparationError::Cancelled);
            }
            if let Destination::AnchorSlug(slug) = href.destination {
                href.destination = heading_order
                    .binary_search_by(|index| {
                        arena
                            .text(self.headings[*index as usize].slug)
                            .cmp(arena.text(slug))
                    })
                    .map_or(
                        Destination::Unavailable("This heading is not in the current README."),
                        |at| Destination::Anchor(heading_order[at]),
                    );
            }
        }
        let mut href_order = order(self.hrefs.len())?;
        href_order.sort_unstable_by(|a, b| {
            arena
                .text(self.hrefs[*a as usize].text)
                .cmp(arena.text(self.hrefs[*b as usize].text))
        });
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let mut href_focus_order = order(self.hrefs.len())?;
        href_focus_order.sort_unstable_by_key(|index| self.hrefs[*index as usize].focus);
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let mut heading_focus_order = order(self.headings.len())?;
        heading_focus_order.sort_unstable_by_key(|index| self.headings[*index as usize].focus);
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let mut focus_rows = Vec::new();
        reserve(&mut focus_rows, self.links.len())?;
        focus_rows.resize(self.links.len(), 0);
        let mut start = 0_u32;
        for href in &mut self.hrefs {
            href.focus_start = start;
            start = start
                .checked_add(href.count)
                .ok_or(PreparationError::Capacity)?;
        }
        for (index, link) in self.links.iter().enumerate() {
            if cancelled() {
                return Err(PreparationError::Cancelled);
            }
            focus_rows[(self.hrefs[link.href as usize].focus_start + link.occurrence) as usize] =
                narrow(index)?;
        }
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        Ok(NavigationIndex {
            arena: self.strings.arena,
            links: self.links,
            hrefs: self.hrefs,
            headings: self.headings,
            href_order,
            href_focus_order,
            heading_focus_order,
            focus_rows,
        })
    }
}

fn walk<'a>(
    root: &'a Node,
    source_bytes: usize,
    cancelled: &dyn Fn() -> bool,
    mut visit: impl FnMut(&'a Node) -> Result<(), PreparationError>,
) -> Result<(), PreparationError> {
    let maximum = source_bytes
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or(PreparationError::Capacity)?;
    let mut stack = Vec::new();
    reserve(&mut stack, 1)?;
    stack.push(root);
    let mut visited = 0_usize;
    while let Some(node) = stack.pop() {
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        visited = visited.checked_add(1).ok_or(PreparationError::Capacity)?;
        if visited > maximum {
            return Err(PreparationError::Capacity);
        }
        visit(node)?;
        if let Some(children) = node.children() {
            reserve(&mut stack, children.len())?;
            stack.extend(children.iter().rev());
        }
    }
    Ok(())
}

fn plain(children: &[Node], cancelled: &dyn Fn() -> bool) -> Result<String, PreparationError> {
    let mut result = String::new();
    let mut stack = Vec::new();
    reserve(&mut stack, children.len())?;
    stack.extend(children.iter().rev());
    while let Some(node) = stack.pop() {
        if cancelled() {
            return Err(PreparationError::Cancelled);
        }
        let text = match node {
            Node::Text(text) => Some(text.value.as_str()),
            Node::InlineCode(code) => Some(code.value.as_str()),
            Node::Code(code) => Some(code.value.as_str()),
            Node::Image(image) => Some(image.alt.as_str()),
            Node::ImageReference(image) => Some(image.alt.as_str()),
            Node::Break(_) => Some(" "),
            _ => None,
        };
        if let Some(text) = text {
            result
                .try_reserve(text.len())
                .map_err(|_| PreparationError::Capacity)?;
            result.push_str(text);
        } else if let Some(children) = node.children() {
            reserve(&mut stack, children.len())?;
            stack.extend(children.iter().rev());
        }
    }
    Ok(result)
}

fn slug(title: &str) -> Result<String, PreparationError> {
    let mut result = String::new();
    result
        .try_reserve(
            title
                .len()
                .checked_mul(3)
                .ok_or(PreparationError::Capacity)?,
        )
        .map_err(|_| PreparationError::Capacity)?;
    let mut pending_dash = false;
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            if pending_dash && !result.is_empty() && !result.ends_with('-') {
                result.push('-');
            }
            pending_dash = false;
            result.push(character);
        } else if character.is_whitespace() {
            pending_dash = true;
        }
    }
    let end = result.trim_end_matches('-').len();
    result.truncate(end);
    let start = result.len() - result.trim_start_matches('-').len();
    result.drain(..start);
    Ok(result)
}

fn bounded_label(value: &str) -> &str {
    let mut end = value.len().min(4 * 1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), PreparationError> {
    values
        .try_reserve(additional)
        .map_err(|_| PreparationError::Capacity)
}

fn narrow(value: usize) -> Result<u32, PreparationError> {
    u32::try_from(value).map_err(|_| PreparationError::Capacity)
}

fn order(count: usize) -> Result<Vec<u32>, PreparationError> {
    narrow(count)?;
    let mut order = Vec::new();
    reserve(&mut order, count)?;
    order.extend((0..count).map(|index| index as u32));
    Ok(order)
}

fn link_id(digest: &[u8; 32], occurrence: u32) -> Arc<str> {
    Arc::from(format!(
        "cargo-readme-link-{}-{occurrence:x}",
        blake3::Hash::from_bytes(*digest).to_hex()
    ))
}

fn heading_id(digest: &[u8; 32]) -> Arc<str> {
    Arc::from(format!(
        "cargo-readme-heading-{}",
        blake3::Hash::from_bytes(*digest).to_hex()
    ))
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        fn hex(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            }
        }
        digest[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
    }
    Some(digest)
}

fn unique_focus(order: &[u32], at: usize, digest: impl Fn(u32) -> [u8; 32]) -> Option<u32> {
    let index = *order.get(at)?;
    let key = digest(index);
    if at
        .checked_sub(1)
        .and_then(|at| order.get(at))
        .is_some_and(|index| digest(*index) == key)
        || order.get(at + 1).is_some_and(|index| digest(*index) == key)
    {
        return None;
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn checked_capacity_refusal_has_no_silent_partial_result() {
        assert!(matches!(
            Interner::new(usize::MAX),
            Err(PreparationError::Capacity)
        ));
        let mut strings = Interner::new(1).expect("small checked arena");
        assert_eq!(
            strings.intern(&"x".repeat(529)),
            Err(PreparationError::Capacity)
        );
        assert!(strings.arena.bytes.is_empty());
        assert!(strings.arena.texts.is_empty());
        assert!(strings.heads.is_empty());
    }

    #[test]
    fn collision_lookup_compares_exact_text_and_ambiguous_focus_is_refused() {
        let mut strings = Interner::new(16).expect("arena");
        let first = strings.intern("first").expect("first text");
        let second = strings.intern("second").expect("second text");
        assert_ne!(first, second);
        assert_eq!(strings.intern("first"), Ok(first));
        assert_eq!(unique_focus(&[0, 1], 0, |_| [7; 32]), None);
        assert_eq!(unique_focus(&[0, 1], 1, |_| [7; 32]), None);
        assert_eq!(unique_focus(&[0, 1], 1, |index| [index as u8; 32]), Some(1));
    }
}
