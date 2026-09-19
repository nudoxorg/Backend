//! Closure admission, manifest path-copy, and transitive CAS tests.

use super::*;
use crate::StoredValue;
use backend_version::{CanonicalRelation, ObjectKey, Schema};

struct LargeTestRelation;

impl backend_version::Relation for LargeTestRelation {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 10;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

impl CanonicalRelation for LargeTestRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }
}

#[test]
fn decode_rejects_recommitted_forged_object_version() {
    let object = TypedObject::from_wire_parts(
        SchemaIdentity::new(0x79, 4, 1),
        [1; ID_BYTES],
        [2; ID_BYTES],
        Box::from([3u8]),
    );
    let id = ClosureId([9; ID_BYTES]);
    let objects = [object];
    let manifest = ClosureManifest::from_test_parts(&objects, id);
    let Ok(encoded) = manifest.encode(4_096) else {
        return;
    };
    assert!(matches!(
        ClosureManifest::decode(&encoded, 4_096),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn runtime_identity_matches_backend_version_golden_vector() {
    let schema = SchemaIdentity::new(0x7f, 1, 1);
    let bytes = 7u64.to_be_bytes();
    let expected = backend_version::object_version_digest(schema, &bytes)
        .unwrap_or_else(|_| std::panic::resume_unwind(Box::new("test operation failed")));
    assert!(verify_backend_object_version(schema, &bytes, &expected));
    assert!(!verify_backend_object_version(schema, b"forged", &expected));
    assert_eq!(
        expected,
        [
            243, 211, 54, 253, 209, 222, 31, 230, 177, 6, 201, 233, 65, 182, 195, 109, 151, 219,
            190, 122, 149, 66, 250, 69, 127, 246, 191, 171, 95, 196, 245, 186,
        ]
    );
}

#[test]
fn manifest_delta_path_copies_only_changed_index_neighborhood() {
    struct ManifestSchema;

    impl Schema for ManifestSchema {
        const DOMAIN: u8 = 0x79;
        const TYPE: u16 = 0x20;
        const VERSION: u8 = 1;
        type Value = u64;

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    let mut objects = (0u64..4_096)
        .map(|value| {
            let key = ObjectKey::<ManifestSchema>::from_value(&value);
            TypedObject::from_value(&key, &value)
        })
        .collect::<Vec<_>>();
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let base = ClosureManifest::new(objects).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("manifest construction failed: {error:?}")))
    });
    let before = base.objects()[base.objects().len() / 2].clone();
    let replacement_value = u64::MAX;
    let replacement_key = ObjectKey::<ManifestSchema>::from_value(&replacement_value);
    let after = TypedObject::from_value(&replacement_key, &replacement_value);
    let changes = ManifestChange::replacement(&before, &after).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("manifest replacement failed: {error:?}")))
    });
    let prepared = base.prepare_delta(&changes).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("manifest delta failed: {error:?}")))
    });
    let work = prepared.work();
    assert!(work.visited_nodes < 512);
    assert!(work.copied_nodes < 64);
    assert!(work.reused_rows > 0);
    let target = prepared.commit();
    assert_ne!(target.id(), base.id());
    assert_eq!(target.objects().len(), base.objects().len());
    assert!(target.objects().iter().any(|object| object == &after));
    let untouched = base
        .objects()
        .iter()
        .find(|object| object.id() != before.id())
        .ok_or("missing untouched object")
        .unwrap_or_else(|error| std::panic::resume_unwind(Box::new(error)));
    let retained = target
        .objects()
        .iter()
        .find(|object| object.id() == untouched.id())
        .ok_or("missing retained object")
        .unwrap_or_else(|error| std::panic::resume_unwind(Box::new(error)));
    assert!(std::ptr::eq(
        untouched.bytes().as_ptr(),
        retained.bytes().as_ptr()
    ));
}

#[test]
fn decode_rejects_recommitted_malformed_raw_relation_root() {
    let Ok(node) = backend_version::canonical_leaf::<RawRelation>(&[(
        b"key".to_vec(),
        StoredValue::new(b"value".to_vec(), 1, Vec::new()),
    )]) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let Ok(valid) = TypedObject::from_state_root(node.commitment(), &node) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let mut bytes = valid.bytes().to_vec();
    bytes[0] ^= 1;
    let forged_version = backend_version::state_root_digest(valid.schema(), &bytes)
        .unwrap_or_else(|_| std::panic::resume_unwind(Box::new("test operation failed")));
    let forged = TypedObject::from_wire_parts(
        valid.schema(),
        *valid.key(),
        forged_version,
        bytes.into_boxed_slice(),
    );
    assert!(ClosureManifest::new(vec![forged.clone()]).is_err());
    let provisional_objects = [forged.clone()];
    let provisional =
        ClosureManifest::from_test_parts(&provisional_objects, ClosureId([0; ID_BYTES]));
    let manifest_objects = [forged];
    let manifest = ClosureManifest::from_test_parts(&manifest_objects, provisional.id());
    let Ok(encoded) = manifest.encode(4_096) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    assert!(matches!(
        ClosureManifest::decode(&encoded, 4_096),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn closure_rejects_unknown_relation_state_root_without_decoder() {
    let schema = SchemaIdentity::new(0x79, 0x55, 1);
    let bytes = b"arbitrary relation bytes";
    let version = backend_version::state_root_digest(schema, bytes)
        .unwrap_or_else(|_| std::panic::resume_unwind(Box::new("test operation failed")));
    let object = TypedObject::from_wire_parts(
        schema,
        [7; ID_BYTES],
        version,
        bytes.to_vec().into_boxed_slice(),
    );
    assert!(ClosureManifest::new(vec![object]).is_err());
}

#[test]
fn registered_relation_root_round_trips_through_wire_admission() {
    struct TestRelation;

    impl backend_version::Relation for TestRelation {
        const DOMAIN: u8 = 0x7a;
        const TYPE: u16 = 9;
        type Key = u64;
        type Value = u64;

        fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }

        fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    impl CanonicalRelation for TestRelation {
        fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
            bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| backend_version::RelationDecodeError::Malformed)
        }

        fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
            bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| backend_version::RelationDecodeError::Malformed)
        }
    }

    let Ok(node) = backend_version::canonical_leaf::<TestRelation>(&[(1, 2)]) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let Ok(object) = TypedObject::from_state_root(node.commitment(), &node) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let Ok(registry) = RelationAdmissionRegistry::default().with_relation::<TestRelation>() else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    assert!(ClosureManifest::new(vec![object.clone()]).is_ok());
    let Ok(manifest) = ClosureManifest::new_with_registry(vec![object], &registry) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let Ok(encoded) = manifest.encode(4_096) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    let Ok(reopened) = ClosureManifest::decode_with_registry(&encoded, 4_096, &registry) else {
        std::panic::resume_unwind(Box::new("test operation failed"))
    };
    assert_eq!(reopened, manifest);
    assert!(ClosureManifest::decode(&encoded, 4_096).is_err());
}

#[test]
fn large_registered_relation_closure_is_transitive_and_fails_closed() {
    let state = backend_version::RelationState::<LargeTestRelation>::from_entries(
        (0u64..2_048).map(|value| (value, value.wrapping_mul(3))),
        backend_version::CoverageWitness::Partial(backend_version::partial_coverage(1)),
    )
    .unwrap_or_else(|error| {
        eprintln!("test relation state failed: {error:?}");
        std::panic::resume_unwind(Box::new("test operation failed"));
    });
    let registry = RelationAdmissionRegistry::default()
        .with_relation::<LargeTestRelation>()
        .unwrap_or_else(|error| {
            eprintln!("test relation registry failed: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        });
    let manifest = ClosureManifest::for_relation_state_with_registry(&state, &registry)
        .unwrap_or_else(|error| {
            eprintln!("test relation closure failed: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        });
    assert!(manifest.objects().len() > 1);
    let edges = manifest.object_edges(&registry).unwrap_or_else(|error| {
        eprintln!("test relation edges failed: {error:?}");
        std::panic::resume_unwind(Box::new("test operation failed"));
    });
    assert!(!edges.is_empty());
    for edge in edges {
        assert!(
            manifest
                .objects()
                .iter()
                .any(|object| object.id() == edge.from())
        );
        assert!(
            manifest
                .objects()
                .iter()
                .any(|object| object.id() == edge.to())
        );
    }

    let path = std::env::temp_dir().join(format!(
        "backend-store-relation-closure-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = crate::durable::FileStore::open_with_registry(&path, 1_000_000, registry.clone())
        .unwrap_or_else(|error| {
            eprintln!("test store open failed: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        });
    let id = store.write_closure(&manifest).unwrap_or_else(|error| {
        eprintln!("test closure write failed: {error:?}");
        std::panic::resume_unwind(Box::new("test operation failed"));
    });
    let reopened = crate::durable::FileStore::open_with_registry(&path, 1_000_000, registry)
        .unwrap_or_else(|error| {
            eprintln!("test store reopen failed: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        });
    assert_eq!(reopened.read_closure(id), Ok(manifest.clone()));

    let object = std::fs::read_dir(path.join("objects"))
        .unwrap_or_else(|error| {
            eprintln!("test object directory failed: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        })
        .find_map(Result::ok)
        .map_or_else(
            || {
                eprintln!("test object was absent");
                std::panic::resume_unwind(Box::new("test operation failed"));
            },
            |entry| entry.path(),
        );
    std::fs::remove_file(object).unwrap_or_else(|error| {
        eprintln!("test object removal failed: {error:?}");
        std::panic::resume_unwind(Box::new("test operation failed"));
    });
    assert!(matches!(
        reopened.read_closure(id),
        Err(StoreError::Corrupt)
    ));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn relation_node_cas_is_schema_parametric_and_reopens_children() {
    let state = backend_version::RelationState::<LargeTestRelation>::from_entries(
        (0u64..2_048).map(|value| (value, value.wrapping_mul(5))),
        backend_version::CoverageWitness::Partial(backend_version::partial_coverage(1)),
    )
    .unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("test relation state failed: {error:?}")))
    });
    let registry = RelationAdmissionRegistry::default()
        .with_relation::<LargeTestRelation>()
        .unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!(
                "test relation registry failed: {error:?}"
            )))
        });
    let path = std::env::temp_dir().join(format!(
        "backend-store-relation-node-cas-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = crate::durable::FileStore::open_with_registry(&path, 1_000_000, registry)
        .unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!("test store open failed: {error:?}")))
        });
    let write_stats = store.write_relation_state(&state).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!(
            "test relation node write failed: {error:?}"
        )))
    });
    assert!(write_stats.nodes_written > 1);
    assert!(write_stats.bytes_written > 0);
    let root = store
        .read_relation_node::<LargeTestRelation>(state.root())
        .unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!(
                "test relation node read failed: {error:?}"
            )))
        });
    assert_eq!(
        root.schema(),
        SchemaIdentity::of_relation::<LargeTestRelation>()
    );
    assert_eq!(root.version(), state.root().as_bytes());
    let claim = backend_version::UntrustedId::<LargeTestRelation>::from_wire(
        state.root().as_bytes(),
        backend_version::IdContext::relation::<LargeTestRelation>(),
    )
    .unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("test root claim failed: {error:?}")))
    });
    let node = store
        .read_relation_node_with_children(claim)
        .unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!("test typed node view failed: {error:?}")))
        });
    assert_eq!(node.node().root(), state.root());
    assert_eq!(node.object(), root.id());
    assert!(!node.children().is_empty());
    assert_eq!(
        node.children().len(),
        node.node()
            .child_summaries()
            .unwrap_or_else(|error| {
                std::panic::resume_unwind(Box::new(format!("test child summary failed: {error:?}")))
            })
            .len()
    );
    let lazy = backend_version::LazyTree::open(&store, claim).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!("test lazy tree open failed: {error:?}")))
    });
    assert_eq!(
        lazy.lookup(&1u64).unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!("test lazy lookup failed: {error:?}")))
        }),
        Some(5)
    );
    let edges = store
        .relation_node_edges(state.root())
        .unwrap_or_else(|error| {
            std::panic::resume_unwind(Box::new(format!(
                "test relation node edges failed: {error:?}"
            )))
        });
    assert!(!edges.is_empty());
    let second = store.write_relation_state(&state).unwrap_or_else(|error| {
        std::panic::resume_unwind(Box::new(format!(
            "test relation node rewrite failed: {error:?}"
        )))
    });
    assert_eq!(second.nodes_written, 0);
    assert_eq!(second.bytes_written, 0);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn cloned_large_closure_shares_immutable_storage() {
    struct TestSchema;

    impl Schema for TestSchema {
        const DOMAIN: u8 = 0x79;
        const TYPE: u16 = 5;
        const VERSION: u8 = 1;
        type Value = u64;

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    let mut objects = (0..2048u64)
        .map(|value| {
            let key = ObjectKey::<TestSchema>::from_value(&value);
            TypedObject::from_value(&key, &value)
        })
        .collect::<Vec<_>>();
    objects.sort_by_key(|object| (object.schema, object.key, object.version));
    let manifest = match ClosureManifest::new(objects) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("test objects are not canonical: {error:?}");
            std::panic::resume_unwind(Box::new("test operation failed"));
        }
    };
    let cloned = manifest.clone();
    assert!(std::ptr::eq(
        manifest.objects().as_ptr(),
        cloned.objects().as_ptr()
    ));

    let object = &manifest.objects()[0];
    let cloned_object = object.clone();
    assert!(std::ptr::eq(
        object.bytes().as_ptr(),
        cloned_object.bytes().as_ptr()
    ));
}
