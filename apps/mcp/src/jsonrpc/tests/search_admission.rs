//! Search admission precedes connection to the local owner.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct SearchAdmissionEndpoint {
    connects: Arc<AtomicUsize>,
    health: Arc<AtomicUsize>,
}

impl reconnect::Endpoint for SearchAdmissionEndpoint {
    type Product = Fake;

    fn connect(&mut self, previous: Option<Fake>) -> Result<Fake, ClientError> {
        assert!(previous.is_none(), "first valid request opens the session once");
        self.connects.fetch_add(1, Ordering::Relaxed);
        Ok(Fake {
            health_calls: Some(self.health.clone()),
            ..Fake::default()
        })
    }
}

fn ready_search_server(
    connects: Arc<AtomicUsize>,
    health: Arc<AtomicUsize>,
) -> Server<reconnect::Reconnecting<SearchAdmissionEndpoint>> {
    let mut server = Server::with_authority(
        reconnect::Reconnecting::disconnected(SearchAdmissionEndpoint { connects, health }),
        PROJECT.to_owned(),
        [9; 32],
    );
    server
        .handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"search-admission","version":"1"}}}"#)
        .expect("initialize is local");
    assert!(server
        .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .is_none());
    server
}

#[test]
fn invalid_search_is_typed_before_owner_and_same_session_reads_still_work() {
    for forwarded in [false, true] {
        for query in [Value::Null, json!(""), json!(" \t\n"), json!(7)] {
            let connects = Arc::new(AtomicUsize::new(0));
            let health = Arc::new(AtomicUsize::new(0));
            let mut server = ready_search_server(connects.clone(), health.clone());
            let search_arguments = json!({"query":query});
            let params = if forwarded {
                json!({"name":"backend.call", "arguments":{"name":"backend.search", "arguments":search_arguments}})
            } else {
                json!({"name":"backend.search", "arguments":search_arguments})
            };
            let invalid = server
                .handle_value(
                    json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":params}),
                    None,
                )
                .expect("invalid search reply");
            let result = &invalid["result"];
            assert_eq!(result["isError"], true, "{invalid}");
            assert_eq!(result["structuredContent"]["answer"], "fault", "{invalid}");
            assert_eq!(result["structuredContent"]["slug"], "usage", "{invalid}");
            assert_eq!(result["structuredContent"]["operand"], "query", "{invalid}");
            if query.as_str().is_some_and(|text| !text.is_empty()) {
                let message = text_of(result);
                assert!(message.contains("non-whitespace character"), "{invalid}");
                assert!(!message.contains("indexing"), "{invalid}");
            }
            assert_eq!(connects.load(Ordering::Relaxed), 0, "no owner connect: {invalid}");

            let status = server
                .handle_value(
                    json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"backend.status", "arguments":{}}}),
                    None,
                )
                .expect("same-session status");
            assert_eq!(status["result"]["isError"], false, "{status}");
            let search = server
                .handle_value(
                    json!({"jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{"name":"backend.search", "arguments":{"query":"ferris"}}}),
                    None,
                )
                .expect("same-session search");
            assert_eq!(search["result"]["isError"], false, "{search}");
            assert!(
                !search["result"]["structuredContent"]["records"]
                    .as_array()
                    .expect("fixture search rows")
                    .is_empty(),
                "{search}"
            );
            assert_eq!(connects.load(Ordering::Relaxed), 1, "reads share one session");
            assert!(health.load(Ordering::Relaxed) > 0);
        }
    }
}

#[test]
fn physical_crlf_search_previews_match_lf_on_direct_and_registered_mcp_routes() {
    let unix = "pub fn ferris(\n  pattern: &str, // keep \\r\\n\n) -> Beacon";
    let windows = unix.replace('\n', "\r\n");
    let expected = unix.replace('\n', "\\n");
    for signature in [unix, windows.as_str()] {
        for detail in ["summary", "standard", "full"] {
            for forwarded in [false, true] {
                let row = declaration_row().with_signature(signature).with_excerpt(
                    backend_library::SourceExcerpt::captured(
                        signature,
                        backend_library::SourceExcerptExtent::Complete,
                    )
                    .expect("exact captured declaration"),
                );
                let native = root(vec![package_row(), module_row(), row]);
                let original = native.canonical_relation_bytes().expect("native row preimage");
                let cursor = backend_library::Cursor::for_view_root(&native);
                let catalog = backend_library::Library::from_view(native, cursor)
                    .expect("immutable native fixture catalog");
                let mut server = ready(Fake {
                    page_catalog: Some(catalog),
                    ..Fake::default()
                });
                let arguments = json!({"query":"ferris", "detail":detail});
                let result = if forwarded {
                    call(&mut server, "backend.call", &json!({
                        "name":"backend.search", "arguments":arguments,
                    }))
                } else {
                    call(&mut server, "backend.search", &arguments)
                };
                assert_eq!(result["isError"], false, "{result}");
                let records = result["structuredContent"]["records"]
                    .as_array().expect("real native search rows");
                assert_eq!(records.len(), 1, "{result}");
                assert_eq!(records[0]["identity"]["coordinate"], DECLARATION);
                if detail == "summary" {
                    assert!(
                        records[0].get("signature").is_none(),
                        "summary intentionally omits signature payloads: {result}",
                    );
                } else {
                    assert_eq!(records[0]["signature"], expected);
                }
                assert!(expected.contains(r"\r\n"), "authored escapes remain literal");
                assert_eq!(
                    server.product.page_catalog.as_ref().expect("same catalog")
                        .view().canonical_relation_bytes().expect("native preimage after display"),
                    original,
                    "rendering cannot rewrite captured signature/source or native row identity"
                );
            }
        }
    }
}
