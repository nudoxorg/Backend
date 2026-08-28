//! Write/admin handlers: add a package (idempotent), fetch its state, trigger a
//! re-sync. These enqueue work; they never run the pipeline inline. All three
//! answer with the domain [`Initialized`] (id + lifecycle state), serialized
//! directly — there is no parallel response DTO.

#[allow(unused_imports)]
use crate::server::registry;
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use heart::{Freshness, PackageId, ResolutionState};

use crate::server::Server;
use crate::server::authz::Principal;
use crate::server::coordination::indexing::Indexer;
use crate::server::coordination::initialization::{
    InitializationDecision, Initialized, initialization_decision,
};
use crate::server::error::{ServerError, ServerResult};
use crate::server::http::dto::{AddPackageDto, AddPackageExt};
use registry::vector::EmbeddingModel;

/// `POST /packages` — resolve coordinates and ensure the package is indexed.
/// Idempotent: a duplicate returns the existing id + state (never re-enqueues).
/// (Also serves the former `/packages/ensure`; the two were the same operation.)
#[tracing::instrument(skip_all, fields(ecosystem = %req.ecosystem, name = %req.name, version = %req.version))]
pub async fn add_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Json(req): Json<AddPackageDto>,
) -> ServerResult<Json<Initialized>> {
    let cap = server.authorize_write(&principal, "packages.ensure_initialized")?;
    // Parse + validate the wire request into typed coordinates (this is where the
    // version is consumed, so the derived id is correct).
    let coordinates = req.into_coordinates()?;
    let initialized = server.ensure_initialized(&cap, &coordinates).await?;
    tracing::info!(
        package = %initialized.package,
        enqueued = initialized.enqueued,
        "package ensured"
    );
    Ok(Json(initialized))
}

/// `GET /packages/:id` — the package's current lifecycle state.
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn get_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
    let package = PackageId::from_uuid(id);
    let state = server
        .parse_status(package)
        .await?
        .ok_or(ServerError::NotFound)?;
    Ok(Json(Initialized {
        package,
        state,
        enqueued: false,
    }))
}

/// `GET /packages/:id/files/*path` — stream one source file from the stored
/// package snapshot, honoring a single-range `Range` header so a snippet read
/// fetches only the bytes it needs from the CAS backend rather than the whole
/// file. The manifest is the path allow-list; a whole-file read's CAS fetch
/// verifies content integrity before returning bytes (a ranged read cannot —
/// see [`Server::source_file_range`]).
#[tracing::instrument(skip_all, fields(package = %id, path = %path))]
pub async fn get_source_file<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    Path((id, path)): Path<(uuid::Uuid, String)>,
    headers: HeaderMap,
) -> ServerResult<Response> {
    let path = path.trim_start_matches('/');
    if path.is_empty()
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "..")
    {
        return Err(crate::server::error::BadRequestReason::InvalidSourcePath {
            path: path.to_owned(),
        }
        .into());
    }

    let package = PackageId::from_uuid(id);

    let range_header = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());

    // No `Range` header at all: the plain whole-file path, unchanged except
    // that the response now advertises range support for next time.
    let Some(range_header) = range_header else {
        let (hash, bytes) = server.source_file(package, path).await?;
        return Ok(whole_file_response(hash, bytes));
    };

    // A `Range` header is present: its bytes are only meaningful against the
    // file's size, so look that up first (manifest-only — no CAS read yet).
    // This also lets an out-of-bounds range answer 416 without ever touching
    // the CAS backend.
    let (_hash, size) = server.source_file_meta(package, path).await?;

    match parse_byte_range(range_header, size) {
        // A header this parser doesn't understand (multi-range, non-`bytes`
        // unit, garbled numbers) is treated the same as "no header": serve
        // the whole file with 200, per RFC 7233 §3.1's "MAY ignore" latitude.
        RangeOutcome::Full => {
            let (hash, bytes) = server.source_file(package, path).await?;
            Ok(whole_file_response(hash, bytes))
        }
        RangeOutcome::Unsatisfiable => {
            let mut response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .body(Body::empty())
                .expect("a static status + empty body always builds");
            let headers = response.headers_mut();
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            if let Ok(value) = HeaderValue::try_from(format!("bytes */{size}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            Ok(response)
        }
        RangeOutcome::Satisfiable {
            start,
            end_inclusive,
        } => {
            let (hash, _size, bytes) = server
                .source_file_range(package, path, start..end_inclusive + 1)
                .await?;
            let mut response = Body::from(bytes).into_response();
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            if let Ok(value) = HeaderValue::try_from(format!("bytes {start}-{end_inclusive}/{size}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            if let Ok(value) = HeaderValue::try_from(format!("\"{hash}\"")) {
                headers.insert(header::ETAG, value);
            }
            Ok(response)
        }
    }
}

/// The common 200 response shape for a whole-file read: body + content-type +
/// etag + `Accept-Ranges: bytes` (advertised on every response, ranged or
/// not, so a client learns range support even from a plain GET).
fn whole_file_response(hash: String, bytes: bytes::Bytes) -> Response {
    let mut response = Body::from(bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::try_from(format!("\"{hash}\"")) {
        headers.insert(header::ETAG, value);
    }
    response
}

/// The three ways a `Range` header can resolve against a known file `size` —
/// a pure enum so [`parse_byte_range`] is unit-testable without a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeOutcome {
    /// No usable range: serve the whole file (200). This is also the outcome
    /// for a header the parser declines to interpret (see
    /// [`parse_byte_range`]'s doc comment).
    Full,
    /// A well-formed, in-bounds byte range: `[start, end_inclusive]`
    /// (`end_inclusive` is always `< size`).
    Satisfiable { start: u64, end_inclusive: u64 },
    /// A well-formed byte range that names bytes outside `[0, size)` (or
    /// `size == 0`) — the request was specific and answerable with a
    /// definite "no", which is a 416, not a fallback to the whole file.
    Unsatisfiable,
}

/// Parse a `Range` header value against a known file `size`.
///
/// Understands the single-range forms `bytes=START-END`, `bytes=START-`
/// (open-ended, to EOF), and `bytes=-SUFFIX` (the last `SUFFIX` bytes).
///
/// Anything this function does not understand — a non-`bytes` unit, a
/// multi-range list (`bytes=0-10,20-30`), or numbers that fail to parse —
/// resolves to [`RangeOutcome::Full`] rather than [`RangeOutcome::Unsatisfiable`].
/// RFC 7233 §3.1 allows a server to ignore a `Range` header it does not
/// support and answer with the full representation, and folding "can't parse
/// this" onto the same code path as "no header at all" is simpler than a
/// second error taxonomy for header syntax the client never gets to see
/// (browsers do not retry on 416 the way they retry on a missing partial
/// response). A range that *does* parse but points outside `[0, size)`
/// resolves to [`RangeOutcome::Unsatisfiable]` — that one is a 416, because
/// the request was well-formed and the server can answer it definitively.
fn parse_byte_range(header: &str, size: u64) -> RangeOutcome {
    let Some(spec) = header.strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    // Multiple ranges would need a `multipart/byteranges` response this
    // handler doesn't build — fall back to serving the whole file.
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((start_str, end_str)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };

    if start_str.is_empty() {
        // `bytes=-SUFFIX`: the last SUFFIX bytes of the file.
        let Ok(suffix) = end_str.parse::<u64>() else {
            return RangeOutcome::Full;
        };
        // A zero-length suffix, or an empty file, has nothing to serve.
        if suffix == 0 || size == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        let start = size.saturating_sub(suffix);
        return RangeOutcome::Satisfiable {
            start,
            end_inclusive: size - 1,
        };
    }

    let Ok(start) = start_str.parse::<u64>() else {
        return RangeOutcome::Full;
    };
    if size == 0 || start >= size {
        return RangeOutcome::Unsatisfiable;
    }

    let end_inclusive = if end_str.is_empty() {
        // `bytes=START-`: open-ended, to EOF.
        size - 1
    } else {
        match end_str.parse::<u64>() {
            // A requested end past EOF is clamped, not rejected (RFC 7233
            // §2.1: "the last-byte-pos value is not a byte's actual offset").
            Ok(end) => end.min(size - 1),
            Err(_) => return RangeOutcome::Full,
        }
    };

    // `start` was already checked `< size` above, so `end_inclusive < start`
    // is only possible when the client sent a backwards range explicitly.
    if end_inclusive < start {
        return RangeOutcome::Unsatisfiable;
    }

    RangeOutcome::Satisfiable {
        start,
        end_inclusive,
    }
}

#[cfg(test)]
mod range_tests {
    use super::{RangeOutcome, parse_byte_range};

    #[test]
    fn no_bytes_prefix_is_full() {
        assert_eq!(parse_byte_range("items=0-10", 100), RangeOutcome::Full);
        assert_eq!(parse_byte_range("garbage", 100), RangeOutcome::Full);
    }

    #[test]
    fn multi_range_is_full() {
        assert_eq!(
            parse_byte_range("bytes=0-10,20-30", 100),
            RangeOutcome::Full
        );
    }

    #[test]
    fn malformed_numbers_are_full() {
        assert_eq!(parse_byte_range("bytes=abc-10", 100), RangeOutcome::Full);
        assert_eq!(parse_byte_range("bytes=0-xyz", 100), RangeOutcome::Full);
        assert_eq!(parse_byte_range("bytes=", 100), RangeOutcome::Full);
        assert_eq!(parse_byte_range("bytes=abc", 100), RangeOutcome::Full);
    }

    #[test]
    fn closed_range_is_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=0-9", 100),
            RangeOutcome::Satisfiable {
                start: 0,
                end_inclusive: 9
            }
        );
        assert_eq!(
            parse_byte_range("bytes=10-19", 100),
            RangeOutcome::Satisfiable {
                start: 10,
                end_inclusive: 19
            }
        );
    }

    #[test]
    fn closed_range_end_past_eof_is_clamped() {
        assert_eq!(
            parse_byte_range("bytes=90-999", 100),
            RangeOutcome::Satisfiable {
                start: 90,
                end_inclusive: 99
            }
        );
    }

    #[test]
    fn open_ended_range_runs_to_eof() {
        assert_eq!(
            parse_byte_range("bytes=95-", 100),
            RangeOutcome::Satisfiable {
                start: 95,
                end_inclusive: 99
            }
        );
        assert_eq!(
            parse_byte_range("bytes=0-", 100),
            RangeOutcome::Satisfiable {
                start: 0,
                end_inclusive: 99
            }
        );
    }

    #[test]
    fn suffix_range_takes_last_n_bytes() {
        assert_eq!(
            parse_byte_range("bytes=-10", 100),
            RangeOutcome::Satisfiable {
                start: 90,
                end_inclusive: 99
            }
        );
    }

    #[test]
    fn suffix_larger_than_file_clamps_to_whole_file() {
        assert_eq!(
            parse_byte_range("bytes=-1000", 100),
            RangeOutcome::Satisfiable {
                start: 0,
                end_inclusive: 99
            }
        );
    }

    #[test]
    fn zero_suffix_is_unsatisfiable() {
        assert_eq!(parse_byte_range("bytes=-0", 100), RangeOutcome::Unsatisfiable);
    }

    #[test]
    fn start_at_or_past_size_is_unsatisfiable() {
        assert_eq!(parse_byte_range("bytes=100-200", 100), RangeOutcome::Unsatisfiable);
        assert_eq!(parse_byte_range("bytes=100-", 100), RangeOutcome::Unsatisfiable);
        assert_eq!(parse_byte_range("bytes=500-600", 100), RangeOutcome::Unsatisfiable);
    }

    #[test]
    fn backwards_range_is_unsatisfiable() {
        assert_eq!(parse_byte_range("bytes=50-10", 100), RangeOutcome::Unsatisfiable);
    }

    #[test]
    fn empty_file_is_always_unsatisfiable() {
        assert_eq!(parse_byte_range("bytes=0-0", 0), RangeOutcome::Unsatisfiable);
        assert_eq!(parse_byte_range("bytes=0-", 0), RangeOutcome::Unsatisfiable);
        assert_eq!(parse_byte_range("bytes=-10", 0), RangeOutcome::Unsatisfiable);
    }
}

/// `POST /packages/:id/sync` — force a freshness re-check and re-enqueue if stale.
#[tracing::instrument(skip_all, fields(package = %id))]
pub async fn sync_package<M: EmbeddingModel>(
    State(server): State<Arc<Server<M>>>,
    principal: Principal,
    Path(id): Path<uuid::Uuid>,
) -> ServerResult<Json<Initialized>> {
    let _cap = server.authorize_write(&principal, "packages.sync")?;
    let package = PackageId::from_uuid(id);
    let state = server
        .parse_status(package)
        .await?
        .ok_or(ServerError::NotFound)?;

    // Freshness is only decidable for a stored package: recompute the canonical
    // content hash from the current blob manifest and compare it to the snapshot
    // recorded at the `Stored` transition.
    let freshness = match &state {
        ResolutionState::Stored { hash } => {
            let recomputed = Indexer::new(Arc::clone(&server))
                .content_hash(package)
                .await?;
            Some(Freshness::compare(*hash, recomputed))
        }
        _ => None,
    };

    let decision = initialization_decision(Some(&state), freshness);
    let enqueued = matches!(decision, InitializationDecision::Enqueue);
    if enqueued {
        // Idempotent: the queue enforces one live job per package.
        server
            .queue()
            .enqueue(package)
            .await
            .map_err(crate::server::registry::RegistryError::from)?;
    }
    tracing::info!(?freshness, ?decision, "sync decision");
    Ok(Json(Initialized {
        package,
        state,
        enqueued,
    }))
}
