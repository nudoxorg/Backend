#![allow(clippy::expect_used, clippy::unwrap_used)]

use allocation_counter::measure;
use backend_store::{
    ArtifactClosureClaim, ClosureManifest, FileStore, StoreError, TypedObject, UntrustedObjectId,
};
use backend_version::{ObjectKey, Schema};
use std::{
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_STORE: AtomicUsize = AtomicUsize::new(0);
const OBJECT_HEADER_BYTES: u64 = 15 + 32 + 1 + 2 + 1 + 32 + 32 + 8;

struct BytesSchema;

impl Schema for BytesSchema {
    const DOMAIN: u8 = 0xf4;
    const TYPE: u16 = 24;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

struct TestStore {
    path: PathBuf,
    store: FileStore,
}

impl TestStore {
    fn new(max_pack_bytes: usize) -> Self {
        let ordinal = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-store-bounded-read-{}-{ordinal}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        let store = FileStore::open(&path, max_pack_bytes).expect("open bounded-read store");
        Self { path, store }
    }
}

impl Drop for TestStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Default)]
struct DigestWriter {
    hasher: blake3::Hasher,
    bytes: u64,
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(bytes);
        self.bytes = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).expect("test write length fits u64"))
            .expect("test byte count fits u64");
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl DigestWriter {
    fn digest(&self) -> [u8; 32] {
        *self.hasher.clone().finalize().as_bytes()
    }
}

fn object(payload: &[u8]) -> TypedObject {
    let key = ObjectKey::<BytesSchema>::from_value(payload);
    TypedObject::from_value(&key, payload)
}

fn object_path(root: &std::path::Path, object_id: &[u8; 32]) -> PathBuf {
    root.join("objects")
        .join(format!("{}.object", hex(object_id)))
}

fn closure_path(root: &std::path::Path, closure_id: &[u8; 32]) -> PathBuf {
    root.join("closures")
        .join(format!("{}.closure", hex(closure_id)))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn make_writable(path: &std::path::Path) {
    let mut permissions = fs::metadata(path)
        .expect("read file metadata")
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).expect("make immutable test file writable");
}

#[test]
fn sixteen_mib_payload_copy_uses_bounded_working_memory() {
    const PAYLOAD_LEN: usize = 16 * 1024 * 1024;
    let test_store = TestStore::new(24 * 1024 * 1024);
    let payload = (0..PAYLOAD_LEN)
        .map(|index| u8::try_from(index % 251).expect("bounded payload byte"))
        .collect::<Vec<_>>();
    let item = object(&payload);
    let object_id = test_store
        .store
        .write_object(&item)
        .expect("write source object");
    let claim = UntrustedObjectId::from_bytes(*object_id.as_bytes());
    let payload_len = u64::try_from(PAYLOAD_LEN).expect("payload length fits u64");

    let mut output = DigestWriter::default();
    let mut result = None;
    let allocations = measure(|| {
        result = Some(test_store.store.write_verified_object_payload(
            claim,
            payload_len,
            &mut output,
        ));
    });
    let checked = result
        .expect("copy helper ran")
        .expect("verify and copy object");
    assert_eq!(checked.id(), object_id);
    assert_eq!(checked.payload_len(), payload_len);
    assert_eq!(output.bytes, payload_len);
    assert_eq!(output.digest(), *blake3::hash(&payload).as_bytes());
    assert!(
        allocations.bytes_max < 256 * 1024,
        "bounded copy peak allocation was {} bytes ({allocations:?})",
        allocations.bytes_max
    );
}

#[test]
fn zero_byte_object_is_verified_and_copied_as_empty() {
    let test_store = TestStore::new(64 * 1024);
    let item = object(&[]);
    let object_id = test_store
        .store
        .write_object(&item)
        .expect("write empty object");
    let mut output = DigestWriter::default();
    let checked = test_store
        .store
        .write_verified_object_payload(
            UntrustedObjectId::from_bytes(*object_id.as_bytes()),
            0,
            &mut output,
        )
        .expect("verify and copy empty object");
    assert_eq!(checked.payload_len(), 0);
    assert_eq!(output.bytes, 0);
    assert_eq!(output.digest(), *blake3::hash(&[]).as_bytes());
}

#[test]
fn tampered_object_writes_no_unverified_payload_bytes() {
    let test_store = TestStore::new(64 * 1024);
    let item = object(b"checked payload");
    let object_id = test_store
        .store
        .write_object(&item)
        .expect("write object to tamper");
    let path = object_path(&test_store.path, object_id.as_bytes());
    make_writable(&path);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open object for tampering");
    file.seek(SeekFrom::Start(OBJECT_HEADER_BYTES + 1))
        .expect("seek to payload");
    file.write_all(b"X").expect("change payload byte");

    let mut output = DigestWriter::default();
    assert!(matches!(
        test_store.store.write_verified_object_payload(
            UntrustedObjectId::from_bytes(*object_id.as_bytes()),
            1024,
            &mut output,
        ),
        Err(StoreError::Corrupt)
    ));
    assert_eq!(
        output.bytes, 0,
        "verification must finish before output begins"
    );
}

#[test]
fn closure_claim_opens_metadata_index_and_separates_membership_from_bytes() {
    let test_store = TestStore::new(1024 * 1024);
    let item = object(b"manifest member");
    let object_id = item.id();
    let manifest = ClosureManifest::new(vec![item]).expect("build closure");
    let closure_id = test_store
        .store
        .write_closure(&manifest)
        .expect("write closure index");

    let cold_store = FileStore::open(&test_store.path, 1024 * 1024).expect("cold reopen store");
    assert_eq!(
        cold_store
            .admit_closure_claim(ArtifactClosureClaim::from_bytes(*closure_id.as_bytes()))
            .expect("cold-admit stored closure claim"),
        closure_id
    );
    assert!(matches!(
        cold_store.admit_closure_claim(ArtifactClosureClaim::from_bytes([0x99; 32])),
        Err(StoreError::Corrupt)
    ));
    let index = cold_store
        .open_closure_claim(ArtifactClosureClaim::from_id(closure_id))
        .expect("open checked closure claim");
    assert_eq!(index.id(), closure_id);
    assert_eq!(index.object_count(), 1);
    assert!(index.contains_object_id(object_id).expect("member lookup"));
    assert_eq!(
        index
            .page_ids(None, 1)
            .expect("metadata ID page")
            .stats()
            .objects_read,
        0
    );

    let missing_path = object_path(&test_store.path, object_id.as_bytes());
    fs::remove_file(&missing_path).expect("remove closure member");
    let index = cold_store
        .read_closure_index(closure_id)
        .expect("reopen metadata index despite missing payload");
    assert!(
        index
            .contains_object_id(object_id)
            .expect("membership comes from authenticated index")
    );
    assert!(matches!(
        cold_store.verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes())),
        Err(StoreError::Corrupt)
    ));

    let descriptor_path = closure_path(&test_store.path, closure_id.as_bytes());
    make_writable(&descriptor_path);
    let mut descriptor = OpenOptions::new()
        .write(true)
        .open(&descriptor_path)
        .expect("open descriptor for tampering");
    descriptor
        .seek(SeekFrom::Start(0))
        .expect("seek to descriptor header");
    descriptor
        .write_all(b"X")
        .expect("tamper closure descriptor");
    assert!(matches!(
        cold_store.read_closure_index(closure_id),
        Err(StoreError::Corrupt)
    ));
}
