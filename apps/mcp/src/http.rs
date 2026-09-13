//! Authenticated MCP over a bounded Axum HTTP endpoint.
//!
//! The transport owns connection/session concerns only. Every request still
//! enters the same JSON-RPC server and typed product session as stdio MCP.

use crate::jsonrpc::Server;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use backend_client::Session;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MCP_PATH: &str = "/mcp";
const TOKEN_ENV: &str = "BACKEND_MCP_TOKEN";
const MAX_SESSIONS: usize = 64;
const MAX_IN_FLIGHT: usize = 64;
const SESSION_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");

/// A socket address proven to be local before the listener is opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LoopbackBind(SocketAddr);

impl LoopbackBind {
    pub(super) fn new(address: SocketAddr) -> Result<Self, String> {
        address
            .ip()
            .is_loopback()
            .then_some(Self(address))
            .ok_or_else(|| "--http must bind a loopback address".to_owned())
    }
}

impl Default for LoopbackBind {
    fn default() -> Self {
        Self(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))
    }
}

#[derive(Clone)]
struct BearerToken(Arc<str>);

impl BearerToken {
    fn load() -> Result<Self, String> {
        match std::env::var(TOKEN_ENV) {
            Ok(value) => Self::parse(value),
            Err(std::env::VarError::NotPresent) => Self::generate(),
            Err(error) => Err(format!("{TOKEN_ENV}: {error}")),
        }
    }

    fn parse(value: String) -> Result<Self, String> {
        let valid =
            (16..=256).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic());
        valid
            .then(|| Self(value.into()))
            .ok_or_else(|| format!("{TOKEN_ENV} must contain 16-256 visible ASCII bytes"))
    }

    fn generate() -> Result<Self, String> {
        let mut entropy = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut entropy))
            .map_err(|error| format!("could not generate {TOKEN_ENV}: {error}"))?;
        Ok(Self(hex(&entropy).into()))
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(candidate) = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return false;
        };
        constant_time_eq(candidate.as_bytes(), self.0.as_bytes())
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SessionId(String);

impl SessionId {
    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        headers
            .get(&SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .map(|value| Self(value.to_owned()))
    }

    fn header_value(&self) -> Option<HeaderValue> {
        HeaderValue::from_str(&self.0).ok()
    }
}

struct Sessions {
    endpoint: PathBuf,
    project: String,
    token: BearerToken,
    next_id: AtomicU64,
    live: Mutex<HashMap<SessionId, Arc<Mutex<Server<Session>>>>>,
    request_capacity: Arc<tokio::sync::Semaphore>,
}

impl Sessions {
    fn dispatch(&self, headers: &HeaderMap, body: &[u8]) -> Response {
        if !self.token.authorizes(headers) {
            return unauthorized();
        }

        let parsed = serde_json::from_slice::<Value>(body);
        let is_initialize = parsed
            .as_ref()
            .ok()
            .and_then(|value| value.get("method"))
            .and_then(Value::as_str)
            == Some("initialize");

        let supplied = SessionId::from_headers(headers);
        if is_initialize && supplied.is_none() {
            return self.initialize(body);
        }

        let Some(session_id) = supplied else {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                -32001,
                "Mcp-Session-Id is required",
            );
        };
        let Ok(sessions) = self.live.lock() else {
            return rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "session store unavailable",
            );
        };
        let Some(server) = sessions.get(&session_id).cloned() else {
            return rpc_error(StatusCode::NOT_FOUND, -32001, "MCP session not found");
        };
        drop(sessions);
        let Ok(mut server) = server.lock() else {
            return rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "MCP session unavailable",
            );
        };
        rpc_response(server.handle(body), Some(&session_id))
    }

    fn initialize(&self, body: &[u8]) -> Response {
        let product = match Session::connect(&self.endpoint) {
            Ok(product) => product,
            Err(error) => return rpc_error(StatusCode::BAD_GATEWAY, -32603, &error.to_string()),
        };
        let mut server = Server::new(product, self.project.clone());
        let reply = server.handle(body);
        let initialized = reply
            .as_ref()
            .and_then(|value| value.get("result"))
            .is_some();
        let session_id = self.next_session_id();
        if initialized {
            let Ok(mut sessions) = self.live.lock() else {
                return rpc_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    -32603,
                    "session store unavailable",
                );
            };
            if sessions.len() >= MAX_SESSIONS {
                return rpc_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    -32000,
                    "MCP session capacity reached",
                );
            }
            sessions.insert(session_id.clone(), Arc::new(Mutex::new(server)));
            rpc_response(reply, Some(&session_id))
        } else {
            rpc_response(reply, None)
        }
    }

    fn remove(&self, headers: &HeaderMap) -> Response {
        if !self.token.authorizes(headers) {
            return unauthorized();
        }
        let Some(session_id) = SessionId::from_headers(headers) else {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                -32001,
                "Mcp-Session-Id is required",
            );
        };
        match self.live.lock() {
            Ok(mut sessions) => match sessions.remove(&session_id) {
                Some(_) => StatusCode::NO_CONTENT.into_response(),
                None => rpc_error(StatusCode::NOT_FOUND, -32001, "MCP session not found"),
            },
            Err(_) => rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "session store unavailable",
            ),
        }
    }

    fn next_session_id(&self) -> SessionId {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut input = Vec::with_capacity(self.token.0.len() + 8);
        input.extend_from_slice(self.token.0.as_bytes());
        input.extend_from_slice(&sequence.to_le_bytes());
        SessionId(hex(&blake3::hash(&input).as_bytes()[..16]))
    }
}

async fn accept(State(state): State<Arc<Sessions>>, headers: HeaderMap, body: Bytes) -> Response {
    let Ok(permit) = state.request_capacity.clone().try_acquire_owned() else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            -32000,
            "MCP request capacity reached",
        );
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        state.dispatch(&headers, &body)
    })
    .await
    .unwrap_or_else(|_| {
        rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            -32603,
            "request task failed",
        )
    })
}

async fn remove(State(state): State<Arc<Sessions>>, headers: HeaderMap) -> Response {
    let Ok(permit) = state.request_capacity.clone().try_acquire_owned() else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            -32000,
            "MCP request capacity reached",
        );
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        state.remove(&headers)
    })
    .await
    .unwrap_or_else(|_| {
        rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            -32603,
            "request task failed",
        )
    })
}

pub(super) fn main_entry(paths: &backend_runtime::WorkspacePaths, bind: LoopbackBind) -> ExitCode {
    match run(paths, bind) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(paths: &backend_runtime::WorkspacePaths, bind: LoopbackBind) -> Result<(), String> {
    let endpoint = backend_runtime::ensure_locald(paths).map_err(|error| error.to_string())?;
    let project = canonical_project(paths.project());
    let token = BearerToken::load()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(serve(endpoint, project, token, bind))
}

async fn serve(
    endpoint: PathBuf,
    project: String,
    token: BearerToken,
    bind: LoopbackBind,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind.0)
        .await
        .map_err(|error| format!("could not bind {}: {error}", bind.0))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let state = Arc::new(Sessions {
        endpoint,
        project,
        token: token.clone(),
        next_id: AtomicU64::new(0),
        live: Mutex::new(HashMap::new()),
        request_capacity: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
    });
    let app = Router::new()
        .route(MCP_PATH, post(accept).delete(remove))
        .layer(DefaultBodyLimit::max(crate::MAX_FRAME))
        .with_state(state);
    eprintln!(
        "backend-mcp: ready {}",
        json!({
            "transport": "streamable-http",
            "url": format!("http://{address}{MCP_PATH}"),
            "authorization": format!("Bearer {}", token.0),
            "maxSessions": MAX_SESSIONS,
            "maxInFlight": MAX_IN_FLIGHT,
            "maxRequestBytes": crate::MAX_FRAME,
        })
    );
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

fn canonical_project(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn rpc_response(reply: Option<Value>, session: Option<&SessionId>) -> Response {
    let mut response = match reply {
        Some(reply) => (StatusCode::OK, Json(reply)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    };
    if let Some(value) = session.and_then(SessionId::header_value) {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": code, "message": message }
        })),
    )
        .into_response()
}

fn unauthorized() -> Response {
    let mut response = rpc_error(
        StatusCode::UNAUTHORIZED,
        -32001,
        "missing or invalid bearer token",
    );
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"backend-mcp\""),
    );
    response
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let width = left.len().max(right.len());
    for index in 0..width {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_proof_rejects_non_loopback_addresses() {
        let ipv4_loopback = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
        let ipv6_loopback = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::LOCALHOST,
            0,
            0,
            0,
        ));
        let unspecified = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        assert!(LoopbackBind::new(ipv4_loopback).is_ok());
        assert!(LoopbackBind::new(ipv6_loopback).is_ok());
        assert!(LoopbackBind::new(unspecified).is_err());
    }

    #[test]
    fn token_comparison_includes_length() {
        assert!(constant_time_eq(b"abcdefghijklmnop", b"abcdefghijklmnop"));
        assert!(!constant_time_eq(b"abcdefghijklmnop", b"abcdefghijklmno"));
        assert!(!constant_time_eq(b"abcdefghijklmnop", b"xbcdefghijklmnop"));
    }
}
