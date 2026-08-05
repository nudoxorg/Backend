//! Client glue: the typed wire vocabulary a client (GUI / CLI) uses to talk to
//! the `index` and `registry` layers, plus the capability vocabulary shared by
//! the request surfaces.
//!
//! Post-relayering (§8) there is no monolithic server: `index` (data/storage/
//! coordination) and `registry` (graph + vector) are separate layers, and the
//! client composes them. This module holds the pieces of that composition that
//! are *pure vocabulary* — serializable request/response shapes and the
//! authorization tokens — with no transport (axum/reqwest live in the composition
//! binary, not here, so `heart` stays dependency-light).
//!
//! What lived on the former `server` crate and landed here:
//! - the wire DTOs the client serializes/deserializes ([`dto`]);
//! - the capability + tenant vocabulary ([`authz`]).
//!
//! The *serving* side of those DTOs (axum extractors, the reqwest client, the
//! router, the assembled `Server`) is composition and stays in the staged
//! server bits, not in `heart`.

pub mod authz;
pub mod dto;
pub mod query;

pub use authz::{AdminCap, Principal, ReadCap, TenantId, WriteCap};
pub use dto::{
    AddPackageDto, COMPILED_LOOKUP_MAX_KEYS, CompiledLookupEntry, CompiledLookupRequest,
    CompiledLookupResponse, HealthDto, JobKeyHex, JobKeyHexError, RERANK_MAX_DOCUMENTS,
    RerankDocument, RerankRequestDto, RerankResponseDto, RerankScore,
};
pub use query::{
    AbstractQuery, ExecutionQuery, Filter, LiteralQuery, PackageSelector, QueryError, Search,
    SymbolCursor, SymbolCursorKey, VersionConstraint,
};
