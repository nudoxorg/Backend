//! Bounded request admission and cancellation, independent of the Server lock.

use super::codec::{error_reply, json_depth_within, valid_id};
use backend_client::{RequestCancellationReason, RequestContext};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) const MAX_PENDING_REQUESTS: usize = 32;

#[derive(Clone, Eq, Hash, PartialEq)]
enum RequestId {
    String(String),
    Number(serde_json::Number),
}

impl RequestId {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::String(value) => Some(Self::String(value.clone())),
            Value::Number(value) => Some(Self::Number(value.clone())),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Pending {
    requests: HashMap<RequestId, RequestContext>,
    closed: bool,
}

/// Each transport session has its own registry; equal ids never cross sessions.
#[derive(Clone, Default)]
pub(crate) struct RequestRegistry(Arc<Mutex<Pending>>);

/// The registry entry and socket binding are retired together, including unwind.
pub(crate) struct RequestLease {
    registry: RequestRegistry,
    id: RequestId,
    pub(crate) context: RequestContext,
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        self.context.finish();
        let mut pending = self
            .registry
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending
            .requests
            .get(&self.id)
            .is_some_and(|current| current.same_request(&self.context))
        {
            pending.requests.remove(&self.id);
        }
    }
}

impl RequestRegistry {
    #[cfg(test)]
    pub(crate) fn in_flight_context(&self, id: &Value) -> Option<RequestContext> {
        let id = RequestId::from_value(id)?;
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .get(&id)
            .cloned()
    }

    /// Handles a well-formed cancellation notification before any Server lock.
    /// Malformed controls are ordinary notifications and have no authority.
    pub(crate) fn cancel_notification(&self, value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.contains_key("id")
            || object.get("method").and_then(Value::as_str) != Some("notifications/cancelled")
            || !json_depth_within(value)
        {
            return false;
        }
        let Some(params) = object.get("params").and_then(Value::as_object) else {
            return false;
        };
        if params
            .get("reason")
            .is_some_and(|reason| !reason.is_string())
        {
            return false;
        }
        let Some(id) = params.get("requestId").and_then(RequestId::from_value) else {
            return false;
        };
        let context = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .get(&id)
            .cloned();
        if let Some(context) = context {
            context.cancel(RequestCancellationReason::Explicit);
        }
        true
    }

    /// Registers valid request ids before queueing or connecting to the owner.
    pub(crate) fn admit(&self, value: &Value) -> Result<Option<RequestLease>, Value> {
        let Some(object) = value.as_object() else {
            return Ok(None);
        };
        let Some(id) = object.get("id") else {
            return Ok(None);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.get("method").and_then(Value::as_str).is_none()
            || !valid_id(id)
            || !json_depth_within(value)
        {
            return Ok(None);
        }
        let Some(key) = RequestId::from_value(id) else {
            return Ok(None);
        };
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.closed {
            return Err(error_reply(
                id.clone(),
                -32800,
                "MCP session is shutting down",
                None,
            ));
        }
        if pending.requests.contains_key(&key) {
            return Err(error_reply(
                id.clone(),
                -32600,
                "Request id is already in flight",
                None,
            ));
        }
        if pending.requests.len() >= MAX_PENDING_REQUESTS {
            return Err(error_reply(
                id.clone(),
                -32000,
                "MCP request capacity reached",
                None,
            ));
        }
        let context = RequestContext::new();
        if let Some(sender) = progress_sender() {
            context.set_preparation_observer(sender);
        }
        pending.requests.insert(key.clone(), context.clone());
        Ok(Some(RequestLease {
            registry: self.clone(),
            id: key,
            context,
        }))
    }

    pub(crate) fn close(&self, reason: RequestCancellationReason) {
        let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        pending.closed = true;
        for context in pending.requests.values() {
            context.cancel(reason);
        }
    }
}

/// Held by the HTTP response future; dropping it cancels its blocking task.
pub(crate) struct DisconnectGuard(Option<RequestContext>);

impl DisconnectGuard {
    pub(crate) const fn new(context: Option<RequestContext>) -> Self {
        Self(context)
    }
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for DisconnectGuard {
    fn drop(&mut self) {
        if let Some(context) = &self.0 {
            context.cancel(RequestCancellationReason::ClientDisconnected);
        }
    }
}

pub(crate) fn decode(input: &[u8]) -> Result<Value, Value> {
    serde_json::from_slice(input).map_err(|error| {
        error_reply(
            Value::Null,
            -32700,
            "Parse error",
            Some(serde_json::json!({ "detail": error.to_string() })),
        )
    })
}

pub(crate) fn cancelled_reply(id: Value, context: &RequestContext) -> Option<Value> {
    context.cancellation_reason().map(|reason| {
        error_reply(
            id,
            -32800,
            "Request cancelled",
            Some(serde_json::json!({"reason": reason.to_string()})),
        )
    })
}

// At most one bounded stderr worker per MCP process. A host may stop reading
// stderr indefinitely; that can block only this logger, never the query actor.
fn progress_sender() -> Option<std::sync::mpsc::SyncSender<backend_client::QueryPreparationProgress>>
{
    static SENDER: std::sync::OnceLock<
        Option<std::sync::mpsc::SyncSender<backend_client::QueryPreparationProgress>>,
    > = std::sync::OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (sender, receiver) =
                std::sync::mpsc::sync_channel::<backend_client::QueryPreparationProgress>(8);
            std::thread::Builder::new()
                .name("mcp-progress".to_owned())
                .spawn(move || {
                    use std::io::Write as _;
                    for progress in receiver {
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "search preparation: {}",
                            progress.state.as_str()
                        );
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .clone()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "request lifecycle laws require admitted fixture ids"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call"})
    }
    fn cancel(id: Value) -> Value {
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}})
    }
    fn lease(registry: &RequestRegistry, id: Value) -> RequestLease {
        registry
            .admit(&request(id))
            .expect("admitted")
            .expect("request lease")
    }

    #[test]
    fn exact_ids_unknown_ids_id_reuse_and_sessions_cannot_cross_cancel() {
        let a = RequestRegistry::default();
        let b = RequestRegistry::default();
        let number = lease(&a, json!(7));
        let text = lease(&a, json!("7"));
        let other_session = lease(&b, json!(7));
        assert!(a.cancel_notification(&cancel(json!(8))));
        assert!(number.context.check().is_ok());
        assert!(a.cancel_notification(&cancel(json!(7))));
        assert_eq!(
            number.context.cancellation_reason(),
            Some(RequestCancellationReason::Explicit)
        );
        assert!(text.context.check().is_ok());
        assert!(other_session.context.check().is_ok());
        assert!(
            a.admit(&request(json!(7))).is_err(),
            "id reuse while in flight is refused"
        );
        let old = number.context.clone();
        drop(number);
        let fresh = lease(&a, json!(7));
        old.cancel(RequestCancellationReason::Shutdown);
        assert!(
            fresh.context.check().is_ok(),
            "old lifetime cannot cancel reused id"
        );
        assert!(a.cancel_notification(&cancel(json!(7))));
        assert_eq!(
            fresh.context.cancellation_reason(),
            Some(RequestCancellationReason::Explicit)
        );
    }

    #[test]
    fn malformed_controls_never_acquire_cancellation_authority() {
        let registry = RequestRegistry::default();
        let live = lease(&registry, json!(1));
        for malformed in [
            json!({"jsonrpc":"1.0","method":"notifications/cancelled","params":{"requestId":1}}),
            json!({"jsonrpc":"2.0","id":2,"method":"notifications/cancelled","params":{"requestId":1}}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":null}}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"id":1}}),
        ] {
            assert!(!registry.cancel_notification(&malformed));
        }
        assert!(live.context.check().is_ok());
    }

    #[test]
    fn registry_capacity_shutdown_and_disconnect_guard_are_exact_and_bounded() {
        let registry = RequestRegistry::default();
        let leases = (0..MAX_PENDING_REQUESTS)
            .map(|id| lease(&registry, json!(id)))
            .collect::<Vec<_>>();
        assert!(
            registry
                .admit(&request(json!(MAX_PENDING_REQUESTS)))
                .is_err()
        );
        let explicit = &leases[0].context;
        explicit.cancel(RequestCancellationReason::Explicit);
        registry.close(RequestCancellationReason::Shutdown);
        assert_eq!(
            explicit.cancellation_reason(),
            Some(RequestCancellationReason::Explicit)
        );
        assert!(
            leases[1..]
                .iter()
                .all(|lease| lease.context.cancellation_reason()
                    == Some(RequestCancellationReason::Shutdown))
        );
        drop(leases);
        assert!(
            registry.admit(&request(json!(99))).is_err(),
            "closed session stays closed"
        );
        let abandoned = RequestContext::new();
        drop(DisconnectGuard::new(Some(abandoned.clone())));
        assert_eq!(
            abandoned.cancellation_reason(),
            Some(RequestCancellationReason::ClientDisconnected)
        );
        let completed = RequestContext::new();
        let mut guard = DisconnectGuard::new(Some(completed.clone()));
        guard.disarm();
        drop(guard);
        assert!(completed.check().is_ok());
    }
}
