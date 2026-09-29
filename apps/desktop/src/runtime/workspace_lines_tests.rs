//! The lines your workspace uses a declaration on, read the way the page will
//! carry them (W-Open I3): the right line at the index's span, each file read
//! once, and nothing guessed.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::pages::{ByteSpan, DeclRef, FileSpan, Known, ReferenceScope};
use backend_library::SemanticLinkKind;
use std::cell::RefCell;

/// Given texts by path, counting each read.
#[derive(Default)]
struct Given {
    files: HashMap<PathBuf, Arc<str>>,
    asked: RefCell<Vec<PathBuf>>,
}

impl Files for Given {
    fn text(&self, path: &Path) -> Option<Arc<str>> {
        self.asked.borrow_mut().push(path.to_path_buf());
        self.files.get(path).cloned()
    }
}

const APP: &str = "fn main() {\n    let value = parse(text)?;\n    show(value);\n}\n";

fn reference(package: &str, file: &str, at: Option<usize>, relation: SemanticLinkKind, confidence: SemanticConfidence) -> ReferenceSite {
    ReferenceSite {
        site: DeclRef::from_label(&format!("{package}::{file}:1::main"), None, None, None).expect("a declaration"),
        relation,
        confidence,
        span: at.map_or_else(
            || Known::unknown(crate::model::pages::GapReason::NotServed, "no span"),
            |at| {
                let start = u32::try_from(at).expect("a small offset");
                Known::Known(FileSpan { file: Arc::from(file), bytes: ByteSpan::new(start, start + 5).expect("span") })
            },
        ),
        scope: ReferenceScope::Local,
    }
}

fn given() -> Given {
    Given { files: HashMap::from([(PathBuf::from("/work/app/src/main.rs"), Arc::from(APP))]), asked: RefCell::default() }
}

#[test]
fn a_use_is_the_line_at_its_span_with_its_place_and_how_sure_the_index_is() {
    let files = given();
    let at = APP.find("parse").expect("the call");
    let lines = read(
        &[reference("/work/app", "src/main.rs", Some(at), SemanticLinkKind::Calls, SemanticConfidence::Compiler)],
        &files,
    );
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert_eq!((line.line, &*line.text), (2, "let value = parse(text)?;"), "the line the span is on, trimmed");
    assert_eq!((&*line.package, &*line.file, &*line.path), ("app", "src/main.rs", "/work/app/src/main.rs"));
    assert_eq!((line.relation, line.resolution), (SemanticLinkKind::Calls, Resolution::Resolved));
    let by_name = read(
        &[reference("/work/app", "src/main.rs", Some(at), SemanticLinkKind::Calls, SemanticConfidence::Heuristic)],
        &files,
    );
    assert_eq!(by_name[0].resolution, Resolution::ByName, "a name match is marked, not passed off as resolved");
}

#[test]
fn nothing_is_guessed_a_use_that_cannot_be_read_is_left_out_and_each_file_is_read_once() {
    let files = given();
    let at = APP.find("show").expect("the call");
    let lines = read(
        &[
            reference("/work/app", "src/main.rs", Some(at), SemanticLinkKind::Calls, SemanticConfidence::Compiler),
            reference("/work/app", "src/main.rs", None, SemanticLinkKind::Calls, SemanticConfidence::Compiler),
            reference("/work/app", "src/gone.rs", Some(0), SemanticLinkKind::Calls, SemanticConfidence::Compiler),
            reference("/work/app", "src/main.rs", Some(APP.len() + 10), SemanticLinkKind::Calls, SemanticConfidence::Compiler),
            reference("pkg:cargo/serde@1.0.0", "src/lib.rs", Some(0), SemanticLinkKind::Calls, SemanticConfidence::Compiler),
            reference("/work/app", "src/main.rs", Some(at), SemanticLinkKind::MethodCall, SemanticConfidence::Compiler),
        ],
        &files,
    );
    assert_eq!(
        lines.iter().map(|line| (line.line, line.relation)).collect::<Vec<_>>(),
        [(3, SemanticLinkKind::Calls), (3, SemanticLinkKind::MethodCall)],
        "only the two readable local uses are lines, in the index's order"
    );
    let asked = files.asked.borrow();
    assert_eq!(
        asked.iter().filter(|path| path.ends_with("src/main.rs")).count(),
        1,
        "the file with three uses was read once: {asked:?}"
    );
    assert!(!asked.iter().any(|path| path.to_string_lossy().contains("serde")), "a registry package's uses are not yours to open");
}

#[test]
fn a_line_is_the_text_between_newlines_a_blank_one_is_no_line_and_a_long_one_is_cut() {
    let text = "a\n\n   \nlet x = 1;\n";
    assert_eq!(line_at(text, 0), Some((1, "a".to_owned())));
    assert_eq!(line_at(text, 2), None, "a blank line names nothing");
    assert_eq!(line_at(text, 5), None, "neither does one of spaces");
    assert_eq!(line_at(text, text.find('x').expect("x")), Some((4, "let x = 1;".to_owned())));
    assert_eq!(line_at(text, text.len() + 1), None, "past the end is nowhere");
    assert_eq!(line_at("é = 1", 1), None, "inside a character is nowhere");
    let long = "x".repeat(UseLine::MAX_TEXT + 100);
    assert_eq!(line_at(&long, 3).map(|(_, shown)| shown.chars().count()), Some(UseLine::MAX_TEXT), "cut, not dropped");
}

#[test]
fn lines_survive_the_page_they_ride_on_field_for_field() {
    let files = given();
    let at = APP.find("parse").expect("the call");
    let lines = read(
        &[reference("/work/app", "src/main.rs", Some(at), SemanticLinkKind::Calls, SemanticConfidence::Compiler)],
        &files,
    );
    let saved = serde_json::to_vec(&lines.to_vec()).expect("encode");
    let back: Vec<UseLine> = serde_json::from_slice(&saved).expect("decode");
    assert_eq!(back, lines.to_vec(), "the snapshot keeps every field of every line");
}

#[test]
fn files_come_from_disk_as_text_and_a_generated_giant_is_not_your_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/scratch").join(format!("i3-lines-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let small = dir.join("small.rs");
    std::fs::write(&small, APP).expect("write");
    assert_eq!(OnDisk.text(&small).as_deref(), Some(APP), "a file is its text");
    let giant = dir.join("giant.rs");
    std::fs::write(&giant, vec![b'x'; usize::try_from(LARGEST_FILE).expect("a size") + 1]).expect("write");
    assert!(OnDisk.text(&giant).is_none(), "over the largest file read, it is left alone");
    let binary = dir.join("binary.bin");
    std::fs::write(&binary, [0xff, 0xfe, 0x00]).expect("write");
    assert!(OnDisk.text(&binary).is_none(), "and a file that is not text has no line");
    assert!(OnDisk.text(&dir.join("gone.rs")).is_none(), "a file that is not there has none either");
    let _ = std::fs::remove_dir_all(dir);
}
