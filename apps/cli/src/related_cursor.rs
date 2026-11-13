//! Opaque transport for owner-sealed related-edge continuation cursors.
//!
//! Decoding does not authenticate a cursor. The owner verifies the seal and
//! the exact selection, revision, direction and credit before using it.

use backend_library::RelatedPageCursor;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

const PREFIX: &str = "re1-";
const MAX_TOKEN_BYTES: usize = 8192;

/// Encodes the complete owner-issued cursor as a bounded opaque CLI token.
pub fn encode(cursor: &RelatedPageCursor) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(cursor).map_err(|_| "cannot encode related cursor")?;
    let token = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
    if token.len() > MAX_TOKEN_BYTES {
        return Err("related cursor exceeds the token limit");
    }
    Ok(token)
}

/// Parses a canonical token. Its authority remains subject to owner verification.
pub fn decode(token: &str) -> Result<RelatedPageCursor, &'static str> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err("related cursor exceeds the token limit");
    }
    let encoded = token.strip_prefix(PREFIX).ok_or("invalid related cursor")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "invalid related cursor")?;
    let cursor: RelatedPageCursor =
        serde_json::from_slice(&bytes).map_err(|_| "invalid related cursor")?;
    if encode(&cursor)? != token {
        return Err("related cursor is not canonical");
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_owner_seal_and_rejects_noncanonical_json() {
        use backend_library::{
            RelatedDirection, RelatedEdgeKey, RelatedPageRequest, RelatedPageScope, RelatedRowKey,
            SemanticLinkKind, VerifiedRelatedPageRequest,
        };
        let request = RelatedPageRequest {
            scope: RelatedPageScope {
                root: [1; 32],
                source: [2; 32],
                source_selected: true,
                direction: RelatedDirection::Both,
                issued: 10,
                delivery: 5,
                windowed: true,
            },
            cursor: None,
        };
        let cursor = VerifiedRelatedPageRequest::verify(&request, [4; 32], &[3; 32])
            .unwrap()
            .issue_cursor(RelatedEdgeKey {
                from: RelatedRowKey::Symbol([2; 32]),
                to: RelatedRowKey::Symbol([5; 32]),
                relation: SemanticLinkKind::Calls,
            })
            .unwrap();
        let token = encode(&cursor).unwrap();
        let decoded = decode(&token).unwrap();
        assert_eq!(decoded, cursor);
        let successor = RelatedPageRequest {
            cursor: Some(decoded),
            ..request
        };
        assert!(VerifiedRelatedPageRequest::verify(&successor, [4; 32], &[3; 32]).is_ok());
        let pretty = serde_json::to_vec_pretty(&cursor).unwrap();
        assert!(decode(&format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(pretty))).is_err());
    }

    #[test]
    fn refuses_unbounded_or_malformed_tokens() {
        for token in ["", "re1-", "re1-e30=", "re1-e30", "wrong-e30"] {
            assert!(decode(token).is_err(), "accepted {token:?}");
        }
        assert!(decode(&format!("re1-{}", "A".repeat(MAX_TOKEN_BYTES))).is_err());
    }
}
