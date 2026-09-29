//! The launch snapshot file (W-Open I2): what is saved reads back equal,
//! field for field, and a file that does not check out is ignored whole.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::pages::{DocFragment, RowKey};
use backend_library::DeclarationKind;
use crate::runtime::actor::CancellationToken;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, dossier, page, symbol};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nx-snap-{tag}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

pub(crate) fn served(tag: &str, sequence: u64) -> VersionedRoot {
    VersionedRoot::from_revision(
        1,
        backend_library::Cursor::at(
            backend_library::view_state_root(&[("snapshot".to_owned(), tag.to_owned())]),
            sequence,
        ),
        0,
    )
}

fn row_key(byte: u8) -> RowKey {
    serde_json::from_str(&format!("[{}]", vec![byte.to_string(); 32].join(","))).expect("row key")
}

fn fixture(request: &ReadRequest) -> PageValue {
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext {
        worker: 0,
        cancel: &cancel,
        outlines: &outlines,
    };
    Fixture.read(request, &context).expect("fixture page")
}

/// The fixture page with every field a real page fills that the fixture
/// leaves empty: row keys, a doc link, and a typed kind on a reference.
fn rich_page(name: &str) -> SymbolPage {
    let mut page = page(name);
    page.identity.key = Some(row_key(7));
    page.identity.path = Some(Arc::from("src/glyph.rs"));
    page.identity.line = Some(138);
    page.docs = Arc::from([
        DocFragment::Text(Arc::from("Reads ")),
        DocFragment::Link {
            label: Arc::from("Typed"),
            target: row_key(9),
            coordinate: Some(symbol("Typed")),
        },
        DocFragment::Code(Arc::from("RelationLabel::Typed")),
        DocFragment::Break,
    ]);
    assert_eq!(page.identity.kind, Some(DeclarationKind::Enum));
    page
}

fn pages(name: &str) -> Vec<(PageKey, Kept, PageValue)> {
    let id = symbol(name);
    let package = crate::model::pages::PackageRef::parse(PACKAGE).expect("package");
    let symbol_page = rich_page(name);
    let PageValue::Source(source) = fixture(&ReadRequest::Source(id.clone())) else {
        panic!("a source view")
    };
    let PageValue::Orbit(orbit) = fixture(&ReadRequest::Orbit) else {
        panic!("the orbit model")
    };
    let dossier = dossier();
    vec![
        (
            PageKey::Symbol(id.clone()),
            Kept::Symbol(Arc::new(symbol_page.clone())),
            PageValue::Symbol(symbol_page),
        ),
        (
            PageKey::Source(id),
            Kept::Source(Arc::new(source.clone())),
            PageValue::Source(source),
        ),
        (
            PageKey::Package(package),
            Kept::Package(Arc::new(dossier.clone())),
            PageValue::Package(dossier),
        ),
        (PageKey::Orbit, Kept::Orbit(Arc::new(orbit.clone())), PageValue::Orbit(orbit)),
    ]
}

fn write(dir: &Path, root: VersionedRoot, pages: &[(PageKey, Kept, PageValue)]) -> SnapshotFile {
    let file = SnapshotFile::in_data(dir);
    let kept = pages
        .iter()
        .map(|(key, kept, _)| (key.clone(), kept.clone()))
        .collect::<Vec<_>>();
    assert!(file.write(root, &kept).expect("write") > 0);
    file
}

#[test]
fn every_saved_page_reads_back_equal_field_for_field() {
    let dir = scratch("equal");
    let root = served("equal", 3);
    let saved = pages("RelationLabel");
    let file = write(&dir, root, &saved);
    let wanted = saved.iter().map(|(key, _, _)| key.clone()).collect::<Vec<_>>();
    let seed = file.read(&wanted).expect("the snapshot reads back");
    assert!(seed.root.serves(root), "the root it was read at is the root it names");
    assert!(!seed.root.serves(served("equal", 4)), "a later root is not that root");
    assert!(!seed.root.serves(VersionedRoot::unserved()), "no page is current before an owner");
    assert_eq!(seed.pages.len(), saved.len());
    for ((key, value), (saved_key, _, saved_value)) in seed.pages.iter().zip(&saved) {
        assert_eq!(key, saved_key);
        // Whole-value equality: a field the codec drops fails here, not in a count.
        assert_eq!(value, saved_value, "{key:?} lost a field in the round trip");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_the_asked_sections_are_read_and_a_missing_one_is_simply_absent() {
    let dir = scratch("asked");
    let saved = pages("RelationLabel");
    let file = write(&dir, served("asked", 1), &saved);
    let absent = PageKey::Symbol(symbol("NeverSaved"));
    let seed = file
        .read(&[absent, saved[2].0.clone()])
        .expect("the snapshot reads back");
    assert_eq!(seed.pages.len(), 1, "one asked page is in the file, one is not");
    assert_eq!(seed.pages[0].1, saved[2].2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_corrupt_snapshot_is_ignored_whole_and_kept_as_bad() {
    let saved = pages("RelationLabel");
    let wanted = saved.iter().map(|(key, _, _)| key.clone()).collect::<Vec<_>>();
    let clean = {
        let dir = scratch("clean");
        let file = write(&dir, served("corrupt", 1), &saved);
        let bytes = std::fs::read(file.path()).expect("bytes");
        let _ = std::fs::remove_dir_all(dir);
        bytes
    };
    let table_len = u32::from_le_bytes(clean[12..16].try_into().expect("u32")) as usize;
    // A letter inside a payload's string: the JSON still parses (to another
    // value), so only the section hash can refuse it.
    let letter = clean
        .windows(b"present".len())
        .rposition(|window| window == b"present")
        .expect("a payload names the fixture package");
    assert!(letter > HEADER + table_len, "the letter is in a payload, not the table");
    let corruptions: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 6] = [
        ("a letter in a payload string", Box::new(move |bytes: &mut Vec<u8>| bytes[letter] ^= 0x20)),
        ("a payload byte", Box::new(|bytes: &mut Vec<u8>| {
            let last = bytes.len() - 2;
            bytes[last] ^= 0x20;
        })),
        ("a table byte", Box::new(move |bytes: &mut Vec<u8>| bytes[HEADER + table_len / 2] ^= 0x01)),
        ("the magic", Box::new(|bytes: &mut Vec<u8>| bytes[0] ^= 0xff)),
        ("the schema", Box::new(|bytes: &mut Vec<u8>| bytes[8] = bytes[8].wrapping_add(1))),
        ("a truncation", Box::new(|bytes: &mut Vec<u8>| bytes.truncate(bytes.len() / 2))),
    ];
    for (what, corrupt) in corruptions {
        let dir = scratch("corrupt");
        let file = SnapshotFile::in_data(&dir);
        let mut bytes = clean.clone();
        corrupt(&mut bytes);
        std::fs::write(file.path(), &bytes).expect("write corrupt");
        assert!(file.read(&wanted).is_none(), "{what}: a corrupt snapshot is never used");
        assert!(!file.path().exists(), "{what}: the corrupt file is moved aside");
        assert_eq!(
            std::fs::read(file.path().with_extension("bad")).expect("kept as .bad"),
            bytes,
            "{what}: kept as it was, for a person to look at"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    // The clean bytes read back: the corruptions, not the file, failed.
    let dir = scratch("clean-again");
    let file = SnapshotFile::in_data(&dir);
    std::fs::write(file.path(), &clean).expect("write clean");
    assert_eq!(file.read(&wanted).map(|seed| seed.pages.len()), Some(saved.len()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nothing_is_saved_before_an_owner_answers_and_no_file_is_no_seed() {
    let dir = scratch("unserved");
    let file = SnapshotFile::in_data(&dir);
    let kept = pages("RelationLabel")
        .into_iter()
        .map(|(key, kept, _)| (key, kept))
        .collect::<Vec<_>>();
    assert_eq!(file.write(VersionedRoot::unserved(), &kept).expect("write"), 0);
    assert!(!file.path().exists());
    assert!(file.read(&[PageKey::Orbit]).is_none());
    assert!(!file.path().with_extension("bad").exists(), "an absent file is not a bad one");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_route_keeps_the_pages_it_draws_and_its_shelf_dossier() {
    use crate::navigation::View;
    let package = PageKey::Package(crate::model::pages::PackageRef::parse(PACKAGE).expect("package"));
    let id = symbol("RelationLabel");
    let page = crate::shell::tests::view_route("RelationLabel", View::Page);
    assert_eq!(kept_keys(&page), [PageKey::Symbol(id.clone()), package.clone(), PageKey::Orbit]);
    let code = crate::shell::tests::view_route("RelationLabel", View::Code);
    assert_eq!(
        kept_keys(&code),
        [PageKey::Source(id.clone()), PageKey::Symbol(id), package, PageKey::Orbit]
    );
    assert_eq!(kept_keys(&Route::World), [PageKey::Orbit]);
}
