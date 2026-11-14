//! Revision-bound declaration source pages for the CLI.
//!
//! Cursor authentication uses the existing workspace authority; reading a
//! cursor never creates or rotates that authority. The selected symbol remains
//! a locator that the owner re-admits in the pinned view on every request.

use backend_client::{ClientError, Session};
use backend_library::{CommandReply, DeclarationSelector, SelectedSymbolId, SourceRangeRequest};
use backend_present::{Answer, DeclarationSourceRange, Fault, Operand, ProductView};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

const PREFIX: &str = "csr1-";
const PAYLOAD_BYTES: usize = 100;
const TAG_BYTES: usize = 16;
const PAGE_BYTES: u16 = 2048;
const DOMAIN: &[u8] = b"nudox.cli.source-range.v1\0";

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) project: String,
    pub(crate) selector: DeclarationSelector,
    pub(crate) cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Continuation {
    root: [u8; 32],
    source: [u8; 32],
    selected: [u8; 32],
    offset: u32,
}

fn context(request: &Request) -> Vec<u8> {
    let mut result = Vec::new();
    let mut part = |bytes: &[u8]| {
        result.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        result.extend_from_slice(bytes);
    };
    part(request.project.as_bytes());
    match &request.selector {
        DeclarationSelector::Coordinate(coordinate) => {
            part(b"coordinate");
            part(coordinate.as_bytes());
        }
        DeclarationSelector::Selected(symbol) => {
            part(b"selected");
            part(symbol.bytes());
        }
        DeclarationSelector::SelectedAt { symbol, coordinate } => {
            part(b"selected-at");
            part(symbol.bytes());
            part(coordinate.as_bytes());
        }
    }
    result
}

fn mac(key: &[u8; 32], context: &[u8], payload: &[u8]) -> [u8; TAG_BYTES] {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(DOMAIN);
    hasher.update(&(context.len() as u64).to_be_bytes());
    hasher.update(context);
    hasher.update(payload);
    let mut tag = [0; TAG_BYTES];
    tag.copy_from_slice(&hasher.finalize().as_bytes()[..TAG_BYTES]);
    tag
}

fn encode(key: &[u8; 32], context: &[u8], continuation: Continuation) -> String {
    let mut bytes = Vec::with_capacity(PAYLOAD_BYTES + TAG_BYTES);
    bytes.extend_from_slice(&continuation.root);
    bytes.extend_from_slice(&continuation.source);
    bytes.extend_from_slice(&continuation.selected);
    bytes.extend_from_slice(&continuation.offset.to_be_bytes());
    let tag = mac(key, context, &bytes);
    bytes.extend_from_slice(&tag);
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn decode(key: &[u8; 32], context: &[u8], token: &str) -> Option<Continuation> {
    if token.len() > 256 {
        return None;
    }
    let encoded = token.strip_prefix(PREFIX)?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    if bytes.len() != PAYLOAD_BYTES + TAG_BYTES || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return None;
    }
    let expected = mac(key, context, &bytes[..PAYLOAD_BYTES]);
    let mismatch = expected
        .iter()
        .zip(&bytes[PAYLOAD_BYTES..])
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b));
    if mismatch != 0 {
        return None;
    }
    let root = bytes[0..32].try_into().ok()?;
    let source = bytes[32..64].try_into().ok()?;
    let selected = bytes[64..96].try_into().ok()?;
    let offset = u32::from_be_bytes(bytes[96..100].try_into().ok()?);
    if offset == 0 || SelectedSymbolId::new(selected).is_none() {
        return None;
    }
    Some(Continuation {
        root,
        source,
        selected,
        offset,
    })
}

fn authority(paths: &backend_runtime::WorkspacePaths) -> Result<[u8; 32], Fault> {
    backend_engine::read_authority_secret(paths.authority_secret()).map_err(|error| {
        Fault::new(
            backend_present::FaultSlug::Endpoint,
            Operand::Argument("cursor".to_owned()),
            backend_present::Cause::new(
                backend_present::CauseSlug::Unconfigured,
                format!("source continuation authority is unavailable: {error}"),
            ),
            backend_present::Affordance::None,
        )
    })
}

fn protocol(detail: &str) -> Fault {
    Fault::from_client_error(
        &ClientError::Protocol(detail.to_owned()),
        Operand::Text("source".to_owned()),
    )
}

pub(crate) fn read(
    session: &mut Session,
    paths: &backend_runtime::WorkspacePaths,
    request: &Request,
) -> Result<Answer, Fault> {
    let context = context(request);
    let previous = match request.cursor.as_deref() {
        Some(token) => Some(decode(&authority(paths)?, &context, token).ok_or_else(|| {
            Fault::usage(
                "cursor",
                "invalid source continuation; restart the source read",
            )
        })?),
        None => None,
    };
    let (selector, root, range) = if let Some(previous) = previous {
        let selected = SelectedSymbolId::new(previous.selected)
            .ok_or_else(|| Fault::usage("cursor", "invalid selected source identity"))?;
        let selector = match &request.selector {
            DeclarationSelector::SelectedAt { coordinate, .. } => DeclarationSelector::SelectedAt {
                symbol: selected,
                coordinate: coordinate.clone(),
            },
            _ => DeclarationSelector::Selected(selected),
        };
        (
            selector,
            Some(backend_library::encode_id(&previous.root)),
            SourceRangeRequest {
                start: previous.offset,
                limit: PAGE_BYTES,
                source_identity: Some(previous.source),
            },
        )
    } else {
        let selector = match &request.selector {
            DeclarationSelector::Coordinate(text) => {
                let mut engine = crate::run::SessionEngine::new(session);
                DeclarationSelector::Coordinate(backend_present::resolve_declaration(
                    &mut engine,
                    text,
                    "source",
                )?)
            }
            selector => selector.clone(),
        };
        (
            selector,
            None,
            SourceRangeRequest {
                start: 0,
                limit: PAGE_BYTES,
                source_identity: None,
            },
        )
    };
    let reply = session
        .declaration_source_range(&selector, root.as_deref(), range)
        .map_err(|error| {
            let operand = Operand::Text("source".to_owned());
            match error {
                ClientError::StaleCursor
                | ClientError::CommandFailed(backend_library::CommandFailure::CursorMismatch) => {
                    Fault::from_client_error(&ClientError::StaleCursor, operand)
                }
                other => Fault::from_client_error(&other, operand),
            }
        })?;
    let CommandReply::Document(document) = reply.reply else {
        return Err(protocol("owner returned an unexpected source reply"));
    };
    let page = document
        .source_range
        .as_ref()
        .ok_or_else(|| protocol("owner omitted the requested source range"))?;
    if document.source_policy != page.source_policy {
        return Err(protocol(
            "owner source policy differs between document and range",
        ));
    }
    // Session has already admitted this reply; this is geometry only.
    if !page.matches_request_shape(range) {
        return Err(protocol("owner source range failed request admission"));
    }
    let next_cursor = if page.end < page.total {
        Some(encode(
            &authority(paths)?,
            &context,
            Continuation {
                root: *document.basis().as_bytes(),
                source: page.source_identity,
                selected: *document.symbol.as_bytes(),
                offset: page.end,
            },
        ))
    } else {
        None
    };
    let range = DeclarationSourceRange {
        revision: backend_library::encode_id(document.basis().as_bytes()),
        selected_symbol: backend_library::encode_id(document.symbol.as_bytes()),
        coordinate: document
            .selection
            .as_ref()
            .map(|selection| selection.coordinate().to_owned()),
        verified_at_read: true,
        source: page.clone(),
        next_cursor,
    };
    Ok(Answer::Product(Box::new(
        ProductView::assembled("source-range", Vec::new()).with_declaration_source_range(range),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_json_keeps_exact_unicode_crlf_and_terminal_newline() {
        let original = "first é漢🦀\r\nlast\r\n";
        let page = backend_library::SourceRangePage {
            source_policy: backend_library::SourcePolicyQualification::Unknown,
            text: original.to_owned(),
            start: 0,
            end: original.len() as u32,
            total: original.len() as u32,
            scope: backend_library::SourceRangeScope::NativePrimaryDeclaration,
            source_identity: [2; 32],
            owner_start: 13,
            owner_end: 13 + original.len() as u32,
        };
        let range = DeclarationSourceRange {
            revision: "01".repeat(32),
            selected_symbol: "03".repeat(32),
            coordinate: Some("pkg::file.ts:1::f".to_owned()),
            verified_at_read: true,
            source: page,
            next_cursor: None,
        };
        let answer = Answer::Product(Box::new(
            ProductView::assembled("source-range", Vec::new()).with_declaration_source_range(range),
        ));
        let value: serde_json::Value =
            serde_json::from_str(&crate::render::json(&answer)).expect("CLI JSON");
        assert_eq!(
            value["declaration_source_range"]["source"]["text"]
                .as_str()
                .expect("raw text")
                .as_bytes(),
            original.as_bytes()
        );
        assert_eq!(value["declaration_source_range"]["verified_at_read"], true);
        assert_eq!(
            value["declaration_source_range"]["source"]["scope"],
            "native_primary_declaration"
        );
        let human = backend_present::text::answer(&answer, backend_present::Theme::plain());
        assert!(human.contains("native primary declaration span"));
        assert!(human.contains("ignore-policy stability is not established"));
        let markdown = crate::render::markdown_text(&answer);
        assert!(markdown.contains("native primary declaration span"));
        assert!(markdown.contains("ignore-policy stability is not established"));
        assert!(
            value["declaration_source_range"]
                .get("nextCursor")
                .is_none()
        );
    }

    #[test]
    fn cursor_binds_project_selector_revision_source_and_offset() {
        let key = [7; 32];
        let request = Request {
            project: "/workspace/a".to_owned(),
            selector: DeclarationSelector::Coordinate("pkg::f".to_owned()),
            cursor: None,
        };
        let value = Continuation {
            root: [1; 32],
            source: [2; 32],
            selected: [3; 32],
            offset: 2048,
        };
        let token = encode(&key, &context(&request), value);
        assert!(token.len() < 180);
        assert_eq!(decode(&key, &context(&request), &token), Some(value));
        let mut foreign = request.clone();
        foreign.project = "/workspace/b".to_owned();
        assert!(decode(&key, &context(&foreign), &token).is_none());
        foreign = request.clone();
        foreign.selector = DeclarationSelector::Coordinate("pkg::g".to_owned());
        assert!(decode(&key, &context(&foreign), &token).is_none());
        for index in [0, 32, 64, 99] {
            let mut raw = URL_SAFE_NO_PAD
                .decode(token.strip_prefix(PREFIX).expect("prefix"))
                .expect("base64");
            raw[index] ^= 1;
            let tampered = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(raw));
            assert!(decode(&key, &context(&request), &tampered).is_none());
        }
        assert!(decode(&[8; 32], &context(&request), &token).is_none());
    }

    #[test]
    fn legacy_scope_remains_unknown_in_cli_rendering() {
        let page = backend_library::SourceRangePage {
            source_policy: backend_library::SourcePolicyQualification::Unknown,
            text: "legacy".to_owned(),
            start: 0,
            end: 6,
            total: 6,
            scope: backend_library::SourceRangeScope::Unknown,
            source_identity: [2; 32],
            owner_start: 0,
            owner_end: 6,
        };
        let mut legacy = serde_json::to_value(&page).expect("legacy page JSON");
        legacy
            .as_object_mut()
            .expect("legacy object")
            .remove("scope");
        let page: backend_library::SourceRangePage =
            serde_json::from_value(legacy).expect("legacy page admission");
        let answer = Answer::Product(Box::new(
            ProductView::assembled("source-range", Vec::new()).with_declaration_source_range(
                DeclarationSourceRange {
                    revision: "01".repeat(32),
                    selected_symbol: "03".repeat(32),
                    coordinate: None,
                    verified_at_read: true,
                    source: page,
                    next_cursor: None,
                },
            ),
        ));
        let value: serde_json::Value =
            serde_json::from_str(&crate::render::json(&answer)).expect("CLI JSON");
        assert_eq!(
            value["declaration_source_range"]["source"]["scope"],
            "unknown"
        );
        let human = backend_present::text::answer(&answer, backend_present::Theme::plain());
        assert!(human.contains("Scope unknown"));
        assert!(!human.contains("native primary declaration span"));
        let markdown = crate::render::markdown_text(&answer);
        assert!(markdown.contains("Scope unknown"));
        assert!(!markdown.contains("native primary declaration span"));
    }

    #[test]
    fn same_file_cursor_authenticates_selected_declaration() {
        let key = [7; 32];
        let first_id = SelectedSymbolId::new([3; 32]).expect("first symbol");
        let second_id = SelectedSymbolId::new([4; 32]).expect("second symbol");
        let first = Request {
            project: "/project".to_owned(),
            selector: DeclarationSelector::SelectedAt {
                symbol: first_id,
                coordinate: "/project::same.ts:1::alpha".to_owned(),
            },
            cursor: None,
        };
        let continuation = Continuation {
            root: [1; 32],
            source: [2; 32],
            selected: [3; 32],
            offset: 2048,
        };
        let token = encode(&key, &context(&first), continuation);
        let other_declaration = Request {
            selector: DeclarationSelector::SelectedAt {
                symbol: second_id,
                coordinate: "/project::same.ts:201::bravo".to_owned(),
            },
            ..first.clone()
        };
        let crossed_identity = Request {
            selector: DeclarationSelector::SelectedAt {
                symbol: second_id,
                coordinate: "/project::same.ts:1::alpha".to_owned(),
            },
            ..first.clone()
        };
        assert_eq!(decode(&key, &context(&first), &token), Some(continuation));
        assert!(decode(&key, &context(&other_declaration), &token).is_none());
        assert!(decode(&key, &context(&crossed_identity), &token).is_none());
    }
}
