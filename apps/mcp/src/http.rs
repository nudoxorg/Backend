//! Authenticated MCP over a bounded Axum HTTP endpoint.
//!
//! The transport owns connection/session concerns only. Every request still
//! enters the same JSON-RPC server and typed product session as stdio MCP.

use crate::jsonrpc::requests::{self, DisconnectGuard, RequestLease, RequestRegistry};
use crate::jsonrpc::{
    ReconnectingProduct, ResponseTransport, Server, disconnected_reconnecting_product,
    encode_response, finalize_response,
};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use backend_client::RequestCancellationReason;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::IntoFuture;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const MCP_PATH: &str = "/mcp";
const TOKEN_ENV: &str = "BACKEND_MCP_TOKEN";
const TOKEN_FILE: &str = "mcp-http-token";
static TOKEN_STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_SESSIONS: usize = 64;
const MAX_IN_FLIGHT: usize = 64;
const AUTHENTICATION_RETRY_SECONDS: u64 = 1;
const MAX_BOOTSTRAP_CAUSE_BYTES: usize = 4096;
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
            Err(std::env::VarError::NotPresent) => Self::load_workspace(paths),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!(
                "{TOKEN_ENV} must contain 16-256 visible ASCII bytes"
            )),
        }
    }

    fn load_workspace(
        paths: &backend_runtime::WorkspacePaths,
    ) -> Result<(Self, TokenSource), String> {
        paths
            .initialize_data_directory()
            .map_err(|error| error.to_string())?;
        let path = paths.data().join(TOKEN_FILE);
        Self::provision(&path).map(|token| (token, TokenSource::WorkspaceFile(path)))
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
type LiveSessions = HashMap<SessionId, Arc<LiveSession>>;

struct LiveSession {
    server: Mutex<Server<ReconnectingProduct>>,
    requests: RequestRegistry,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.requests.close(RequestCancellationReason::Shutdown);
    }
}

struct AdmittedRequest {
    session_id: SessionId,
    session: Arc<LiveSession>,
    value: Value,
    lease: Option<RequestLease>,
}

impl AdmittedRequest {
    fn context(&self) -> Option<backend_client::RequestContext> {
        self.lease.as_ref().map(|lease| lease.context.clone())
    }

    fn run(self) -> Response {
        let context = self.context();
        let Ok(mut server) = self.session.server.lock() else {
            return rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "MCP session unavailable",
            );
        };
        let reply = server.handle_value(self.value, context);
        drop(server);
        drop(self.lease);
        rpc_response(reply, Some(&self.session_id))
    }
}

struct Sessions {
    paths: backend_runtime::WorkspacePaths,
    project: String,
    cursor_secret: crate::jsonrpc::CursorAuthority,
    token: BearerToken,
    next_id: AtomicU64,
    live: Mutex<LiveSessions>,
}

/// Binding a loopback socket does not admit an authentication authority. No
/// session exists until the one owned initializer returns a durable token.
struct HttpBootstrap {
    paths: backend_runtime::WorkspacePaths,
    authentication: OnceLock<Authentication>,
    request_capacity: Arc<tokio::sync::Semaphore>,
}

enum Authentication {
    Ready(Arc<Sessions>),
    Failed(String),
}

impl HttpBootstrap {
    fn new(paths: backend_runtime::WorkspacePaths) -> Self {
        Self {
            paths,
            authentication: OnceLock::new(),
            request_capacity: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    fn sessions(&self, body: Option<&[u8]>) -> Result<Arc<Sessions>, Response> {
        match self.authentication.get() {
            Some(Authentication::Ready(sessions)) => Ok(Arc::clone(sessions)),
            Some(Authentication::Failed(cause)) => Err(authentication_unavailable(
                body,
                "authentication_failed",
                "Repair the authentication setup described by the cause, then restart backend-mcp.",
                Some(cause),
            )),
            None => Err(authentication_unavailable(
                body,
                "authentication_pending",
                "Retry after durable HTTP authentication is ready; no MCP session has been admitted.",
                None,
            )),
        }
    }

    fn finish(&self, admitted: Result<(BearerToken, TokenSource), String>, address: SocketAddr) {
        let authentication = match admitted {
            Ok((token, source)) => {
                let sessions = Arc::new(Sessions {
                    paths: self.paths.clone(),
                    project: canonical_project(self.paths.project()),
                    cursor_secret: crate::jsonrpc::CursorAuthority::workspace(&self.paths),
                    token,
                    next_id: AtomicU64::new(0),
                    live: Mutex::new(HashMap::new()),
                });
                if self
                    .authentication
                    .set(Authentication::Ready(sessions))
                    .is_ok()
                {
                    eprintln!("backend-mcp: ready {}", ready_event(address, &source));
                }
                return;
            }
            Err(mut cause) => {
                let mut boundary = cause.len().min(MAX_BOOTSTRAP_CAUSE_BYTES);
                while !cause.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                cause.truncate(boundary);
                Authentication::Failed(cause)
            }
        };
        if self.authentication.set(authentication).is_ok()
            && let Some(Authentication::Failed(cause)) = self.authentication.get()
        {
            eprintln!(
                "backend-mcp: authentication failed {}",
                json!({
                    "transport": "streamable-http",
                    "url": format!("http://{address}{MCP_PATH}"),
                    "phase": "durable-authentication",
                    "cause": cause,
                    "action": "Repair authentication setup, then restart backend-mcp.",
                })
            );
        }
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        let sessions = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for session in sessions.values() {
            session.requests.close(RequestCancellationReason::Shutdown);
        }
    }
}

impl Sessions {
    #[cfg(test)]
    fn dispatch(&self, headers: &HeaderMap, body: &[u8]) -> Response {
        match self.admit(headers, body) {
            Ok(request) => request.run(),
            Err(response) => response,
        }
    }

    // This phase runs before the Server mutex and before blocking task admission.
    // Cancellation needs neither a query slot nor a replacement owner connection.
    fn admit(&self, headers: &HeaderMap, body: &[u8]) -> Result<AdmittedRequest, Response> {
        if !self.token.authorizes(headers) {
            return Err(unauthorized());
        }
        let value = match requests::decode(body) {
            Ok(value) => value,
            Err(reply) => return Err(rpc_response(Some(reply), None)),
        };
        let is_initialize = value.get("method").and_then(Value::as_str) == Some("initialize");
        let supplied = SessionId::from_headers(headers);
        if is_initialize && supplied.is_none() {
            return Err(self.initialize(body));
        }
        let Some(session_id) = supplied else {
            return Err(rpc_error(
                StatusCode::BAD_REQUEST,
                -32001,
                "Mcp-Session-Id is required",
            ));
        };
        let Ok(sessions) = self.live.lock() else {
            return Err(rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "session store unavailable",
            ));
        };
        let Some(session) = sessions.get(&session_id).cloned() else {
            return Err(rpc_error(
                StatusCode::NOT_FOUND,
                -32001,
                "MCP session not found",
            ));
        };
        drop(sessions);
        if session.requests.cancel_notification(&value) {
            return Err(rpc_response(None, Some(&session_id)));
        }
        let lease = session
            .requests
            .admit(&value)
            .map_err(|reply| rpc_response(Some(reply), Some(&session_id)))?;
        Ok(AdmittedRequest {
            session_id,
            session,
            value,
            lease,
        })
    }

    fn initialize(&self, body: &[u8]) -> Response {
        let mut server = Server::with_authority(
            disconnected_reconnecting_product(&self.paths),
            self.project.clone(),
            self.cursor_secret.clone(),
        );
        let Some(reply) = server.handle(body) else {
            return rpc_response(None, None);
        };
        let Ok(reply) = finalize_response(reply, ResponseTransport::HttpBody) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if !reply.is_result() {
            return encoded_json_response(StatusCode::OK, reply.into_bytes());
        }
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
        let session_id = self.next_session_id();
        sessions.insert(
            session_id.clone(),
            Arc::new(LiveSession {
                server: Mutex::new(server),
                requests: RequestRegistry::default(),
            }),
        );
        with_session_header(
            encoded_json_response(StatusCode::OK, reply.into_bytes()),
            Some(&session_id),
        )
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
                Some(session) => {
                    session.requests.close(RequestCancellationReason::Shutdown);
                    StatusCode::NO_CONTENT.into_response()
                }
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

async fn accept(
    State(state): State<Arc<HttpBootstrap>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let sessions = match state.sessions(Some(&body)) {
        Ok(sessions) => sessions,
        Err(response) => return response,
    };
    let request = match sessions.admit(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Ok(permit) = state.request_capacity.clone().try_acquire_owned() else {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            -32000,
            "MCP request capacity reached",
        );
    };
    let mut disconnect = DisconnectGuard::new(request.context());
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        request.run()
    })
    .await;
    disconnect.disarm();
    result.unwrap_or_else(|_| {
        rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            -32603,
            "request task failed",
        )
    })
}

async fn remove(State(state): State<Arc<HttpBootstrap>>, headers: HeaderMap) -> Response {
    // Session deletion is a cancellation control. It has no owner I/O and
    // remains reachable when all ordinary blocking query slots are occupied.
    match state.sessions(None) {
        Ok(sessions) => sessions.remove(&headers),
        Err(response) => response,
    }
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(serve(paths.clone(), bind))
}

async fn serve(paths: backend_runtime::WorkspacePaths, bind: LoopbackBind) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind.0)
        .await
        .map_err(|error| format!("could not bind {}: {error}", bind.0))?;
    let state = Arc::new(HttpBootstrap::new(paths));
    serve_bound(listener, state, BearerToken::load).await
}

async fn serve_bound<F>(
    listener: tokio::net::TcpListener,
    state: Arc<HttpBootstrap>,
    initialize: F,
) -> Result<(), String>
where
    F: FnOnce(&backend_runtime::WorkspacePaths) -> Result<(BearerToken, TokenSource), String>
        + Send
        + 'static,
{
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    let app = Router::new()
        .route(MCP_PATH, post(accept).delete(remove))
        .layer(DefaultBodyLimit::max(crate::MAX_MCP_REQUEST_FRAME))
        .with_state(Arc::clone(&state));
    eprintln!(
        "backend-mcp: bound {}",
        json!({
            "transport": "streamable-http",
            "url": format!("http://{address}{MCP_PATH}"),
            "phase": "authentication_pending",
            "action": "Wait for the ready event before creating an MCP session.",
        })
    );
    let paths = state.paths.clone();
    // This is one process-owned blocking task. The listener serves truthful
    // Pending responses while durable setup waits; no deadline pretends that
    // a token or owner has become ready.
    let initializer = tokio::task::spawn_blocking(move || initialize(&paths));
    let server = IntoFuture::into_future(axum::serve(listener, app));
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result.map_err(|error| error.to_string()),
        admitted = initializer => {
            state.finish(
                admitted.unwrap_or_else(|_| Err("HTTP authentication initializer failed".to_owned())),
                address,
            );
            server.await.map_err(|error| error.to_string())
        }
    }
}

fn authentication_unavailable(
    body: Option<&[u8]>,
    kind: &'static str,
    action: &'static str,
    cause: Option<&str>,
) -> Response {
    let id = body
        .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        .and_then(|request| request.get("id").cloned())
        .filter(|id| id.is_string() || id.is_number() || id.is_null())
        .unwrap_or(Value::Null);
    let mut reply = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32002,
            "message": "MCP authentication is not ready",
            "data": {"kind": kind, "phase": "durable-authentication", "action": action},
        },
    });
    if let Some(cause) = cause {
        reply["error"]["data"]["cause"] = json!(cause);
    } else {
        reply["error"]["data"]["retryAfterSeconds"] = json!(AUTHENTICATION_RETRY_SECONDS);
    }
    let mut response = json_response(StatusCode::SERVICE_UNAVAILABLE, reply);
    if cause.is_none() {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

fn canonical_project(path: &Path) -> String {
    backend_runtime::normalize_surface_path(path)
        .to_string_lossy()
        .into_owned()
}

fn rpc_response(reply: Option<Value>, session: Option<&SessionId>) -> Response {
    let response = match reply {
        Some(reply) => json_response(StatusCode::OK, reply),
        None => StatusCode::ACCEPTED.into_response(),
    };
    with_session_header(response, session)
}

fn with_session_header(mut response: Response, session: Option<&SessionId>) -> Response {
    if let Some(value) = session.and_then(SessionId::header_value) {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

fn encoded_json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn json_response(status: StatusCode, reply: Value) -> Response {
    match encode_response(reply, ResponseTransport::HttpBody) {
        Ok(body) => encoded_json_response(status, body),
        // Whole-reply admission always supplies a small fallback. If encoding
        // nevertheless fails, never emit a partial or unmeasured JSON reply.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn ready_event(address: SocketAddr, source: &TokenSource) -> Value {
    json!({
        "transport": "streamable-http",
        "url": format!("http://{address}{MCP_PATH}"),
        "authorization": source.hint(),
        "maxSessions": MAX_SESSIONS,
        "maxInFlight": MAX_IN_FLIGHT,
        "maxRequestBytes": crate::MAX_MCP_REQUEST_FRAME,
        "maxResponseBytes": ResponseTransport::HttpBody.limit_bytes(),
    })
}

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    json_response(
        status,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": code, "message": message }
        }),
    )
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

    fn bootstrap_paths(fixture: &PrivateTokenFixture) -> backend_runtime::WorkspacePaths {
        let project = fixture.0.join("project");
        std::fs::create_dir(&project).expect("HTTP fixture project");
        backend_runtime::WorkspacePaths::discover(
            Some(project),
            Some(fixture.0.join("state")),
            None,
        )
        .expect("HTTP fixture workspace selection")
    }

    async fn wire_request(address: SocketAddr, authorization: Option<String>) -> String {
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"bootstrap-test","version":"1"}}}"#;
        wire_request_body(address, authorization, body.to_owned()).await
    }

    async fn wire_request_body(
        address: SocketAddr,
        authorization: Option<String>,
        body: String,
    ) -> String {
        tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            let mut connection = std::net::TcpStream::connect_timeout(
                &address,
                std::time::Duration::from_secs(2),
            )
            .expect("bound HTTP listener accepts a connection");
            connection
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("bounded response wait");
            connection
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("bounded request write");
            write!(
                connection,
                "POST /mcp HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}\r\n{body}",
                body.len(),
                authorization.map_or_else(String::new, |token| format!("Authorization: Bearer {token}\r\n")),
            )
            .expect("write initialize request");
            let mut response = String::new();
            connection
                .take(64 * 1024)
                .read_to_string(&mut response)
                .expect("complete bounded HTTP reply");
            assert!(response.len() < 64 * 1024, "response exceeded test bound");
            response
        })
        .await
        .expect("wire observer completed")
    }

    async fn wait_for_authentication(state: &HttpBootstrap) {
        // Ready follows real file and directory durability barriers. This
        // completion bound is separate from the two-second Pending-response
        // assertions below and does not impose a production Ready deadline.
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while state.authentication.get().is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("authentication initializer completes");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bound_http_answers_pending_before_blocked_durable_authentication_and_admits_afterward()
    {
        let fixture = PrivateTokenFixture::new();
        let paths = bootstrap_paths(&fixture);
        let state = Arc::new(HttpBootstrap::new(paths.clone()));
        let listener = tokio::net::TcpListener::bind(LoopbackBind::default().0)
            .await
            .expect("loopback bind");
        let address = listener.local_addr().expect("bound address");
        let (entered, admission_entered) = tokio::sync::oneshot::channel();
        let (release, admission_release) = std::sync::mpsc::channel();
        let serving = tokio::spawn(serve_bound(listener, Arc::clone(&state), move |paths| {
            let _ = entered.send(());
            admission_release
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("release blocked authentication initializer");
            BearerToken::load_workspace(paths)
        }));
        tokio::time::timeout(std::time::Duration::from_secs(2), admission_entered)
            .await
            .expect("initializer began")
            .expect("initializer entered barrier");
        assert!(
            !paths.data().exists(),
            "metadata bootstrap must not create workspace state"
        );
        let occupied = state
            .request_capacity
            .clone()
            .acquire_many_owned(u32::try_from(MAX_IN_FLIGHT).expect("bounded capacity"))
            .await
            .expect("occupy request capacity");
        let saturated = wire_request(address, None).await;
        assert!(saturated.starts_with("HTTP/1.1 503"), "{saturated}");
        assert!(saturated.contains("MCP request capacity reached"));
        drop(occupied);
        let pending = wire_request(address, None).await;
        assert!(pending.starts_with("HTTP/1.1 503"), "{pending}");
        assert!(pending.to_ascii_lowercase().contains("retry-after: 1\r\n"));
        assert!(!pending.to_ascii_lowercase().contains("mcp-session-id:"));
        let (_, body) = pending.split_once("\r\n\r\n").expect("HTTP body");
        let pending = assert_http_body_accounting(body.as_bytes());
        assert_eq!(pending["id"], 7);
        assert_eq!(pending["error"]["data"]["kind"], "authentication_pending");
        assert!(state.authentication.get().is_none());
        release
            .send(())
            .expect("resume real durable token admission");
        wait_for_authentication(&state).await;
        let token = BearerToken::read_file(&paths.data().join(TOKEN_FILE))
            .expect("stable durable HTTP token exists before Ready");
        let unauthorized = wire_request(address, None).await;
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
        let initialized = wire_request(address, Some(token.0.to_string())).await;
        assert!(initialized.starts_with("HTTP/1.1 200"), "{initialized}");
        assert!(initialized.to_ascii_lowercase().contains("mcp-session-id:"));
        assert!(
            !paths.authority_secret().exists(),
            "initialize has no cursor"
        );
        let Authentication::Ready(sessions) = state.authentication.get().expect("Ready") else {
            panic!("durable authentication failed");
        };
        assert_eq!(sessions.live.lock().expect("live sessions").len(), 1);
        serving.abort();
        assert!(
            serving
                .await
                .expect_err("test listener retired")
                .is_cancelled()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_http_authentication_retains_first_cause_and_admits_no_sessions() {
        let fixture = PrivateTokenFixture::new();
        let paths = bootstrap_paths(&fixture);
        paths
            .initialize_data_directory()
            .expect("private state fixture");
        let token_path = paths.data().join(TOKEN_FILE);
        let secret = "invalid-private-token-not-for-response".repeat(10);
        backend_platform::durable::write_private_atomic(&token_path, secret.as_bytes())
            .expect("invalid existing token");
        let expected = BearerToken::load_workspace(&paths)
            .err()
            .expect("exact admission refusal");
        let state = Arc::new(HttpBootstrap::new(paths));
        let listener = tokio::net::TcpListener::bind(LoopbackBind::default().0)
            .await
            .expect("loopback bind");
        let address = listener.local_addr().expect("bound address");
        let serving = tokio::spawn(serve_bound(
            listener,
            Arc::clone(&state),
            BearerToken::load_workspace,
        ));
        wait_for_authentication(&state).await;
        for _ in 0..2 {
            let response = wire_request(address, Some(secret.clone())).await;
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            assert!(!response.contains(&secret));
            assert!(!response.to_ascii_lowercase().contains("mcp-session-id:"));
            let (_, body) = response.split_once("\r\n\r\n").expect("HTTP body");
            let failed = assert_http_body_accounting(body.as_bytes());
            assert_eq!(failed["id"], 7);
            assert_eq!(failed["error"]["data"]["kind"], "authentication_failed");
            assert_eq!(failed["error"]["data"]["cause"], expected);
            assert!(
                failed["error"]["data"]["action"]
                    .as_str()
                    .expect("action")
                    .contains("restart")
            );
        }
        assert_eq!(
            std::fs::read(token_path).expect("unchanged credential"),
            secret.as_bytes()
        );
        assert!(matches!(
            state.authentication.get(),
            Some(Authentication::Failed(_))
        ));
        serving.abort();
        assert!(
            serving
                .await
                .expect_err("test listener retired")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn unavailable_authentication_response_keeps_an_oversized_request_id_bounded() {
        let request = serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":"x".repeat(crate::MAX_MCP_RESPONSE_FRAME), "method":"initialize"}))
            .expect("bounded request fixture");
        let response = authentication_unavailable(
            Some(&request),
            "authentication_pending",
            "Retry after authentication is ready.",
            None,
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), crate::MAX_MCP_RESPONSE_FRAME)
            .await
            .expect("response remains within the product byte bound");
        let reply = assert_http_body_accounting(&body);
        assert!(
            reply["id"].is_null(),
            "unreturnable identity cannot overflow the response frame"
        );
        assert_eq!(reply["error"]["data"]["kind"], "authentication_pending");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_http_oversized_initialize_identity_admits_no_session() {
        let fixture = PrivateTokenFixture::new();
        let paths = bootstrap_paths(&fixture);
        let state = Arc::new(HttpBootstrap::new(paths));
        let listener = tokio::net::TcpListener::bind(LoopbackBind::default().0)
            .await
            .expect("HTTP listener");
        let address = listener.local_addr().expect("bound HTTP address");
        let token = "test-http-initialize-identity-token";
        let serving = tokio::spawn(serve_bound(listener, Arc::clone(&state), move |_| {
            Ok((BearerToken(token.into()), TokenSource::Environment))
        }));
        wait_for_authentication(&state).await;
        let Authentication::Ready(sessions) =
            state.authentication.get().expect("HTTP authentication")
        else {
            panic!("HTTP test authentication failed");
        };
        let request = json!({"jsonrpc":"2.0", "id":"x".repeat(backend_present::DEFAULT_RESPONSE_BUDGET_BYTES), "method":"initialize", "params": {
            "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"oversized-id-test","version":"1"}
        }}).to_string();
        assert!(request.len() <= crate::MAX_MCP_REQUEST_FRAME);
        for _ in 0..3 {
            let response =
                wire_request_body(address, Some(token.to_owned()), request.clone()).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            let (headers, body) = response
                .split_once("\r\n\r\n")
                .expect("HTTP response framing");
            assert!(!headers.to_ascii_lowercase().contains("mcp-session-id:"));
            let emitted = assert_http_body_accounting(body.as_bytes());
            assert_eq!(emitted["error"]["code"], -32000);
            assert!(emitted["id"].is_null());
            assert!(sessions.live.lock().expect("session store").is_empty());
        }
        let initialized = wire_request(address, Some(token.to_owned())).await;
        let (headers, body) = initialized
            .split_once("\r\n\r\n")
            .expect("successful HTTP initialize");
        assert!(headers.to_ascii_lowercase().contains("mcp-session-id:"));
        assert_eq!(assert_http_body_accounting(body.as_bytes())["id"], 7);
        assert_eq!(sessions.live.lock().expect("session store").len(), 1);
        let event = ready_event(address, &TokenSource::Environment);
        assert_eq!(
            event["maxResponseBytes"],
            ResponseTransport::HttpBody.limit_bytes()
        );
        assert_eq!(
            event["maxResponseBytes"],
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES
        );
        assert!(ResponseTransport::HttpBody.limit_bytes() < crate::MAX_MCP_RESPONSE_FRAME);
        serving.abort();
        assert!(
            serving
                .await
                .expect_err("test HTTP listener retired")
                .is_cancelled()
        );
    }

    fn assert_http_body_accounting(body: &[u8]) -> Value {
        assert!(body.len() <= backend_present::DEFAULT_RESPONSE_BUDGET_BYTES);
        assert_ne!(body.last(), Some(&b'\n'));
        let reply: Value = serde_json::from_slice(body).expect("complete HTTP JSON body");
        let budget = if reply.get("result").is_some() {
            &reply["result"]["_meta"]["backend/wireBudget"]
        } else {
            &reply["error"]["data"]["_meta"]["backend/wireBudget"]
        };
        assert_eq!(budget["bytes"], body.len());
        assert_eq!(budget["scope"], "complete_jsonrpc_body");
        assert_eq!(
            budget["limitBytes"],
            ResponseTransport::HttpBody.limit_bytes()
        );
        assert_eq!(
            budget["estimatedTokens"],
            backend_present::estimate_tokens(body.len())
        );
        reply
    }

    async fn reply_wire_request(address: SocketAddr, reply: Value) -> String {
        tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            let body = serde_json::to_vec(&reply).expect("test response input");
            let mut connection = std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(2)).expect("HTTP encoder listener");
            connection.set_read_timeout(Some(std::time::Duration::from_secs(2))).expect("bounded HTTP read");
            connection.set_write_timeout(Some(std::time::Duration::from_secs(2))).expect("bounded HTTP write");
            write!(connection, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).expect("HTTP headers");
            connection.write_all(&body).expect("response fixture");
            let mut response = String::new();
            connection.take(128 * 1024).read_to_string(&mut response).expect("complete HTTP response");
            assert!(response.len() < 128 * 1024);
            response
        }).await.expect("HTTP wire observer")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_http_body_accounts_success_refusal_error_and_exact_ceiling() {
        let listener = tokio::net::TcpListener::bind(LoopbackBind::default().0)
            .await
            .expect("HTTP listener");
        let address = listener.local_addr().expect("HTTP address");
        let app = Router::new().route(
            MCP_PATH,
            post(|body: Bytes| async move {
                let reply = serde_json::from_slice(&body).expect("test semantic reply");
                rpc_response(Some(reply), None)
            }),
        );
        let serving = tokio::spawn(async move { axum::serve(listener, app).await });
        let structured = json!({"answer":"records", "unicode":"λ אב🙂", "escaped":"\\\"\n", "nextCursor":"owner-cursor"});
        let base = json!({"jsonrpc":"2.0", "id":"request-אב🙂", "result": {
            "content":[{"type":"text", "text":"λ אב🙂\n\\\""}],
            "structuredContent":structured, "isError":false
        }});
        let mut refusal = base.clone();
        refusal["result"]["isError"] = json!(true);
        let mut boundary = json!({"jsonrpc":"2.0", "id":42, "result":{"payload":""}});
        let ceiling = backend_present::DEFAULT_RESPONSE_BUDGET_BYTES;
        let mut padding = ceiling - 512;
        for _ in 0..8 {
            boundary["result"]["payload"] = json!("p".repeat(padding));
            let size = encode_response(boundary.clone(), ResponseTransport::HttpBody)
                .expect("boundary body")
                .len();
            if size == ceiling {
                break;
            }
            // Start below the ceiling because an over-budget candidate would
            // become a fallback and cannot serve as a boundary measurement.
            assert!(size < ceiling);
            padding += ceiling - size;
        }
        assert_eq!(
            encode_response(boundary.clone(), ResponseTransport::HttpBody)
                .expect("exact boundary")
                .len(),
            ceiling
        );
        for reply in [
            base,
            refusal,
            json!({"jsonrpc":"2.0", "id":42, "error":{"code":-32601,"message":"unknown method"}}),
            boundary,
        ] {
            let response = reply_wire_request(address, reply.clone()).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            let (headers, body) = response.split_once("\r\n\r\n").expect("HTTP headers/body");
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("content-type: application/json")
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains(&format!("content-length: {}", body.len()))
            );
            let emitted = assert_http_body_accounting(body.as_bytes());
            assert_eq!(emitted["id"], reply["id"]);
            if reply["result"].get("structuredContent").is_some() {
                assert_eq!(emitted["result"]["structuredContent"], structured);
                assert_eq!(emitted["result"]["isError"], reply["result"]["isError"]);
            }
            let line =
                encode_response(reply, ResponseTransport::StdioLine).expect("same stdio reply");
            assert_eq!(line.last(), Some(&b'\n'));
            let stdio: Value = serde_json::from_slice(&line).expect("complete stdio line");
            let budget = if stdio.get("result").is_some() {
                &stdio["result"]["_meta"]["backend/wireBudget"]
            } else {
                &stdio["error"]["data"]["_meta"]["backend/wireBudget"]
            };
            assert_eq!(budget["bytes"], line.len());
            assert_eq!(budget["scope"], "complete_jsonrpc_line");
            if body.len() == ceiling {
                assert_eq!(stdio["error"]["code"], -32000);
            }
        }
        serving.abort();
        assert!(
            serving
                .await
                .expect_err("test HTTP listener retired")
                .is_cancelled()
        );
    }
    fn cancellation_fixture() -> (
        PrivateTokenFixture,
        Arc<HttpBootstrap>,
        Arc<Sessions>,
        SessionId,
        HeaderMap,
    ) {
        let fixture = PrivateTokenFixture::new();
        let paths = bootstrap_paths(&fixture);
        let token = BearerToken("cancellation-test-http-token".into());
        let sessions = Arc::new(Sessions {
            paths: paths.clone(),
            project: canonical_project(paths.project()),
            cursor_secret: [7; 32].into(),
            token,
            next_id: AtomicU64::new(0),
            live: Mutex::new(HashMap::new()),
        });
        let response = sessions.initialize(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cancel-test","version":"1"}}}"#);
        let session_id =
            SessionId::from_headers(response.headers()).expect("admitted HTTP session");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer cancellation-test-http-token"),
        );
        headers.insert(
            SESSION_HEADER,
            session_id.header_value().expect("session header"),
        );
        let state = Arc::new(HttpBootstrap::new(paths));
        assert!(
            state
                .authentication
                .set(Authentication::Ready(sessions.clone()))
                .is_ok()
        );
        (fixture, state, sessions, session_id, headers)
    }

    fn hold_server(
        session: Arc<LiveSession>,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (held, holding) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _server = session.server.lock().expect("hold actual Server mutex");
            held.send(()).expect("report held Server mutex");
            let _ = released.recv_timeout(std::time::Duration::from_secs(5));
        });
        holding
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("actual Server mutex is held");
        (release, worker)
    }

    #[tokio::test]
    async fn cancellation_bypasses_locked_server_and_full_http_query_capacity() {
        let (_fixture, state, sessions, session_id, headers) = cancellation_fixture();
        let request = sessions
            .admit(
                &headers,
                br#"{"jsonrpc":"2.0","id":7,"method":"tools/call"}"#,
            )
            .ok()
            .expect("admitted before Server lock");
        let context = request.context().expect("request-owned cancellation");
        let session = sessions
            .live
            .lock()
            .expect("sessions")
            .get(&session_id)
            .expect("session")
            .clone();
        let (release, held_server) = hold_server(session);
        let _all_query_slots = state
            .request_capacity
            .clone()
            .acquire_many_owned(MAX_IN_FLIGHT as u32)
            .await
            .expect("full HTTP query capacity");
        let reply = accept(
            State(state),
            headers,
            Bytes::from_static(
                br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#,
            ),
        )
        .await;
        release.send(()).expect("release Server mutex");
        held_server.join().expect("Server holder retired");
        assert_eq!(reply.status(), StatusCode::ACCEPTED);
        assert_eq!(
            context.cancellation_reason(),
            Some(RequestCancellationReason::Explicit)
        );
        drop(request);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_http_response_future_cancels_its_admitted_blocking_request() {
        let (_fixture, _state, sessions, session_id, headers) = cancellation_fixture();
        let request = sessions
            .admit(
                &headers,
                br#"{"jsonrpc":"2.0","id":7,"method":"tools/call"}"#,
            )
            .ok()
            .expect("request admission");
        let context = request.context().expect("request-owned cancellation");
        let session = sessions
            .live
            .lock()
            .expect("sessions")
            .get(&session_id)
            .expect("session")
            .clone();
        let (release, held_server) = hold_server(session);
        let retained = context.clone();
        let (started, start) = tokio::sync::oneshot::channel();
        let response = tokio::spawn(async move {
            let _disconnect = DisconnectGuard::new(Some(retained));
            let task = tokio::task::spawn_blocking(move || request.run());
            let _ = started.send(());
            let _ = task.await;
        });
        start.await.expect("handler owns blocking task");
        response.abort();
        let dropped = response.await;
        release.send(()).expect("release Server mutex");
        held_server.join().expect("Server holder retired");
        assert!(dropped.expect_err("dropped handler").is_cancelled());
        assert_eq!(
            context.cancellation_reason(),
            Some(RequestCancellationReason::ClientDisconnected)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_http_client_socket_drop_cancels_its_pending_blocking_request() {
        let (_fixture, state, sessions, session_id, _headers) = cancellation_fixture();
        let session = sessions
            .live
            .lock()
            .expect("sessions")
            .get(&session_id)
            .expect("session")
            .clone();
        let registry = session.requests.clone();
        let (release, held_server) = hold_server(session);
        let listener = tokio::net::TcpListener::bind(LoopbackBind::default().0)
            .await
            .expect("HTTP cancellation listener");
        let address = listener.local_addr().expect("HTTP cancellation address");
        let app = Router::new()
            .route(MCP_PATH, post(accept))
            .with_state(state);
        let serving = tokio::spawn(IntoFuture::into_future(axum::serve(listener, app)));
        let connection = tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            let mut connection = std::net::TcpStream::connect_timeout(
                &address,
                std::time::Duration::from_secs(2),
            )
            .expect("real HTTP client");
            connection
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("bounded client write");
            let body = br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
            write!(connection, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer cancellation-test-http-token\r\nMcp-Session-Id: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", session_id.0, body.len()).expect("real HTTP headers");
            connection.write_all(body).expect("actual request body");
            connection
        })
        .await
        .expect("wire client ready");
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(context) = registry.in_flight_context(&json!(7)) {
                    break context;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await;
        drop(connection); // actual read/write transport disconnect, not a synthetic task abort
        let stopped = match admitted {
            Ok(context) => {
                tokio::time::timeout(std::time::Duration::from_secs(2), context.cancelled())
                    .await
                    .ok()
            }
            Err(_) => None,
        };
        release.send(()).expect("release Server mutex");
        held_server.join().expect("Server holder retired");
        serving.abort();
        let _ = serving.await;
        assert_eq!(stopped, Some(RequestCancellationReason::ClientDisconnected));
    }

    #[test]
    fn deleting_http_session_cancels_pending_requests_without_waiting_for_server() {
        let (_fixture, _state, sessions, session_id, headers) = cancellation_fixture();
        let request = sessions
            .admit(&headers, br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
            .ok()
            .expect("pending request");
        let context = request.context().expect("pending lifetime");
        let session = sessions
            .live
            .lock()
            .expect("sessions")
            .get(&session_id)
            .expect("session")
            .clone();
        let _held = session.server.lock().expect("active Server lock");
        assert_eq!(sessions.remove(&headers).status(), StatusCode::NO_CONTENT);
        assert_eq!(
            context.cancellation_reason(),
            Some(RequestCancellationReason::Shutdown)
        );
        assert!(
            sessions
                .admit(&headers, br#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#)
                .is_err()
        );
    }
}
