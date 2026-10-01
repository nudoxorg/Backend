//! The launch snapshot file (W-Open I2): what is saved reads back equal,
//! field for field, and a file that does not check out is ignored whole.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::pages::{DocFragment, PackageRef, PageValue, RowKey, SymbolPage};
use crate::runtime::actor::CancellationToken;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, dossier, page, symbol};
use backend_library::DeclarationKind;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A way to damage the file's bytes.
type Corruption = Box<dyn Fn(&mut Vec<u8>)>;

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

fn pages(name: &str) -> Vec<SeedEntry> {
    let id = symbol(name);
    let package = PackageRef::parse(PACKAGE).expect("package");
    let PageValue::Source(source) = fixture(&ReadRequest::Source(id.clone())) else {
        panic!("a source view")
    };
    let PageValue::Orbit(orbit) = fixture(&ReadRequest::Orbit) else {
        panic!("the orbit model")
    };
    vec![
        SeedEntry::Symbol(id.clone(), Arc::new(rich_page(name))),
        SeedEntry::Source(id, Arc::new(source)),
        SeedEntry::Package(package, Arc::new(dossier())),
        SeedEntry::Orbit(Arc::new(orbit)),
    ]
}

fn write(dir: &Path, root: VersionedRoot, pages: &[SeedEntry]) -> SnapshotFile {
    let file = SnapshotFile::in_data(dir);
    assert!(file.write(root, pages).expect("write") > 0);
    file
}

fn keys(pages: &[SeedEntry]) -> Vec<PageKey> {
    pages.iter().map(SeedEntry::key).collect()
}

#[test]
fn every_saved_page_reads_back_equal_field_for_field() {
    let dir = scratch("equal");
    let root = served("equal", 3);
    let saved = pages("RelationLabel");
    let file = write(&dir, root, &saved);
    let seed = file.read(&keys(&saved)).expect("the snapshot reads back");
    assert!(
        seed.root.serves(root),
        "the root it was read at is the root it names"
    );
    assert!(
        !seed.root.serves(served("equal", 4)),
        "a later root is not that root"
    );
    assert!(
        !seed.root.serves(VersionedRoot::unserved()),
        "no page is current before an owner"
    );
    assert_eq!(seed.pages.len(), saved.len());
    for (read, written) in seed.pages.iter().zip(&saved) {
        // Whole-value equality: a field the codec drops fails here, not in a count.
        assert_eq!(
            read,
            written,
            "{:?} lost a field in the round trip",
            written.key()
        );
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
        .read(&[absent, saved[2].key()])
        .expect("the snapshot reads back");
    assert_eq!(
        seed.pages,
        [saved[2].clone()],
        "one asked page is in the file, one is not"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_corrupt_snapshot_is_ignored_whole_and_kept_as_bad() {
    let saved = pages("RelationLabel");
    let wanted = keys(&saved);
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
    assert!(
        letter > HEADER + table_len,
        "the letter is in a payload, not the table"
    );
    let corruptions: [(&str, Corruption); 6] = [
        (
            "a letter in a payload string",
            Box::new(move |bytes: &mut Vec<u8>| bytes[letter] ^= 0x20),
        ),
        (
            "a payload byte",
            Box::new(|bytes: &mut Vec<u8>| {
                let last = bytes.len() - 2;
                bytes[last] ^= 0x20;
            }),
        ),
        (
            "a table byte",
            Box::new(move |bytes: &mut Vec<u8>| bytes[HEADER + table_len / 2] ^= 0x01),
        ),
        (
            "the magic",
            Box::new(|bytes: &mut Vec<u8>| bytes[0] ^= 0xff),
        ),
        (
            "the schema",
            Box::new(|bytes: &mut Vec<u8>| bytes[8] = bytes[8].wrapping_add(1)),
        ),
        (
            "a truncation",
            Box::new(|bytes: &mut Vec<u8>| bytes.truncate(bytes.len() / 2)),
        ),
    ];
    for (what, corrupt) in corruptions {
        let dir = scratch("corrupt");
        let file = SnapshotFile::in_data(&dir);
        let mut bytes = clean.clone();
        corrupt(&mut bytes);
        std::fs::write(file.path(), &bytes).expect("write corrupt");
        assert!(
            file.read(&wanted).is_none(),
            "{what}: a corrupt snapshot is never used"
        );
        assert_eq!(
            std::fs::read(file.path()).expect("canonical cache remains"),
            bytes,
            "{what}: refusal cannot move a concurrently replaced pathname"
        );
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
    assert_eq!(
        file.read(&wanted).map(|seed| seed.pages.len()),
        Some(saved.len())
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nothing_is_saved_before_an_owner_answers_and_no_file_is_no_seed() {
    let dir = scratch("unserved");
    let file = SnapshotFile::in_data(&dir);
    assert_eq!(
        file.write(VersionedRoot::unserved(), &pages("RelationLabel"))
            .expect("write"),
        0
    );
    assert!(!file.path().exists());
    assert!(file.read(&[PageKey::Orbit]).is_none());
    assert!(
        !file.path().with_extension("bad").exists(),
        "an absent file is not a bad one"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_route_keeps_the_pages_it_draws_and_its_shelf_dossier() {
    use crate::navigation::View;
    let package = PageKey::Package(PackageRef::parse(PACKAGE).expect("package"));
    let id = symbol("RelationLabel");
    let page = crate::shell::tests::view_route("RelationLabel", View::Page);
    assert_eq!(
        kept_keys(&page),
        [PageKey::Symbol(id.clone()), package.clone(), PageKey::Orbit]
    );
    let code = crate::shell::tests::view_route("RelationLabel", View::Code);
    assert_eq!(
        kept_keys(&code),
        [
            PageKey::Source(id.clone()),
            PageKey::Symbol(id),
            package,
            PageKey::Orbit
        ]
    );
    assert_eq!(kept_keys(&Route::World), [PageKey::Orbit]);
}

#[test]
fn a_family_the_snapshot_never_keeps_has_no_section_and_a_digest_is_hex_or_nothing() {
    use crate::model::pages::SearchQuery;
    let search =
        PageKey::Search(SearchQuery::new("Engine", SearchQuery::DEFAULT_LIMIT).expect("query"));
    assert!(
        SectionKey::of(&search).is_none() && SectionKey::of(&PageKey::Health).is_none(),
        "asked fresh every time"
    );
    assert!(SectionKey::of(&PageKey::Orbit).is_some());
    let digest = Digest::of(b"a page");
    let spelled = serde_json::to_string(&digest).expect("digest");
    assert_eq!(spelled.len(), 66, "64 hex digits and two quotes: {spelled}");
    assert_eq!(
        serde_json::from_str::<Digest>(&spelled).expect("reads back"),
        digest
    );
    for refused in [
        "\"abc\"",
        &format!("\"{}\"", "g".repeat(64)),
        &format!("\"{}\"", "a".repeat(63)),
    ] {
        assert!(
            serde_json::from_str::<Digest>(refused).is_err(),
            "{refused} is not a digest"
        );
    }
}

#[test]
fn pages_read_by_another_build_are_not_current_at_the_root_they_name() {
    let root = served("writer", 2);
    let mut snapshot = SnapRoot::of(root);
    assert!(
        snapshot.serves(root),
        "this build reads what this build wrote"
    );
    snapshot.writer.len += 1;
    assert!(
        !snapshot.serves(root),
        "another build's mapping of the same root is another page"
    );
}

fn encoded_table(table: &Table, payload: &[u8]) -> Vec<u8> {
    let table = serde_json::to_vec(table).expect("table");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&SCHEMA.to_le_bytes());
    bytes.extend_from_slice(&(table.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&Digest::of(&table).0);
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn oversized_page_rolls_back_without_starving_later_pages_or_copying_its_encoding() {
    let root = served("oversized", 1);
    let mut large = rich_page("Large");
    large.docs = Arc::from([DocFragment::Text(Arc::from("x".repeat(CAP + 1)))]);
    let later = pages("Small");
    let mut saved = vec![SeedEntry::Symbol(symbol("Large"), Arc::new(large))];
    saved.extend(later.clone());
    let bytes = encode(root, &saved).expect("bounded encoding");
    assert!(bytes.len() < FILE_CAP);
    let read = decode(&bytes, &keys(&saved)).expect("bounded snapshot");
    assert_eq!(
        read.pages, later,
        "oversized first page leaves no partial section"
    );

    let mut payload = Vec::new();
    let mut writer = BoundedWriter::new(&mut payload, 31).expect("bounded output");
    writer.write_all(&[1; 20]).expect("first chunk");
    assert!(writer.write_all(&[2; 12]).is_err());
    assert_eq!(
        payload, [1; 20],
        "a rejected chunk never enters the payload"
    );
    assert!(
        payload.capacity() <= 31,
        "growth is clipped to the serialization budget"
    );
}

#[test]
fn oversized_file_and_table_are_refused_before_parsing() {
    let dir = scratch("sparse");
    let file = SnapshotFile::in_data(&dir);
    std::fs::File::create(file.path())
        .expect("create sparse cache")
        .set_len(16_u64 << 30)
        .expect("large sparse length");
    assert!(file.read(&[PageKey::Orbit]).is_none());
    assert!(file.path().exists(), "an inadmissible path is never moved");
    let mut bytes = encode(served("limits", 1), &[]).expect("empty snapshot");
    bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(decode(&bytes, &[]), Err(Refusal::TableLength)));
    let oversized = vec![0; FILE_CAP + 1];
    assert!(matches!(decode(&oversized, &[]), Err(Refusal::TooLarge)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn section_count_duplicate_keys_and_noncanonical_ranges_are_refused() {
    let root = served("sections", 1);
    let mut table = Table {
        root: SnapRoot::of(root),
        sections: (0..=SECTION_CAP)
            .map(|index| Section {
                key: SectionKey::Symbol(symbol(&format!("Item{index}"))),
                offset: index,
                len: 1,
                hash: Digest::of(b"x"),
            })
            .collect(),
    };
    assert!(matches!(
        decode(&encoded_table(&table, &vec![b'x'; SECTION_CAP + 1]), &[]),
        Err(Refusal::Table(_))
    ));
    table.sections.truncate(2);
    table.sections[1].key = table.sections[0].key.clone();
    assert!(matches!(
        decode(&encoded_table(&table, b"xx"), &[]),
        Err(Refusal::Sections)
    ));
    table.sections[1].key = SectionKey::Orbit;
    table.sections[1].offset = 0;
    assert!(matches!(
        decode(&encoded_table(&table, b"xx"), &[]),
        Err(Refusal::Section {
            fault: SectionFault::Layout,
            ..
        })
    ));
    table.sections[1].offset = 1;
    assert!(matches!(
        decode(&encoded_table(&table, b"xxx"), &[]),
        Err(Refusal::TableEnd)
    ));
}

#[test]
fn corruption_in_an_unrequested_section_refuses_the_entire_snapshot() {
    let saved = pages("Integrity");
    let mut bytes = encode(served("integrity", 1), &saved).expect("snapshot");
    let table_len = u32::from_le_bytes(bytes[12..16].try_into().expect("table length")) as usize;
    bytes[HEADER + table_len] ^= 1;
    assert!(matches!(
        decode(&bytes, &[PageKey::Orbit]),
        Err(Refusal::Section {
            fault: SectionFault::Hash,
            ..
        })
    ));
}

#[test]
fn refusal_never_overwrites_existing_diagnostic_bytes() {
    let dir = scratch("diagnostic");
    let file = SnapshotFile::in_data(&dir);
    let bad = file.path().with_extension("bad");
    std::fs::write(&bad, b"older diagnostic").expect("existing diagnostic");
    std::fs::write(file.path(), b"broken cache").expect("corrupt cache");
    assert!(file.read(&[]).is_none());
    assert_eq!(
        std::fs::read(&bad).expect("diagnostic"),
        b"older diagnostic"
    );
    assert_eq!(
        std::fs::read(file.path()).expect("canonical path"),
        b"broken cache"
    );
    let _ = std::fs::remove_dir_all(dir);
}
