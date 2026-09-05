//! Hostile source-span transport decoding checks.

use std::error::Error;

use heart_identity::IndexSnapshotId;
use interface_protocol::{
    MAX_UNTRUSTED_SOURCE_PATH_BYTES, UNTRUSTED_DOCUMENT_ID_BYTES, UntrustedDocumentId,
    UntrustedSourceSpan, UntrustedSourceSpanAuthorityError, UntrustedSourceSpanError,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn document(fill: u8) -> Result<UntrustedDocumentId, Box<dyn Error>> {
    let encoded = vec![fill; UNTRUSTED_DOCUMENT_ID_BYTES];
    UntrustedDocumentId::from_encoded_bytes(&encoded).map_err(Into::into)
}

#[test]
fn round_trips_non_utf8_span_with_snapshot_and_document_correlation() -> TestResult {
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"source-wire-snapshot");
    let span =
        UntrustedSourceSpan::new(b"src/\xff-entry.rs".to_vec(), 3, 7, snapshot, document(9)?)?;

    let encoded = serde_json::to_vec(&span)?;
    let decoded: UntrustedSourceSpan = serde_json::from_slice(&encoded)?;
    assert_eq!(decoded, span);
    assert_eq!(decoded.path(), b"src/\xff-entry.rs");
    assert_eq!(decoded.snapshot(), snapshot);
    assert_eq!(
        decoded.document().as_bytes(),
        &[9; UNTRUSTED_DOCUMENT_ID_BYTES]
    );
    Ok(())
}

#[test]
fn decode_rejects_reversed_foreign_or_oversized_transport_facts() -> TestResult {
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"accepted-source-snapshot");
    let span = UntrustedSourceSpan::new(b"source.rs".to_vec(), 3, 7, snapshot, document(4)?)?;

    let mut reversed = serde_json::to_value(&span)?;
    reversed["end"] = serde_json::json!(2_u32);
    assert!(serde_json::from_value::<UntrustedSourceSpan>(reversed).is_err());

    let mut foreign = serde_json::to_value(&span)?;
    foreign["snapshot"] =
        serde_json::json!(IndexSnapshotId::from_canonical_bytes(b"foreign").to_string());
    let foreign: UntrustedSourceSpan = serde_json::from_value(foreign)?;
    assert!(matches!(
        foreign.require_snapshot(snapshot),
        Err(UntrustedSourceSpanAuthorityError::ForeignSnapshot { expected, observed })
            if expected == snapshot && observed == foreign.snapshot()
    ));

    let oversized = UntrustedSourceSpan::new(
        vec![0; MAX_UNTRUSTED_SOURCE_PATH_BYTES + 1],
        0,
        0,
        snapshot,
        document(5)?,
    );
    assert_eq!(
        oversized,
        Err(UntrustedSourceSpanError::PathLength {
            actual: MAX_UNTRUSTED_SOURCE_PATH_BYTES + 1,
            maximum: MAX_UNTRUSTED_SOURCE_PATH_BYTES,
        })
    );

    let document = vec![7_u8; UNTRUSTED_DOCUMENT_ID_BYTES]
        .into_iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let oversized_path = vec![0_u8; MAX_UNTRUSTED_SOURCE_PATH_BYTES + 1]
        .into_iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let oversized_path_json = format!(
        r#"{{"path":[{oversized_path}],"start":0,"end":0,"snapshot":"{snapshot}","document":[{document}]}}"#
    );
    assert!(serde_json::from_str::<UntrustedSourceSpan>(&oversized_path_json).is_err());

    let oversized_snapshot = "x".repeat(73);
    let oversized_snapshot_json = format!(
        r#"{{"path":[1],"start":0,"end":0,"snapshot":"{oversized_snapshot}","document":[{document}]}}"#
    );
    assert!(serde_json::from_str::<UntrustedSourceSpan>(&oversized_snapshot_json).is_err());
    Ok(())
}

#[test]
fn decode_rejects_unknown_fields_and_wrong_document_width() -> TestResult {
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"source-wire-snapshot");
    let span = UntrustedSourceSpan::new(b"source.rs".to_vec(), 0, 1, snapshot, document(6)?)?;
    let mut unknown = serde_json::to_value(&span)?;
    unknown["candidate"] = serde_json::json!("raw-hit");
    assert!(serde_json::from_value::<UntrustedSourceSpan>(unknown).is_err());

    let mut wrong_width = serde_json::to_value(&span)?;
    wrong_width["document"] = serde_json::json!([1_u8, 2_u8]);
    assert!(serde_json::from_value::<UntrustedSourceSpan>(wrong_width).is_err());
    Ok(())
}
