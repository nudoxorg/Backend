//! Proves that the interface-owned semantic byte owner cannot misstate public authority.

use heart_identity::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};
use interface_core::{SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot};

fn authority(bytes: &[u8]) -> SemanticImageAuthority {
    SemanticImageAuthority {
        identity: ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
            bytes,
        ),
        byte_len: u32::try_from(bytes.len()).expect("fixture bytes fit the public extent"),
    }
}

#[test]
fn snapshot_commits_no_owner_until_extent_and_identity_match() {
    let expected = b"semantic-image-a";
    let snapshot = SemanticImageSnapshot::try_from_reopened(authority(expected), expected)
        .expect("matching reopened bytes become one owned snapshot");
    assert_eq!(snapshot.authority, authority(expected));
    assert_eq!(snapshot.as_ref(), expected);

    let short = SemanticImageSnapshot::try_from_reopened(authority(expected), b"short")
        .expect_err("wrong extent cannot expose an owner");
    assert!(matches!(
        short,
        SemanticImageAccessError::Length {
            authority: observed,
            observed: 5,
        } if observed == authority(expected)
    ));

    let foreign = b"semantic-image-b";
    let mismatch = SemanticImageSnapshot::try_from_reopened(authority(expected), foreign)
        .expect_err("same-width foreign bytes cannot expose an owner");
    assert!(matches!(
        mismatch,
        SemanticImageAccessError::Identity {
            authority: observed_authority,
            observed,
        } if observed_authority == authority(expected) && observed == authority(foreign).identity
    ));
}
