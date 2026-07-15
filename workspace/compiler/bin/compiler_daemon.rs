//! Compiler daemon — an HTTP service that accepts [`compiler::protocol::CompileRequest`]
//! and returns [`compiler::protocol::CompileResponse`].
//!
//! Bind address: `NUDOX_COMPILER_ADDR` env var, defaulting to `0.0.0.0:8080`.
//!
//! Routes:
//!   GET  /health  → 200 OK
//!   POST /compile → **postcard** body: CompileRequest; postcard response:
//!                   CompileResponse (`Content-Type: application/x-postcard`)
//!
//! The protocol types are postcard-serializable by design (see
//! `compiler::protocol` / `registry::protocol`). JSON was a temporary mistake —
//! shipping `Vec<u8>` source files as JSON number arrays ballooned the body past
//! any reasonable limit and disagreed with the wire contract.

use std::sync::Arc;

use anyhow::Context as _;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use compiler::daemon::forge::{ForgeConfig, ForgeRuntime};
use compiler::protocol::{CompileRequest, CompileResponse, WireFile, WireReference};
use sandbox::Policy;
use tracing::info;

/// Content-Type for the postcard compile envelope (must match the server client).
const POSTCARD_CONTENT_TYPE: &str = "application/x-postcard";

// ─────────────────────────────────────────────────────────────────────────────
// Application state
// ─────────────────────────────────────────────────────────────────────────────

/// Shared, cloneable application state.
#[derive(Clone)]
struct AppState {
    /// The owned forge runtime.  Wrapped in `Arc` so it is cheap to clone into
    /// every request handler.
    forge: Arc<ForgeRuntime>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let addr = std::env::var("NUDOX_COMPILER_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned());

    // Assemble the forge runtime.  We use Development policy so the daemon can
    // start without bwrap on hosts that do not have Linux namespaces (e.g. the
    // macOS build image).  A production container would set
    // `NUDOX_ISOLATION_POLICY=production` which the sandbox crate picks up via
    // `IsolationPolicy::require_worker()`.
    let policy = Policy::Development;
    let cfg = ForgeConfig {
        cas_root: std::env::var_os("NUDOX_CAS_ROOT").map(std::path::PathBuf::from),
        ..ForgeConfig::default()
    };
    let handle = tokio::runtime::Handle::current();
    let forge = ForgeRuntime::assemble(policy, cfg, handle)
        .context("failed to assemble ForgeRuntime")?;
    let state = AppState { forge: Arc::new(forge) };

    // Postcard is compact, but large packages still ship every source file.
    // Match the server's write-plane ceiling so the forge worker is never the
    // body-size bottleneck.
    const COMPILE_BODY_CEILING: usize = 256 * 1024 * 1024;

    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/compile", post(compile))
        .layer(axum::extract::DefaultBodyLimit::max(COMPILE_BODY_CEILING))
        .with_state(state);

    info!(addr = %addr, "compiler daemon listening");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    axum::serve(listener, app).await.context("server error")?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
async fn health() -> impl IntoResponse {
    StatusCode::OK
}

/// `POST /compile` — compile one package (postcard request/response).
async fn compile(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let request: CompileRequest = match postcard::from_bytes(&body) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(%error, "postcard decode of CompileRequest failed");
            return postcard_response(
                StatusCode::BAD_REQUEST,
                &CompileResponse::Err {
                    kind: "bad_request".to_owned(),
                    message: format!("invalid postcard CompileRequest: {error}"),
                },
            );
        }
    };

    let response = tokio::task::spawn_blocking(move || {
        compile_package(state.forge.as_ref(), request)
    })
    .await;

    match response {
        Ok(r) => postcard_response(StatusCode::OK, &r),
        Err(e) => {
            tracing::error!(error = %e, "compile task panicked");
            postcard_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &CompileResponse::Err {
                    kind: "internal".to_owned(),
                    message: "compile task panicked".to_owned(),
                },
            )
        }
    }
}

/// Encode a [`CompileResponse`] as postcard with the standard content type.
fn postcard_response(status: StatusCode, response: &CompileResponse) -> axum::response::Response {
    match postcard::to_allocvec(response) {
        Ok(bytes) => {
            let mut res = (status, bytes).into_response();
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(POSTCARD_CONTENT_TYPE),
            );
            res
        }
        Err(error) => {
            tracing::error!(%error, "postcard encode of CompileResponse failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "postcard encode failed").into_response()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Compile logic (blocking — run on spawn_blocking pool)
// ─────────────────────────────────────────────────────────────────────────────

/// Materialize source files into a tempdir, peel the single-top-level-dir
/// wrapper if present, run the full generation pipeline, and map the result
/// into a [`CompileResponse`].
fn compile_package<C: compiler::compile::producer::ForgeContext>(
    forge: &C,
    request: CompileRequest,
) -> CompileResponse {
    let result = try_compile_package(forge, request);
    match result {
        Ok(r) => r,
        Err(e) => CompileResponse::Err {
            kind: "compile_error".to_owned(),
            message: format!("{e:#}"),
        },
    }
}

fn try_compile_package<C: compiler::compile::producer::ForgeContext>(
    forge: &C,
    request: CompileRequest,
) -> anyhow::Result<CompileResponse> {
    // ── 1. Materialize files into a tempdir ───────────────────────────────
    let temp = tempfile::tempdir().context("create tempdir")?;
    for file in &request.files {
        let dest = temp.path().join(&file.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).context("create parent dirs")?;
        }
        std::fs::write(&dest, &file.bytes).context("write source file")?;
    }

    // ── 2. Peel single-top-level wrapper (crates.io / npm tarball layout) ──
    let root = peel_single_top_level(temp.path()).context("peel top-level dir")?;

    // ── 3. Build PackageInput and run generation ──────────────────────────
    let input = compiler::generate::PackageInput {
        coordinates: request.coordinates,
        toolchain: request.toolchain,
        root,
    };

    let generated = compiler::generate::generate_with(forge, &input)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // ── 4. Map GeneratedPackage → CompileResponse::Ok ─────────────────────

    // surface: postcard-encode the IR Index. JSON cannot represent
    // `HashMap<NudoxPath, Entry>` map keys ("key must be a string"); postcard
    // is the project wire format and matches how blob sections are stored.
    let surface =
        postcard::to_allocvec(&generated.surface).context("serialize surface (postcard)")?;

    // references: per-file WireFile from the CstSet
    let references = {
        let mut wire_files = Vec::with_capacity(generated.cst.files.len());
        for cst_file in &generated.cst.files {
            let path = cst_file
                .path
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|| cst_file.path.to_string_lossy().into_owned());

            let mut wire_refs = Vec::with_capacity(cst_file.references.len());
            for reference in &cst_file.references {
                let wire = WireReference::from_reference(reference)
                    .map_err(|e| anyhow::anyhow!("reference encode: {e}"))?;
                wire_refs.push(wire);
            }
            wire_files.push(WireFile { path, references: wire_refs });
        }
        wire_files
    };

    // identifiers: public symbol names for search facets
    let identifiers = identifiers_from_index(&generated.surface);

    Ok(CompileResponse::Ok { surface, references, identifiers })
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (ported from server/coordination/indexing.rs)
// ─────────────────────────────────────────────────────────────────────────────

/// If `root` contains exactly one child directory and no files, return that
/// child (the package root inside a registry tarball).  Otherwise return `root`.
///
/// Ported verbatim from `server::coordination::indexing::peel_single_top_level`.
fn peel_single_top_level(root: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let mut only_dir: Option<std::path::PathBuf> = None;
    let mut file_count = 0usize;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            if only_dir.is_some() {
                // Multiple top-level dirs → keep the extract root.
                return Ok(root.to_path_buf());
            }
            only_dir = Some(entry.path());
        } else if file_type.is_file() {
            file_count += 1;
        }
    }
    Ok(match (only_dir, file_count) {
        (Some(dir), 0) => dir,
        _ => root.to_path_buf(),
    })
}

/// Harvest public symbol names from the surface IR for search facets.
///
/// Ported verbatim from `server::coordination::indexing::identifiers_from_index`.
fn identifiers_from_index(index: &ir::entry::Index) -> Vec<String> {
    use ir::kind::{Entry, Visibility};
    let mut names: Vec<String> = index
        .entries_by_path
        .values()
        .filter_map(|entry| {
            let (name, visibility) = match entry {
                Entry::Module(s) => (&s.name, &s.visibility),
                Entry::RecordType(s) => (&s.name, &s.visibility),
                Entry::Info(s) => (&s.name, &s.visibility),
                Entry::UnionType(s) => (&s.name, &s.visibility),
                Entry::TraitDef(s) => (&s.name, &s.visibility),
                Entry::TraitImpl(s) => (&s.name, &s.visibility),
                Entry::SumType(s) => (&s.name, &s.visibility),
                Entry::Function(s) => (&s.name, &s.visibility),
                Entry::TypeAlias(s) => (&s.name, &s.visibility),
                Entry::Constant(s) => (&s.name, &s.visibility),
                Entry::Variable(s) => (&s.name, &s.visibility),
                Entry::Macro(s) => (&s.name, &s.visibility),
                Entry::PrimitiveType(s) => (&s.name, &s.visibility),
                Entry::Field(s) => (&s.name, &s.visibility),
                Entry::Event(s) => (&s.name, &s.visibility),
            };
            matches!(visibility, Visibility::Public).then(|| name.clone())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}
