//! Authenticated MCP over a bounded Axum HTTP endpoint.
//!
//! The transport owns connection/session concerns only. Every request still
//! enters the same JSON-RPC server and typed product session as stdio MCP.

use crate::jsonrpc::{ReconnectingProduct, Server, disconnected_reconnecting_product};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MCP_PATH: &str = "/mcp";
const TOKEN_ENV: &str = "BACKEND_MCP_TOKEN";
const TOKEN_FILE: &str = "mcp-http-token";
static TOKEN_STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
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

enum TokenSource {
    Environment,
    WorkspaceFile(PathBuf),
}

impl TokenSource {
    fn hint(&self) -> Value {
        match self {
            Self::Environment => json!({"scheme": "Bearer", "tokenEnvironment": TOKEN_ENV}),
            Self::WorkspaceFile(path) => json!({
                "scheme": "Bearer", "tokenEnvironment": TOKEN_ENV, "tokenFile": path.to_string_lossy(),
            }),
        }
    }
}

impl BearerToken {
    fn load(paths: &backend_runtime::WorkspacePaths) -> Result<(Self, TokenSource), String> {
        match std::env::var(TOKEN_ENV) {
            Ok(value) => Self::parse(value).map(|token| (token, TokenSource::Environment)),
            Err(std::env::VarError::NotPresent) => {
                paths
                    .initialize_data_directory()
                    .map_err(|error| error.to_string())?;
                let path = paths.data().join(TOKEN_FILE);
                Self::provision(&path).map(|token| (token, TokenSource::WorkspaceFile(path)))
            }
            Err(std::env::VarError::NotUnicode(_)) => Err(format!(
                "{TOKEN_ENV} must contain 16-256 visible ASCII bytes"
            )),
        }
    }

    fn read_file(path: &Path) -> io::Result<Self> {
        // The existing private-file boundary checks the opened handle's
        // owner, permissions, type and linkage without following a final link.
        // Once publication links the complete staged file, another initializer
        // can briefly see two links before its staging name is removed.
        let mut attempts = 0;
        let file = loop {
            match backend_platform::durable::open_private_read(path) {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied && attempts < 8 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        };
        let mut bytes = Vec::with_capacity(257);
        file.take(257).read_to_end(&mut bytes)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP token file must contain visible ASCII bytes",
            )
        })?;
        Self::parse(text).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP token file must contain 16-256 visible ASCII bytes",
            )
        })
    }

    fn provision(path: &Path) -> Result<Self, String> {
        match Self::read_file(path) {
            Ok(token) => return Ok(token),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot admit HTTP token file {}: {error}",
                    path.display()
                ));
            }
        }
        let token = Self::generate()?;
        let stage = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            TOKEN_STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        backend_platform::durable::write_private_atomic(&stage, token.0.as_bytes())
            .map_err(|error| format!("cannot stage HTTP token file {}: {error}", path.display()))?;
        // A hard link publishes one complete private file with exclusive
        // destination creation. An existing credential is never replaced.
        let published = std::fs::hard_link(&stage, path);
        std::fs::remove_file(&stage)
            .map_err(|error| format!("cannot retire HTTP token staging file: {error}"))?;
        match published {
            Ok(()) => backend_platform::durable::sync_parent(path).map_err(|error| {
                format!("cannot commit HTTP token file {}: {error}", path.display())
            })?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "cannot publish HTTP token file {}: {error}",
                    path.display()
                ));
            }
        }
        Self::read_file(path)
            .map_err(|error| format!("cannot admit HTTP token file {}: {error}", path.display()))
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
        #[cfg(unix)]
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut entropy))
            .map_err(|error| format!("could not generate {TOKEN_ENV}: {error}"))?;
        #[cfg(windows)]
        backend_platform::win32::random::fill(&mut entropy)
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

/// The live MCP sessions, each owning one connected daemon session.
type LiveSessions = HashMap<SessionId, Arc<Mutex<Server<ReconnectingProduct>>>>;

struct Sessions {
    paths: backend_runtime::WorkspacePaths,
    project: String,
    cursor_secret: [u8; 32],
    token: BearerToken,
    next_id: AtomicU64,
    live: Mutex<LiveSessions>,
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
        let mut server = Server::with_authority(
            disconnected_reconnecting_product(&self.paths),
            self.project.clone(),
            self.cursor_secret,
        );
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
    let project = canonical_project(paths.project());
    let cursor_secret = crate::jsonrpc::cursor_secret(paths)?;
    let (token, token_source) = BearerToken::load(paths)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(serve(
        paths.clone(),
        project,
        cursor_secret,
        token,
        token_source,
        bind,
    ))
}

async fn serve(
    paths: backend_runtime::WorkspacePaths,
    project: String,
    cursor_secret: [u8; 32],
    token: BearerToken,
    token_source: TokenSource,
    bind: LoopbackBind,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind.0)
        .await
        .map_err(|error| format!("could not bind {}: {error}", bind.0))?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let state = Arc::new(Sessions {
        paths,
        project,
        cursor_secret,
        token: token.clone(),
        next_id: AtomicU64::new(0),
        live: Mutex::new(HashMap::new()),
        request_capacity: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
    });
    let app = Router::new()
        .route(MCP_PATH, post(accept).delete(remove))
        .layer(DefaultBodyLimit::max(crate::MAX_MCP_REQUEST_FRAME))
        .with_state(state);
    eprintln!(
        "backend-mcp: ready {}",
        json!({
            "transport": "streamable-http",
            "url": format!("http://{address}{MCP_PATH}"),
            "authorization": token_source.hint(),
            "maxSessions": MAX_SESSIONS,
            "maxInFlight": MAX_IN_FLIGHT,
            "maxRequestBytes": crate::MAX_MCP_REQUEST_FRAME,
            "maxResponseBytes": crate::MAX_MCP_RESPONSE_FRAME,
        })
    );
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

fn canonical_project(path: &Path) -> String {
    backend_runtime::normalize_surface_path(path)
        .to_string_lossy()
        .into_owned()
}

fn rpc_response(reply: Option<Value>, session: Option<&SessionId>) -> Response {
    if reply.as_ref().is_some_and(|reply| {
        let mut count = ResponseByteCounter::default();
        serde_json::to_writer(&mut count, reply).is_err()
            || count.bytes > crate::MAX_MCP_RESPONSE_FRAME
    }) {
        return rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            -32000,
            "MCP response exceeds the bounded response frame",
        );
    }
    let mut response = match reply {
        Some(reply) => (StatusCode::OK, Json(reply)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    };
    if let Some(value) = session.and_then(SessionId::header_value) {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

#[derive(Default)]
struct ResponseByteCounter {
    bytes: usize,
}

impl std::io::Write for ResponseByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

    struct PrivateTokenFixture(PathBuf);

    impl PrivateTokenFixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "backend-mcp-http-token-{}-{}",
                std::process::id(),
                TOKEN_STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("create owned token fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                    .expect("private token fixture directory");
            }
            #[cfg(windows)]
            backend_platform::win32::security::restrict_to_current_user(&path)
                .expect("private token fixture ACL");
            Self(path)
        }
    }

    impl Drop for PrivateTokenFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn automatic_http_token_is_private_and_survives_restart_without_rotation() {
        let fixture = PrivateTokenFixture::new();
        let path = fixture.0.join(TOKEN_FILE);
        let first = BearerToken::provision(&path).expect("provision HTTP credential");
        let second = BearerToken::provision(&path).expect("reuse HTTP credential");
        assert!(constant_time_eq(first.0.as_bytes(), second.0.as_bytes()));
        assert_eq!(first.0.len(), 64);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", first.0)).expect("header"),
        );
        assert!(second.authorizes(&headers));
        let hint = TokenSource::WorkspaceFile(path.clone()).hint().to_string();
        assert!(!hint.contains(first.0.as_ref()));
        assert!(hint.contains(TOKEN_FILE));
        assert!(hint.contains(TOKEN_ENV));
        assert!(
            !TokenSource::Environment
                .hint()
                .to_string()
                .contains(first.0.as_ref())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(path)
                    .expect("private token file")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn concurrent_http_initializers_admit_one_complete_credential() {
        let fixture = PrivateTokenFixture::new();
        let path = fixture.0.join(TOKEN_FILE);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    BearerToken::provision(&path).expect("admit concurrently published HTTP token")
                })
            })
            .collect::<Vec<_>>();
        let tokens = workers
            .into_iter()
            .map(|worker| worker.join().expect("initializer"))
            .collect::<Vec<_>>();
        assert!(
            tokens
                .iter()
                .all(|token| constant_time_eq(token.0.as_bytes(), tokens[0].0.as_bytes()))
        );
        assert_eq!(
            std::fs::read_dir(&fixture.0)
                .expect("token directory")
                .count(),
            1
        );
    }

    #[test]
    fn unsafe_or_oversized_http_token_files_refuse_without_replacing_or_echoing_bytes() {
        let fixture = PrivateTokenFixture::new();
        let path = fixture.0.join(TOKEN_FILE);
        let secret = "private-token-not-for-readiness";
        let bytes = secret.repeat(10);
        backend_platform::durable::write_private_atomic(&path, bytes.as_bytes())
            .expect("oversized credential fixture");
        let error = BearerToken::provision(&path)
            .err()
            .expect("bounded token read must refuse");
        assert!(!error.contains(secret));
        assert!(std::fs::read(&path).expect("unmodified refused credential") == bytes.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            use std::os::unix::fs::symlink;
            backend_platform::durable::write_private_atomic(&path, secret.as_bytes())
                .expect("credential fixture");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("unsafe mode fixture");
            assert!(BearerToken::provision(&path).is_err());
            assert!(
                std::fs::read(&path).expect("mode refusal does not rotate credential")
                    == secret.as_bytes()
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("restore fixture mode");
            let link = fixture.0.join("linked-token");
            symlink(&path, &link).expect("symlink fixture");
            assert!(BearerToken::provision(&link).is_err());
            assert!(
                std::fs::symlink_metadata(&link)
                    .expect("symlink remains")
                    .file_type()
                    .is_symlink()
            );
        }
    }

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
