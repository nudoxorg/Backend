//! In your workspace: the places the index says your packages name the
//! declaration, with each place's real line read from the local file at the
//! span (off the UI thread, once, cached). A place whose file cannot be read
//! is left out: a line is never guessed.
//!
//! The index gives a relation and a byte span; this reads the line the span
//! is on. `facet::anatomy::symbol::derive::uses` reads what the line does.

use super::place;
use crate::model::pages::{PackageRef, SymbolPage, SymbolRef};
use backend_library::SemanticLinkKind;
use facet::anatomy::symbol::derive::uses::{Reader, Rel, Site};
use gpui::{App, Global};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// What the service holds for one declaration.
enum Entry {
    /// The files are being read.
    Reading,
    /// Read: the places, with their lines.
    Ready(Arc<[Site]>),
}

#[derive(Default)]
struct Service {
    entries: HashMap<(SymbolRef, usize), Entry>,
    order: Vec<(SymbolRef, usize)>,
}

impl Global for Service {}

/// What a page gets when it asks for its places.
pub(super) enum Reading {
    /// The lines are being read: the page says so.
    Reading,
    /// Read.
    Ready(Arc<[Site]>),
}

const fn rel(kind: SemanticLinkKind) -> Rel {
    match kind {
        SemanticLinkKind::Calls => Rel::Calls,
        SemanticLinkKind::MethodCall => Rel::MethodCall,
        SemanticLinkKind::TypeReference => Rel::TypeReference,
        SemanticLinkKind::Reads => Rel::Reads,
        SemanticLinkKind::Writes => Rel::Writes,
        SemanticLinkKind::Imports => Rel::Imports,
        SemanticLinkKind::Implements => Rel::Implements,
        SemanticLinkKind::Overrides => Rel::Overrides,
        SemanticLinkKind::Reexports => Rel::Reexports,
        SemanticLinkKind::Inherits => Rel::Inherits,
        SemanticLinkKind::Documents => Rel::Documents,
    }
}

/// One use before its line is read.
struct Pending {
    package: String,
    file: String,
    path: PathBuf,
    start: usize,
    rel: Rel,
    exact: bool,
}

fn pending(page: &SymbolPage) -> Vec<Pending> {
    let Some(references) = page.references.known() else { return Vec::new() };
    let mut out = Vec::new();
    for site in references.iter() {
        let Some(span) = site.span.known() else { continue };
        let Some(package) = site.site.coordinate.package() else { continue };
        // Your packages: the local ones. A registry package's uses are not
        // yours to open.
        if !package.is_local() {
            continue;
        }
        let Some(root) = place::root(&package) else { continue };
        out.push(Pending {
            package: package.display_name().to_owned(),
            file: span.file.to_string(),
            path: root.join(span.file.as_ref()),
            start: span.bytes.start as usize,
            rel: rel(site.relation),
            exact: matches!(site.confidence, backend_library::SemanticConfidence::Compiler | backend_library::SemanticConfidence::Imported | backend_library::SemanticConfidence::Indexed),
        });
    }
    out
}

/// The text of a file: given by a test, else read.
fn read_file(path: &PathBuf) -> Option<Arc<str>> {
    #[cfg(test)]
    if let Some(text) = tests::given(path) {
        return Some(text);
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok().map(Arc::from)
}

/// The line a byte offset is on: its one-based number and its trimmed text.
fn line_at(text: &str, at: usize) -> Option<(u32, String)> {
    if at > text.len() || !text.is_char_boundary(at) {
        return None;
    }
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let number = text[..line_start].bytes().filter(|b| *b == b'\n').count() + 1;
    let line = text[line_start..line_end].trim();
    let shown: String = line.chars().take(240).collect();
    Some((u32::try_from(number).ok()?, shown))
}

/// Reads every place's line, file by file.
fn read_all(pending: Vec<Pending>) -> Vec<Site> {
    let mut files: HashMap<PathBuf, Option<Arc<str>>> = HashMap::new();
    let mut out = Vec::new();
    for place in pending {
        let text = files.entry(place.path.clone()).or_insert_with(|| read_file(&place.path)).clone();
        let Some(text) = text else { continue };
        let Some((line, shown)) = line_at(&text, place.start) else { continue };
        if shown.is_empty() {
            continue;
        }
        out.push(Site { package: place.package, file: place.file, path: place.path.to_string_lossy().into_owned(), line, text: shown, rel: place.rel, exact: place.exact });
    }
    out
}

/// The places for `page`'s declaration. The first ask starts the read (off
/// the UI thread) and answers [`Reading::Reading`]; the windows redraw when
/// it lands.
pub(super) fn ask(symbol: &SymbolRef, page: &SymbolPage, cx: &mut App) -> Reading {
    let count = page.references.known().map_or(0, |references| references.len());
    let key = (symbol.clone(), count);
    if let Some(entry) = cx.default_global::<Service>().entries.get(&key) {
        return match entry {
            Entry::Reading => Reading::Reading,
            Entry::Ready(sites) => Reading::Ready(Arc::clone(sites)),
        };
    }
    let pending = pending(page);
    let service = cx.default_global::<Service>();
    service.order.push(key.clone());
    if service.order.len() > 24 {
        let old = service.order.remove(0);
        service.entries.remove(&old);
    }
    #[cfg(test)]
    {
        let sites: Arc<[Site]> = Arc::from(read_all(pending));
        cx.default_global::<Service>().entries.insert(key, Entry::Ready(Arc::clone(&sites)));
        Reading::Ready(sites)
    }
    #[cfg(not(test))]
    {
        service.entries.insert(key.clone(), Entry::Reading);
        let work = cx.background_executor().spawn(async move { read_all(pending) });
        cx.spawn(async move |cx| {
            let sites: Arc<[Site]> = Arc::from(work.await);
            let _ = cx.update(|cx| {
                cx.default_global::<Service>().entries.insert(key, Entry::Ready(sites));
                cx.refresh_windows();
            });
        })
        .detach();
        Reading::Reading
    }
}

/// The reader of a page's lines: what its symbol is, its members and cases.
pub(super) fn reader_of(view: &facet::anatomy::symbol::View) -> Reader {
    use facet::anatomy::symbol::view::Shape;
    let mut members = Vec::new();
    for group in &view.verbs {
        members.extend(group.rows.iter().map(|row| (row.name.clone(), group.verb)));
    }
    let mut cases = Vec::new();
    match &view.shape {
        Some(Shape::OneOf(shape)) => cases.extend(shape.iter().map(|case| case.name.clone())),
        Some(Shape::Holds { fields, .. }) => members.extend(fields.iter().map(|field| (field.name.clone(), facet::anatomy::symbol::view::Do::Reads))),
        _ => {}
    }
    Reader { name: view.head.name.clone(), kind: view.head.kind, members, cases, generic: !view.generics.is_empty() }
}

/// The package a coordinate belongs to, for the header's path.
#[allow(dead_code)]
pub(super) fn package_of(symbol: &SymbolRef) -> Option<PackageRef> {
    symbol.package()
}

#[cfg(test)]
pub(super) mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    thread_local! {
        static GIVEN: RefCell<HashMap<PathBuf, Arc<str>>> = RefCell::new(HashMap::new());
    }

    /// Holds the given files for one test: dropping it forgets them.
    pub(in crate::shell) struct Given;

    impl Drop for Given {
        fn drop(&mut self) {
            GIVEN.with(|given| given.borrow_mut().clear());
        }
    }

    /// The files by absolute path.
    pub(in crate::shell) fn give(files: HashMap<PathBuf, Arc<str>>) -> Given {
        GIVEN.with(|given| *given.borrow_mut() = files);
        Given
    }

    pub(super) fn given(path: &Path) -> Option<Arc<str>> {
        GIVEN.with(|given| given.borrow().get(path).cloned())
    }

    #[test]
    fn a_span_is_the_line_it_is_on() {
        let text = "fn a() {}\n    let x: Metadata = serde_json::from_str(j)?;\nfn b() {}\n";
        let at = text.find("from_str").expect("the call");
        assert_eq!(super::line_at(text, at), Some((2, "let x: Metadata = serde_json::from_str(j)?;".to_owned())));
        assert_eq!(super::line_at(text, 0), Some((1, "fn a() {}".to_owned())));
        assert_eq!(super::line_at(text, text.len() + 5), None);
    }
}
