//! Public caller-buffer laws for canonical generation roots.

use nudox_id::{GenerationId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef};
use nudox_root::{EntryKey, GenerationRoot, RootBuildError, RootEntry, RootWriteError};
use nudox_schema::SchemaId;
use thiserror::Error;

#[derive(Debug, Error)]
enum CanonicalWriterTestError {
    #[error("the canonical test root was invalid")]
    Build(#[from] RootBuildError),
    #[error("an exact caller buffer rejected a canonical root")]
    Write(#[from] RootWriteError),
}

fn entry(key: u64, parent: Option<u64>, content: u8) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: EntryKey::from(key),
        parent: parent.map(EntryKey::from),
        object: ObjectRef {
            content: nudox_id::ContentId::from_digest([content; 32]),
            length: ObjectLength::from(3),
            schema: SchemaId::Object,
            kind: ObjectKind::from(9),
        },
    }
}

#[test]
fn canonical_writer_is_exact_borrowed_and_transactional() -> Result<(), CanonicalWriterTestError> {
    let root = GenerationRoot::new(vec![entry(1, None, 7)])?;
    let required = usize::from(root.canonical_len());
    let golden: &[u8] = &[
        0, 0, 0, 0, 0, 0, 0, 1, // row count
        0, 0, 0, 0, 0, 0, 0, 1, // key
        0, // no parent
        0, 0, 0, 0, 0, 0, 0, 0, // absent parent key
        1, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, // content
        0, 0, 0, 0, 0, 0, 0, 3, // length
        0, 0, 0, 1, // schema
        0, 9, // kind
    ];
    assert_eq!(required, golden.len());

    let mut short = vec![0xa5; required - 1];
    let short_bytes = short.len().into();
    assert_eq!(
        root.write_canonical(&mut short),
        Err(RootWriteError {
            required: root.canonical_len(),
            available: short_bytes,
        })
    );
    assert!(short.iter().all(|byte| *byte == 0xa5));

    let mut exact = vec![0; required];
    let exact_pointer = exact.as_ptr();
    let written = root.write_canonical(&mut exact)?;
    assert_eq!(written, golden);
    assert_eq!(written.as_ptr(), exact_pointer);
    assert_eq!(GenerationId::from_canonical_bytes(written), root.id);

    let mut oversized = vec![0xa5; required + 1];
    assert_eq!(root.write_canonical(&mut oversized)?, golden);
    assert_eq!(oversized.last(), Some(&0xa5));
    Ok(())
}
