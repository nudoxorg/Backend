//! Opt-in packed Search request and opaque producer receipt. Count-only
//! commands and their continuations keep their existing grammar.

use crate::{PackedPageCheckpoint, PagePackingError, PagePackingPolicy, PagePackingProfile};
use serde::{Deserialize, Serialize};

/// Per-call response values, deliberately excluded from stable query identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackedCallContext {
    /// Shared typed CLI JSON response.
    CliJson,
    /// Complete MCP response, including its current ID and token expiry.
    Mcp {
        /// Actual JSON-RPC response ID.
        id: serde_json::Value,
        /// Expiry selected once for this response's outer MCP token.
        expiry: u64,
    },
}

/// An original packing policy plus one dynamic response context and optional
/// authenticated predecessor. Neither a legacy cursor nor a lowered row
/// ceiling can stand in for this explicit request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedQueryRequest {
    policy: PagePackingPolicy,
    call: PackedCallContext,
    predecessor: Option<String>,
}

impl PackedQueryRequest {
    /// Admits one bounded call context. Token authentication and exact source
    /// range reopening belong to the execution owner, after this syntax check.
    pub fn new(
        policy: PagePackingPolicy,
        call: PackedCallContext,
        predecessor: Option<String>,
    ) -> Result<Self, PagePackingError> {
        match (&call, policy.profile()) {
            (PackedCallContext::CliJson, PagePackingProfile::CliJsonV1) => {}
            (PackedCallContext::Mcp { id, expiry }, PagePackingProfile::McpToolV1)
                if *expiry > 0
                    && matches!(
                        id,
                        serde_json::Value::Null
                            | serde_json::Value::Number(_)
                            | serde_json::Value::String(_)
                    ) => {}
            _ => return Err(PagePackingError::Context),
        }
        if predecessor
            .as_ref()
            .is_some_and(|token| !token_shape(token))
        {
            return Err(PagePackingError::Token);
        }
        Ok(Self {
            policy,
            call,
            predecessor,
        })
    }

    /// Stable original policy.
    #[must_use]
    pub const fn policy(&self) -> PagePackingPolicy {
        self.policy
    }
    /// Actual response context for this call.
    #[must_use]
    pub const fn call(&self) -> &PackedCallContext {
        &self.call
    }
    /// Opaque predecessor; it has no authority before owner authentication.
    #[must_use]
    pub fn predecessor(&self) -> Option<&str> {
        self.predecessor.as_deref()
    }
}

fn token_shape(token: &str) -> bool {
    if token.len() != crate::PACKED_CHECKPOINT_TOKEN_BYTES {
        return false;
    }
    let Some(body) = token.strip_prefix("pp1-") else {
        return false;
    };
    let Some((claims, signature)) = body.split_once('-') else {
        return false;
    };
    !claims.is_empty()
        && signature.len() == 64
        && claims
            .bytes()
            .chain(signature.bytes())
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Opaque owner-issued checkpoint beside a producer-certified Search snapshot.
/// Decoding this receipt never turns its bytes into a cursor or identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PackedQueryReceipt {
    token: String,
    more: bool,
    rows: u16,
    bytes: u32,
}

impl PackedQueryReceipt {
    /// Issues a receipt only from a fully checked producer checkpoint.
    #[must_use]
    pub fn issue(
        checkpoint: &PackedPageCheckpoint,
        authenticate: impl FnOnce(&[u8]) -> [u8; 32],
    ) -> Self {
        Self {
            token: checkpoint.encode_token(authenticate),
            more: checkpoint.stop().next_row().is_some(),
            rows: checkpoint.emitted(),
            bytes: checkpoint.measured_bytes(),
        }
    }
    /// Owner checkpoint token, also retained for Complete receipt admission.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Whether the owner observed a successor after this exact prefix.
    #[must_use]
    pub const fn more(&self) -> bool {
        self.more
    }
    /// Exact number of emitted rows.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }
    /// Complete response bytes measured with the current dynamic context.
    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }
}

impl<'de> Deserialize<'de> for PackedQueryReceipt {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Claims {
            token: String,
            more: bool,
            rows: u16,
            bytes: u32,
        }
        let claims = Claims::deserialize(deserializer)?;
        if PackedPageCheckpoint::receipt_metadata(&claims.token)
            != Ok((claims.rows, claims.more, claims.bytes))
        {
            return Err(serde::de::Error::custom("invalid packed Search receipt"));
        }
        Ok(Self {
            token: claims.token,
            more: claims.more,
            rows: claims.rows,
            bytes: claims.bytes,
        })
    }
}

/// Closed producer failure. No variant publishes rows or a continuation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackedQueryFailure {
    /// The actual first un-emitted row exceeds its complete applicable response.
    ItemExceedsWireCredit {
        ordinal: u64,
        required_bytes: u64,
        credit_bytes: u32,
    },
    /// A canonical no-record envelope already exceeds the original credit.
    /// `required_bytes` is a proved lower bound; source rows and the actual
    /// semantic status may require additional response bytes.
    EnvelopeExceedsWireCredit {
        required_bytes: u64,
        credit_bytes: u32,
    },
}
impl PackedQueryFailure {
    pub(crate) fn admit(&self) -> Result<(), String> {
        let (required, credit) = match self {
            Self::ItemExceedsWireCredit {
                required_bytes,
                credit_bytes,
                ..
            }
            | Self::EnvelopeExceedsWireCredit {
                required_bytes,
                credit_bytes,
            } => (*required_bytes, *credit_bytes),
        };
        if credit == 0 || credit > crate::MAX_PACKED_WIRE_BYTES || required <= u64::from(credit) {
            return Err("invalid packed query credit failure".to_owned());
        }
        Ok(())
    }
}
impl core::fmt::Display for PackedQueryFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ItemExceedsWireCredit {
                ordinal,
                required_bytes,
                credit_bytes,
            } => write!(
                formatter,
                "item at ordinal {ordinal} requires {required_bytes} complete response bytes, above the {credit_bytes} byte credit"
            ),
            Self::EnvelopeExceedsWireCredit {
                required_bytes,
                credit_bytes,
            } => write!(
                formatter,
                "response envelope requires at least {required_bytes} bytes, above the {credit_bytes} byte credit"
            ),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        Command, CommandDto, CommandFailure, CommandReply, Cursor, PagePackingDetail,
        PagePackingScope, Query, QueryLimit, QueryPageRecipe, ReplyDto,
    };
    use serde_json::json;
    fn query(rows: u16, detail: PagePackingDetail, id: serde_json::Value, expiry: u64) -> Query {
        let owner = Cursor::new();
        let rows = QueryLimit::new(rows).expect("original credit");
        let policy = PagePackingPolicy::new(
            rows,
            crate::MAX_PACKED_WIRE_BYTES,
            detail,
            PagePackingProfile::McpToolV1,
            PagePackingScope::StdioJsonRpcLine,
        )
        .expect("policy");
        Query::new("self", owner.root(), rows)
            .with_packing(
                PackedQueryRequest::new(policy, PackedCallContext::Mcp { id, expiry }, None)
                    .expect("call"),
            )
            .expect("query")
    }
    #[test]
    fn recipe_binds_original_policy_while_dynamic_response_values_change() {
        let root = Cursor::new().root();
        let first = query(200, PagePackingDetail::Full, json!(1), 100);
        let dynamic = query(200, PagePackingDetail::Full, json!("changed-é-\\\""), 999);
        let identity = QueryPageRecipe::search(root, &first).identity();
        assert_eq!(identity, QueryPageRecipe::search(root, &dynamic).identity());
        assert_ne!(
            identity,
            QueryPageRecipe::search(root, &query(199, PagePackingDetail::Full, json!(1), 100))
                .identity()
        );
        assert_ne!(
            identity,
            QueryPageRecipe::search(root, &query(200, PagePackingDetail::Summary, json!(1), 100))
                .identity()
        );
        let legacy = Query::new("self", root, QueryLimit::new(200).expect("credit"));
        assert_ne!(identity, QueryPageRecipe::search(root, &legacy).identity());
    }
    #[test]
    fn mixed_legacy_cursor_and_packing_is_refused_at_shared_typed_admission() {
        let packed = query(200, PagePackingDetail::Full, json!(1), 100).with_cursor(Cursor::new());
        assert_eq!(
            crate::admit_request(&CommandDto::new(1, Command::Search(packed))),
            Err(crate::RequestAdmissionError::InvalidPacking)
        );
    }
    #[test]
    fn packed_calls_preserve_the_existing_text_bound_without_copying_mac_context() {
        let owner = Cursor::new();
        let packing = query(200, PagePackingDetail::Full, json!("current-id"), 100)
            .packing()
            .expect("packing")
            .clone();
        let text = format!("{}q", "\0".repeat(crate::MAX_COMMAND_TEXT - 1));
        let query = Query::new(&text, owner.root(), QueryLimit::new(200).expect("credit"))
            .with_packing(packing.clone())
            .expect("same original policy");
        let request = CommandDto::new(1, Command::Search(query)).with_certificate(
            crate::WireCertificate::new().with_claim(crate::WireClaim::RootCommitment {
                schema: crate::WireSchema::ViewRelation,
                id: crate::encode_id(owner.root().as_bytes()),
            }),
        );
        crate::admit_request(&request).expect("exact pre-existing text ceiling");
        let bytes = crate::encode_command_body(&request).expect("bounded lower frame");
        assert!(
            bytes.len() > crate::MAX_COMMAND_TEXT * 5,
            "escaped text is larger than raw text"
        );
        assert_eq!(
            crate::decode_command_body_for_owner(&bytes, owner).expect("actual strict owner wire"),
            request
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("wire object");
        let call = &mut value["command"]["data"]["packing"]["call"];
        assert!(call.is_object(), "actual closed call DTO");
        call["cursor_context"] = json!([]);
        assert!(
            crate::decode_command_body_for_owner(
                &serde_json::to_vec(&value).expect("unknown field"),
                owner
            )
            .is_err()
        );
        let too_long = Query::new(
            format!("{text}q"),
            owner.root(),
            QueryLimit::new(200).expect("credit"),
        )
        .with_packing(packing)
        .expect("same policy");
        assert_eq!(
            crate::admit_request(&CommandDto::new(1, Command::Search(too_long))),
            Err(crate::RequestAdmissionError::TextTooLarge)
        );
    }
    #[test]
    fn packed_calls_preserve_scalar_ids_under_the_existing_lower_frame_bound() {
        let owner = Cursor::new();
        let id = json!("\0".repeat(8193));
        let query = query(200, PagePackingDetail::Full, id.clone(), 100);
        let policy = query.packing().expect("packing").policy();
        let request = CommandDto::new(1, Command::Search(query)).with_certificate(
            crate::WireCertificate::new().with_claim(crate::WireClaim::RootCommitment {
                schema: crate::WireSchema::ViewRelation,
                id: crate::encode_id(owner.root().as_bytes()),
            }),
        );
        crate::admit_request(&request).expect("unchanged scalar ID admission");
        let bytes = crate::encode_command_body(&request).expect("unchanged bounded lower frame");
        assert!(bytes.len() > crate::MAX_PACKED_WIRE_BYTES as usize);
        let decoded = crate::decode_command_body_for_owner(&bytes, owner)
            .expect("actual strict owner wire retains complete escaped ID");
        assert_eq!(decoded, request);
        let Command::Search(query) = decoded.command else {
            panic!("same Search command");
        };
        assert_eq!(
            query.packing().expect("packing").call(),
            &PackedCallContext::Mcp { id, expiry: 100 }
        );
        for invalid in [json!(true), json!([]), json!({})] {
            assert!(
                PackedQueryRequest::new(
                    policy,
                    PackedCallContext::Mcp {
                        id: invalid,
                        expiry: 100
                    },
                    None
                )
                .is_err()
            );
        }
        assert!(
            PackedQueryRequest::new(
                policy,
                PackedCallContext::Mcp {
                    id: json!(1),
                    expiry: 0
                },
                None
            )
            .is_err()
        );
    }
    #[test]
    fn packed_receipts_cannot_be_serialized_or_imported_on_another_reply_family() {
        let query = query(200, PagePackingDetail::Full, json!(1), 100);
        let context = crate::PackedPageContext::new(
            Cursor::new(),
            query.basis(),
            QueryPageRecipe::search(Cursor::new().root(), &query).identity(),
            query.packing().expect("policy").policy(),
        )
        .expect("context");
        let checkpoint = crate::PackedPrefixCommitment::new(context, 0)
            .checkpoint(crate::PackedPageStop::Complete, 1)
            .expect("complete empty receipt");
        let receipt = PackedQueryReceipt::issue(&checkpoint, |_| [1; 32]);
        let reply = ReplyDto::new(1, CommandReply::Failed(CommandFailure::NotFound));
        assert!(serde_json::to_vec(&reply.clone().with_packed_query(receipt.clone())).is_err());
        let mut value = serde_json::to_value(reply).expect("ordinary failure");
        value["packed_query"] = serde_json::to_value(receipt).expect("opaque receipt");
        assert!(
            crate::decode_reply_body(&serde_json::to_vec(&value).expect("wrong family wire"))
                .is_err()
        );
    }
    #[test]
    fn receipt_metadata_must_match_canonical_token_claims_without_conferring_authority() {
        let query = query(200, PagePackingDetail::Full, json!(1), 100);
        let context = crate::PackedPageContext::new(
            Cursor::new(),
            query.basis(),
            QueryPageRecipe::search(Cursor::new().root(), &query).identity(),
            query.packing().expect("policy").policy(),
        )
        .expect("context");
        let mut prefix = crate::PackedPrefixCommitment::new(context, 0);
        prefix.push(0, [1; 32]).expect("one row");
        let checkpoint = prefix
            .checkpoint(crate::PackedPageStop::WireCreditReached([2; 32]), 1234)
            .expect("checked checkpoint");
        let receipt = PackedQueryReceipt::issue(&checkpoint, |_| [0; 32]);
        let original = serde_json::to_value(&receipt).expect("receipt");
        assert_eq!(
            serde_json::from_value::<PackedQueryReceipt>(original.clone()).expect("syntax only"),
            receipt
        );
        assert_eq!(
            PackedPageCheckpoint::decode_token_against(receipt.token(), context, |_, _| false),
            Err(PagePackingError::Authentication),
            "syntax admission never verifies a MAC"
        );
        for changed in [
            {
                let mut value = original.clone();
                value["more"] = json!(false);
                value["bytes"] = json!(1200);
                value
            },
            {
                let mut value = original.clone();
                value["rows"] = json!(2);
                value
            },
            {
                let mut value = original.clone();
                value["bytes"] = json!(1235);
                value
            },
        ] {
            assert!(
                serde_json::from_value::<PackedQueryReceipt>(changed).is_err(),
                "redundant metadata cannot contradict the token"
            );
        }
        let claims_hex_bytes = receipt.token().len() - 4 - 1 - 64;
        let stop_at = 4 + claims_hex_bytes - (1 + 32 + 4) * 2;
        for (at, hex) in [
            (stop_at, "03"),   // Unknown stop.
            (stop_at, "00"),   // Complete cannot retain a next-row witness.
            (4, "ff"),         // Unsupported checkpoint schema.
            (4 + 2 * 2, "ff"), // Unsupported owner cursor schema.
        ] {
            let mut value = original.clone();
            let mut token = receipt.token().to_owned();
            token.replace_range(at..at + 2, hex);
            value["token"] = json!(token);
            assert!(
                serde_json::from_value::<PackedQueryReceipt>(value).is_err(),
                "noncanonical claim shape"
            );
        }
    }
    #[test]
    fn wire_credit_faults_stay_closed_and_bind_the_actual_request_credit() {
        let query = query(200, PagePackingDetail::Full, json!(1), 100);
        let request = CommandDto::new(1, Command::Search(query));
        for failure in [
            PackedQueryFailure::ItemExceedsWireCredit {
                ordinal: 17,
                required_bytes: 50_000,
                credit_bytes: 49_152,
            },
            PackedQueryFailure::EnvelopeExceedsWireCredit {
                required_bytes: 50_000,
                credit_bytes: 49_152,
            },
        ] {
            let reply = ReplyDto::new(
                1,
                CommandReply::Failed(CommandFailure::PagePacking(failure)),
            );
            let bytes = serde_json::to_vec(&reply).expect("closed producer failure");
            let decoded = crate::decode_reply_body(&bytes).expect("actual strict failure codec");
            assert_eq!(decoded, reply);
            crate::admit_reply(&request, &decoded).expect("same original output credit");
            assert!(crate::admit_reply(&CommandDto::new(1, Command::Health), &decoded).is_err());
            let mut value = serde_json::to_value(&reply).expect("failure value");
            value["reply"]["data"]["data"]["credit_bytes"] = json!(0);
            assert!(
                crate::decode_reply_body(&serde_json::to_vec(&value).expect("bad failure"))
                    .is_err()
            );
        }
    }
}
