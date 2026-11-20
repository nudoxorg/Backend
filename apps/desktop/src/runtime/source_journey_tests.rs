//! Source and references through a real owner, and what the page says once a
//! file changes on disk. Each declaration is found by a search, opened by the
//! coordinate the search gave, and read around an edit. The code a person sees
//! is the owner's: the compiler's own declaration extent, verified against the
//! indexed bytes, never a guess from the file.

#![allow(clippy::expect_used, clippy::panic)]

use crate::host::lease::DesktopHost;
use crate::model::pages::{PageValue, ReadFailure, SearchQuery, SourceOrigin, SourceView, SymbolRef};
use crate::runtime::actor::CancellationToken;
use crate::runtime::reads::{OutlineCache, PageReader as _, ReadContext, ReadRequest, SessionReader};
use backend_client::Session;
use backend_runtime::WorkspacePaths;
use std::path::PathBuf;

const LIBRARY: &str = "\
//! A crate with several shapes of declaration.

mod util;

/// Adds nothing, loudly.
#[inline]
pub fn helper() -> u32 {
    1111
}

/// A counter.
#[derive(Debug, Default)]
pub struct Counter {
    pub value: u32,
}

impl Counter {
    /// Bumps the counter by the helper.
    pub fn bump(&mut self) -> u32 {
        self.value += helper();
        self.value
    }
}

pub trait Shape {
    fn sides(&self) -> u32;
}

pub fn caller() -> u32 {
    helper() + helper()
}
";

const UTIL: &str = "pub fn from_util() -> u32 {\n    7\n}\n";

struct Journey {
    root: PathBuf,
    project: PathBuf,
    host: DesktopHost,
}

impl Journey {
    fn start() -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let root = crate::host::scratch_base().join(format!("nx-src-{}-{nonce}", std::process::id()));
        crate::host::private_dir(&root).expect("private scratch root");
        let project = root.join("fixture");
        std::fs::create_dir_all(project.join("src")).expect("project");
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"source-journey\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("manifest");
        std::fs::write(project.join("src/lib.rs"), LIBRARY).expect("source");
        std::fs::write(project.join("src/util.rs"), UTIL).expect("module");
        let paths = WorkspacePaths::discover(Some(project.clone()), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths");
        let host = DesktopHost::start_with_paths(paths).expect("the owner starts");
        let mut session = Session::connect(host.endpoint()).expect("session");
        session.index(project.to_str().expect("UTF-8 path")).expect("the owner indexes the project");
        Self { root, project, host }
    }

    fn try_read(&self, request: ReadRequest) -> Result<PageValue, ReadFailure> {
        let mut reader = SessionReader::connect(self.host.endpoint());
        let cancel = CancellationToken::new();
        let outlines = OutlineCache::default();
        let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines, progress: None };
        reader.read(&request, &context)
    }

    fn read(&self, request: ReadRequest) -> PageValue {
        self.try_read(request).unwrap_or_else(|failure| panic!("read failed: {failure:?}"))
    }

    /// The declaration as a person opens it: the coordinate a search gave.
    fn declaration(&self, name: &str) -> SymbolRef {
        let PageValue::Search(page) = self.read(ReadRequest::Search(SearchQuery::new(name, SearchQuery::DEFAULT_LIMIT).expect("query")))
        else { panic!("a search page") };
        let row = page.rows.iter().find(|row| row.decl.name.as_ref() == name).unwrap_or_else(|| panic!("{name} is found"));
        row.decl.coordinate.clone()
    }

    fn source(&self, symbol: &SymbolRef) -> SourceView {
        let PageValue::Source(view) = self.read(ReadRequest::Source(symbol.clone())) else { panic!("a source page") };
        view
    }

    /// The declaration lines the page names, cut from the page's own text.
    fn declared_text(view: &SourceView) -> String {
        let text = view.text.known().expect("the page carries the file's text");
        let lines = view.declaration.known().expect("the page names the declaration's lines");
        text.text().lines().skip(lines.first as usize - 1).take((lines.last - lines.first + 1) as usize).collect::<Vec<_>>().join("\n")
    }
}

impl Drop for Journey {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A local Rust declaration's page shows the code the compiler owns: the
/// whole file, with the declaration's own lines named, whatever shape the
/// declaration has (a function with doc and attribute, a derived struct, a
/// method in an `impl`, a trait, a function in another module's file).
#[test]
fn a_local_rust_declaration_shows_its_own_code() {
    let journey = Journey::start();
    let cases: [(&str, &str, &str); 6] = [
        (
            "helper",
            "src/lib.rs",
            "/// Adds nothing, loudly.\n#[inline]\npub fn helper() -> u32 {\n    1111\n}",
        ),
        (
            "Counter",
            "src/lib.rs",
            "/// A counter.\n#[derive(Debug, Default)]\npub struct Counter {\n    pub value: u32,\n}",
        ),
        (
            "bump",
            "src/lib.rs",
            "/// Bumps the counter by the helper.\n    pub fn bump(&mut self) -> u32 {\n        self.value += helper();\n        self.value\n    }",
        ),
        ("Shape", "src/lib.rs", "pub trait Shape {\n    fn sides(&self) -> u32;\n}"),
        ("caller", "src/lib.rs", "pub fn caller() -> u32 {\n    helper() + helper()\n}"),
        ("from_util", "src/util.rs", "pub fn from_util() -> u32 {\n    7\n}"),
    ];
    for (name, file, code) in cases {
        let view = journey.source(&journey.declaration(name));
        assert_eq!(view.file.known().map(|file| file.as_ref()), Some(file), "{name}: the declaration's file");
        let text = view.text.known().unwrap_or_else(|| panic!("{name}: source text is a gap: {:?}", view.text.gap()));
        assert_eq!(text.origin, SourceOrigin::LocalFile, "{name}");
        let on_disk = std::fs::read_to_string(journey.project.join(file)).expect("file");
        assert_eq!(text.text(), on_disk.as_str(), "{name}: the page shows the file's own bytes");
        assert!(
            Journey::declared_text(&view).ends_with(code),
            "{name}: the named lines are the declaration, got:\n{}",
            Journey::declared_text(&view)
        );
    }
}

/// The references the owner recorded for a declaration come with the code, and
/// the page still says what the owner does not certify.
#[test]
fn a_local_rust_declarations_uses_come_with_its_code() {
    let journey = Journey::start();
    let view = journey.source(&journey.declaration("helper"));
    assert!(view.text.known().is_some(), "source text: {:?}", view.text.gap());
    assert!(view.uses_elsewhere.len() >= 2, "the two calls in `caller` are recorded: {:?}", view.uses_elsewhere);
    assert!(view.references_gap.is_some(), "cross-file reference joins are said to be uncertified");
}

/// What a person opening a local declaration's source is told once the file
/// changes under the index: the owner compares the file with what it indexed
/// and refuses to describe a declaration from a file that moved on, then reads
/// again after a reindex.
#[test]
fn source_says_when_the_file_has_moved_on_and_reads_again_after_a_reindex() {
    let journey = Journey::start();
    let helper = journey.declaration("helper");
    let fresh = journey.source(&helper);
    assert!(Journey::declared_text(&fresh).contains("1111"), "{}", Journey::declared_text(&fresh));

    // The same bytes with a different value: same length, so no size check
    // can notice. The page gets the owner's refusal, not the old text and not
    // the new bytes.
    std::fs::write(journey.project.join("src/lib.rs"), LIBRARY.replace("1111", "2222")).expect("edit");
    match journey.try_read(ReadRequest::Source(helper.clone())) {
        Err(ReadFailure::Fault(error)) => assert!(
            error.message().contains("indexed source changed on disk; reindex and retry"),
            "the refusal says the file moved on and what to do: {}", error.message()
        ),
        other => panic!("an edited file must not be read as the indexed declaration: {other:?}"),
    }

    // Reindexing is the way forward, and the page then shows the new code.
    let mut session = Session::connect(journey.host.endpoint()).expect("session");
    session.index(journey.project.to_str().expect("UTF-8 path")).expect("the owner indexes the edited project");
    let again = journey.source(&journey.declaration("helper"));
    assert!(Journey::declared_text(&again).contains("2222"), "{}", Journey::declared_text(&again));
}

/// A folder with no Cargo manifest has no compiler authority: the owner
/// refuses its compile profile, records the project as failed, and still
/// answers from the structural lane. Those pages read, say which lane
/// answered, and are held to the same rule as every other page: the owner
/// compares the file with what it indexed and refuses once the file moved on.
#[test]
fn a_structural_only_declaration_reads_and_is_refused_once_its_file_changes() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root = crate::host::scratch_base().join(format!("nx-src-bare-{}-{nonce}", std::process::id()));
    crate::host::private_dir(&root).expect("private scratch root");
    let project = root.join("bare");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(project.join("src/lib.rs"), "pub fn bare() -> u32 {\n    3\n}\n").expect("source");
    let paths = WorkspacePaths::discover(Some(project.clone()), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths");
    let host = DesktopHost::start_with_paths(paths).expect("the owner starts");
    let mut session = Session::connect(host.endpoint()).expect("session");
    // No manifest: the only compile profile is refused, so the request fails
    // with the compiler's refusal while the structural lane is published.
    assert!(
        matches!(
            session.index(project.to_str().expect("UTF-8 path")),
            Err(backend_client::ClientError::CommandFailed(backend_library::CommandFailure::CompilerRefused { .. }))
        ),
        "a project without a manifest is refused by the compiler"
    );
    let journey = Journey { root, project, host };
    let bare = journey.declaration("bare");
    let PageValue::Symbol(page) = journey.read(ReadRequest::Symbol(bare.clone())) else { panic!("a symbol page") };
    assert!(!page.identity.semantic, "the structural lane answered, not a compiler");
    assert_eq!(page.identity.name.as_ref(), "bare");

    std::fs::write(journey.project.join("src/lib.rs"), "pub fn bare() -> u32 {\n    4\n}\n").expect("edit");
    match journey.try_read(ReadRequest::Symbol(bare)) {
        Err(ReadFailure::Fault(error)) => assert!(
            error.message().contains("indexed source changed on disk; reindex and retry"),
            "{}", error.message()
        ),
        other => panic!("an edited file must not be read as the indexed declaration: {other:?}"),
    }
}
