//! Generation-stamp and manifest tests: content sensitivity and determinism.
use super::*;
use crate::{
    change::IntroId,
    kinds::Module,
    test_helpers::{entry, node, sym},
};

// ── helpers ────────────────────────────────────────────────────────────

fn intro(byte: u8) -> IntroId {
    IntroId::from_raw([byte; 32])
}

#[allow(dead_code)]
fn fake_content_hash(byte: u8) -> ContentBlake3 {
    ContentBlake3::from_raw([byte; 32])
}

/// Build a table with entries whose content hashes are determined by
/// `id_byte` (same byte for id and content, for simplicity). The actual
/// `entry_content_hash` call is tested indirectly via the sensitivity test
/// below; here we just need distinct entries.
fn make_table(pairs: &[(u8, Option<u8>)]) -> PristineIntroTable {
    let mut t = PristineIntroTable::new();
    for &(id_byte, parent_byte) in pairs {
        let id = intro(id_byte);
        let parent = parent_byte.map(intro);
        t.insert_live(id, entry(sym("x"), node::root([]), Module), parent);
    }
    t
}

fn cas(byte: u8) -> CasKey {
    CasKey::from_raw([byte; 32])
}

fn change_id(byte: u8) -> ChangeId {
    ChangeId::from_raw([byte; 32])
}

fn lineage(eco: &str, name: &str) -> PackageLineageId {
    use crate::change::{EcosystemId, PackageName};
    PackageLineageId::new(EcosystemId::new(eco), PackageName::new(name))
}

fn outbox_entry(byte: u8) -> OutboxEntry {
    OutboxEntry {
        package: lineage("cargo", "my-crate"),
        generation: GenerationStamp::from_domain("test.gen", &[byte; 32]),
        channel: ChannelName::new("main"),
        tip_change: None,
    }
}

fn dummy_toolchain() -> Toolchain {
    Toolchain::new("rust", "1.80.0")
}

fn dummy_uuid() -> PackageUuid {
    PackageUuid::from_bytes([0x01; 16])
}

fn base_manifest() -> BlobManifest {
    BlobManifest {
        package: dummy_uuid(),
        files: Box::new([
            FileEntry {
                path: "src/lib.rs".to_string(),
                content_hash: cas(0x01),
            },
            FileEntry {
                path: "src/main.rs".to_string(),
                content_hash: cas(0x02),
            },
        ]),
        ir_package_ref: cas(0x10),
        change_set_ref: None,
        references_ref: None,
        occurrences_ref: None,
        toolchain: dummy_toolchain(),
    }
}

// ── GenerationStamp (table-based) ──────────────────────────────────────

// (a) Order-independence: two tables with the same entries but different
// insertion order produce the same stamp.
//
// Note: this test uses the same entry body for all rows (both have
// sym("x") and Module kind). The content hashes are therefore identical
// across the two tables, so order-independence of the id sort is exercised.
#[test]
fn stamp_order_independent() {
    let t_forward = make_table(&[(1, None), (2, Some(1))]);
    let t_reverse = make_table(&[(2, None), (1, Some(2))]);
    assert_eq!(
        generation_stamp(&t_forward),
        generation_stamp(&t_reverse),
        "insertion order must not affect the stamp"
    );
}

// (b) Sensitivity — the headline fix: two tables with the SAME IntroId
// set but DIFFERENT entry content produce DIFFERENT stamps.
//
// This test exercises the v2 change directly: under the old v1 stub
// (which folded only IntroIds) these two tables would produce the same
// stamp. Under v2 they must not.
//
// Implementation note: `entry_content_hash` is called by
// `generation_stamp` on the actual `Entry` values. We rely on the fact
// that entries with different `sym().name` values (here "x" vs "y") will
// produce different content hashes. If the content hasher does not cover
// the symbol name this test will catch the regression.
#[test]
fn stamp_sensitive_to_entry_content() {
    // Table A: intro 1 → sym("x"), intro 2 → sym("x").
    let mut t_a = PristineIntroTable::new();
    t_a.insert_live(intro(1), entry(sym("x"), node::root([]), Module), None);
    t_a.insert_live(intro(2), entry(sym("x"), node::root([]), Module), None);

    // Table B: same IntroIds, but intro 1 has sym("different_body").
    let mut t_b = PristineIntroTable::new();
    t_b.insert_live(
        intro(1),
        entry(sym("different_body"), node::root([]), Module),
        None,
    );
    t_b.insert_live(intro(2), entry(sym("x"), node::root([]), Module), None);

    assert_ne!(
        generation_stamp(&t_a),
        generation_stamp(&t_b),
        "stamps must differ when entries have the same IntroIds but different content — \
         this is the headline fix over the v1 stub which could not detect edits"
    );
}

// (c) Golden pin for the stamp of a fixed small table.
//
// If this fails, the v2 stamp preimage changed — either bump
// `GENERATION_DOMAIN` deliberately and paste the new hash from the failure,
// or investigate, because the generation identity just moved by accident.
#[test]
fn stamp_golden_pin() {
    let mut t = PristineIntroTable::new();
    t.insert_live(
        IntroId::from_raw([1u8; 32]),
        entry(sym("a"), node::root([]), Module),
        None,
    );
    t.insert_live(
        IntroId::from_raw([2u8; 32]),
        entry(sym("b"), node::root([]), Module),
        None,
    );
    let hex = generation_stamp(&t).to_hex();
    assert_eq!(
        hex, "e0b044d4fca1c756bcc37d8bbca371022f3d547aa279ae8b8fb4242128d62428",
        "GenerationStamp v2 golden pin moved"
    );
}

// ── BlobManifest serde round-trip ──────────────────────────────────────

// (d) Manifest serde round-trip via serde_json (dev-dep).
#[test]
fn manifest_serde_round_trip() {
    let mut m = base_manifest();
    m.change_set_ref = Some(ChangeSetRef {
        channel: ChannelName::new("main"),
        tip_change: Some(change_id(0xab)),
        change_log_cas: Some(cas(0x42)),
    });
    m.references_ref = Some(cas(0xaa));
    m.occurrences_ref = Some(cas(0xbb));

    let json = serde_json::to_string(&m).expect("serialize");
    let back: BlobManifest = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(m, back, "BlobManifest must round-trip through serde_json");
}

// ── manifest_stamp ─────────────────────────────────────────────────────

// (e) manifest_stamp is deterministic.
#[test]
fn manifest_stamp_deterministic() {
    let m = base_manifest();
    let s1 = manifest_stamp(&m);
    let s2 = manifest_stamp(&m);
    assert_eq!(s1, s2, "manifest_stamp must be deterministic");
}

// (f) Reordering files in the manifest must NOT change manifest_stamp
// (the function sorts by path).
#[test]
fn manifest_stamp_file_order_independent() {
    let m_original = base_manifest();
    let mut m_reordered = base_manifest();
    m_reordered.files = Box::new([
        FileEntry {
            path: "src/main.rs".to_string(),
            content_hash: cas(0x02),
        },
        FileEntry {
            path: "src/lib.rs".to_string(),
            content_hash: cas(0x01),
        },
    ]);
    assert_eq!(
        manifest_stamp(&m_original),
        manifest_stamp(&m_reordered),
        "file insertion order must not affect manifest_stamp"
    );
}

// (g) Changing a file hash MUST change manifest_stamp.
#[test]
fn manifest_stamp_changes_on_file_content_change() {
    let m_original = base_manifest();
    let mut m_changed = base_manifest();
    m_changed.files = Box::new([
        FileEntry {
            path: "src/lib.rs".to_string(),
            content_hash: cas(0xff),
        },
        FileEntry {
            path: "src/main.rs".to_string(),
            content_hash: cas(0x02),
        },
    ]);
    assert_ne!(
        manifest_stamp(&m_original),
        manifest_stamp(&m_changed),
        "changing a file hash must change manifest_stamp"
    );
}

// (h) Changing the toolchain MUST change manifest_stamp.
#[test]
fn manifest_stamp_changes_on_toolchain_change() {
    let m_original = base_manifest();
    let mut m_changed = base_manifest();
    m_changed.toolchain = Toolchain::new("rust", "1.81.0");
    assert_ne!(
        manifest_stamp(&m_original),
        manifest_stamp(&m_changed),
        "changing the toolchain must change manifest_stamp"
    );
}

// ── Outbox ─────────────────────────────────────────────────────────────

// (i) append/pending: entries appear in insertion order; pending does not
// drain.
#[test]
fn outbox_append_and_pending() {
    let outbox = InMemoryOutbox::new();
    assert!(outbox.is_empty().unwrap());

    outbox.append(outbox_entry(1)).unwrap();
    outbox.append(outbox_entry(2)).unwrap();

    let pending = outbox.pending().unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(
        pending[0].generation,
        GenerationStamp::from_domain("test.gen", &[1u8; 32])
    );
    assert_eq!(
        pending[1].generation,
        GenerationStamp::from_domain("test.gen", &[2u8; 32])
    );

    // pending() must NOT drain.
    let pending2 = outbox.pending().unwrap();
    assert_eq!(pending2.len(), 2, "pending() must not drain the outbox");
}

// (j) drain empties the outbox.
#[test]
fn outbox_drain_empties() {
    let outbox = InMemoryOutbox::new();
    outbox.append(outbox_entry(10)).unwrap();
    outbox.append(outbox_entry(20)).unwrap();

    let drained = outbox.drain().unwrap();
    assert_eq!(drained.len(), 2);
    assert!(outbox.is_empty().unwrap(), "drain must empty the outbox");
}

// (k) Outbox is object-safe — can be used as dyn Outbox.
#[test]
fn outbox_is_object_safe() {
    let outbox: Box<dyn Outbox> = Box::new(InMemoryOutbox::new());
    outbox.append(outbox_entry(5)).unwrap();
    let pending = outbox.pending().unwrap();
    assert_eq!(pending.len(), 1);
}

// (l) GenerationStamp has the gen: Debug prefix (confirms it is not a CasKey).
#[test]
fn generation_stamp_debug_prefix() {
    let e = outbox_entry(0xab);
    let debug = format!("{:?}", e.generation);
    assert!(
        debug.starts_with("gen:"),
        "GenerationStamp must have gen: debug prefix, got: {debug}"
    );
}
