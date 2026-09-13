//! Exercises the `heart-root` tests canonical-root-view contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Canonical root-view integration coverage.

use backend_version::{ContentId, ObjectDomain};
use backend_version::object::{ObjectKind, ObjectRef};
use heart_root::{
    GenerationRoot, RootBuildError, RootEntry, RootReadError, RootWriteError, ValidatedRoot,
};
use backend_version::schema::SchemaId;
use thiserror::Error;

#[derive(Debug, Error)]
enum CanonicalRootViewTestError {
    #[error(transparent)]
    Build(#[from] RootBuildError),
    #[error(transparent)]
    Read(#[from] RootReadError),
    #[error(transparent)]
    Write(#[from] RootWriteError),
    #[error("expected truncated root bytes")]
    ExpectedTruncated,
    #[error("expected trailing root bytes")]
    ExpectedTrailing,
}

fn entry(key: u8, parent: Option<u8>) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: u64::from(key).into(),
        parent: parent.map(u64::from).map(Into::into),
        object: ObjectRef {
            content: ContentId::from_digest([key; 32]),
            length: 0.into(),
            schema: SchemaId::Object,
            kind: ObjectKind::from(0),
        },
    }
}

#[test]
fn canonical_bytes_round_trip_and_identity_match() -> Result<(), CanonicalRootViewTestError> {
    let root = GenerationRoot::new(vec![entry(2, Some(1)), entry(1, None)])?;
    let mut bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut bytes)?;
    let borrowed = ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice())?;
    assert_eq!(borrowed.id, root.id);
    assert_eq!(borrowed.entry_count, 2.into());
    assert_eq!(borrowed.bytes, bytes.as_slice());
    Ok(())
}

#[test]
fn exact_extent_is_required() -> Result<(), CanonicalRootViewTestError> {
    let root = GenerationRoot::new(vec![entry(1, None)])?;
    let mut bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut bytes)?;
    let truncated = bytes
        .split_last()
        .map(|(_, truncated)| truncated)
        .ok_or(CanonicalRootViewTestError::ExpectedTruncated)?;
    let error = ValidatedRoot::<ObjectDomain>::try_from(truncated)
        .err()
        .ok_or(CanonicalRootViewTestError::ExpectedTruncated)?;
    if !matches!(error, RootReadError::Truncated { .. }) {
        return Err(CanonicalRootViewTestError::ExpectedTruncated);
    }
    bytes.push(0);
    if !matches!(
        ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice()),
        Err(RootReadError::TrailingBytes { .. })
    ) {
        return Err(CanonicalRootViewTestError::ExpectedTrailing);
    }
    Ok(())
}
