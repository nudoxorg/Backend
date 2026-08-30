use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectRef};
use nudox_root::{EntryKey, GenerationRoot, RootEntry, RootReadError, ValidatedRoot};
use nudox_schema::SchemaId;

fn entry(key: u64, parent: Option<u64>) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: key.into(),
        parent: parent.map(Into::into),
        object: ObjectRef {
            content: ContentId::from([key as u8; 32]),
            length: 0.into(),
            schema: SchemaId::Object,
            kind: ObjectKind::from(0),
        },
    }
}

#[test]
fn canonical_bytes_round_trip_and_identity_match() {
    let root = GenerationRoot::new(vec![entry(2, Some(1)), entry(1, None)]).unwrap();
    let mut bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut bytes).unwrap();
    let borrowed = ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice()).unwrap();
    assert_eq!(borrowed.id, root.id);
    assert_eq!(borrowed.entry_count, 2.into());
    assert_eq!(borrowed.bytes, bytes.as_slice());
}

#[test]
fn exact_extent_is_required() {
    let root = GenerationRoot::new(vec![entry(1, None)]).unwrap();
    let mut bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut bytes).unwrap();
    let error = match ValidatedRoot::<ObjectDomain>::try_from(&bytes[..bytes.len() - 1]) {
        Ok(_) => panic!("expected extent"),
        Err(error) => error,
    };
    assert!(matches!(error, RootReadError::Extent { .. }));
    bytes.push(0);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice()),
        Err(RootReadError::Extent { .. })
    ));
}

