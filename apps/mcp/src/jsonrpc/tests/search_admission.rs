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
        for query in [Value::Null, json!(""), json!(7)] {
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
