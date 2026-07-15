//! Wire contract between the Cargo-side `server` and the Buck2 compiler daemon.
//!
//! Formerly the standalone dual-built `protocol` crate; folded into the
//! `registry` service (the Cargo consumer, `server`, reaches it via
//! `registry::protocol`). The compiler daemon keeps its own Buck-side copy of
//! this source verbatim so both ends agree on the byte layout.
//!
//! ## HTTP transport
//!
//! `POST /compile` bodies are **postcard** (`Content-Type: application/x-postcard`),
//! not JSON. Source files are raw `Vec<u8>` on the wire — JSON number-arrays
//! would explode payload size. After a successful compile the server emits the
//! IR + sections into the content-addressed **blob** object store (see
//! `registry::blob::emit`); the daemon itself does not write blobs.
//!
//! ## Wire layout stability
//!
//! [`WireReference`] and [`WireFile`] are the postcard-serializable mirrors of
//! `ir::syntax::ResolvedReference`.  Their field shapes and the
//! `ReferenceKind` discriminant table are intentionally copied (not moved) from
//! [`crate::blob`] so both the server and the compiler daemon agree on the byte
//! layout.

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors that can arise while encoding or decoding protocol wire types.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A `WireReference::span_start > span_end`, which is physically impossible.
    #[error("reference span is inverted (start > end)")]
    InvertedReferenceSpan,

    /// A `WireReference::kind` byte did not map to any known `ReferenceKind`.
    #[error("unknown reference-kind discriminant on wire: {wire}")]
    UnknownReferenceKindDiscriminant { wire: u8 },

    /// A path stored in a `WireTarget` was not valid UTF-8.
    #[error("reference path is not valid UTF-8")]
    NonUtf8ReferencePath,
}

// ─────────────────────────────────────────────────────────────────────────────
// Top-level request / response
// ─────────────────────────────────────────────────────────────────────────────

/// A single source file shipped to the compiler daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBytes {
    /// In-package path of the file (relative to the package root).
    pub path: String,
    /// Raw source bytes.
    pub bytes: Vec<u8>,
}

/// Everything the compiler daemon needs to compile one package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileRequest {
    /// The package being compiled (origin + name + version).
    pub coordinates: heart::package::Coordinates,
    /// The toolchain to compile against.
    pub toolchain: heart::Toolchain,
    /// All source files, shipped in full (the daemon is remote and has no
    /// access to the caller's filesystem).
    pub files: Vec<FileBytes>,
}

/// The compiler daemon's reply to a [`CompileRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompileResponse {
    /// Compilation succeeded.
    Ok {
        /// Postcard-encoded bytes of `ir::entry::Index` (the IR surface).
        /// Opaque to this crate (no `ir` dep); the server stores them as a blob
        /// section via `BlobBuilder::set_ir` and later readers postcard-decode.
        surface: Vec<u8>,
        /// Per-file resolved reference spans, ready to be turned back into
        /// `ir::syntax::ResolvedReference` via [`WireReference::into_reference`].
        references: Vec<WireFile>,
        /// Public symbol names extracted for search facets.
        identifiers: Vec<String>,
    },
    /// Compilation failed.
    Err {
        /// A short machine-readable error category (e.g. `"parse_error"`).
        kind: String,
        /// A human-readable description of the failure.
        message: String,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire mirror of ir::syntax::ResolvedReference
//
// Copied verbatim from crate::blob so the compiler daemon (Buck2) and server
// (Cargo) share the identical postcard layout.  Do NOT change field names or
// types without updating both sides simultaneously.
// ─────────────────────────────────────────────────────────────────────────────

/// One file's worth of reference spans, on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireFile {
    /// In-package path of the file these references were extracted from.
    pub path: String,
    /// The individual reference spans within this file.
    pub references: Vec<WireReference>,
}

/// One reference span, on the wire.
///
/// This is the postcard-serializable mirror of `ir::syntax::ResolvedReference`.
/// Field layout must stay byte-identical to the copy in [`crate::blob`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireReference {
    /// Where this reference points.
    pub target: WireTarget,
    /// Byte offset of the span start (inclusive) within the source file.
    pub span_start: u64,
    /// Byte offset of the span end (exclusive) within the source file.
    pub span_end: u64,
    /// Stable wire discriminant for `ir::syntax::ReferenceKind` — see
    /// [`kind_to_wire`] / [`kind_from_wire`].
    pub kind: u8,
}

/// The wire form of `ir::entry::NudoxPath`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireTarget {
    /// A reference into an external dependency.
    External { path: String, dependency: String },
    /// A reference within the same package.
    Local(String),
}

impl WireReference {
    /// Encode a [`ir::syntax::ResolvedReference`] into wire form.
    pub fn from_reference(
        reference: &ir::syntax::ResolvedReference,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            target: WireTarget::from_target(&reference.target)?,
            span_start: reference.span.start as u64,
            span_end: reference.span.end as u64,
            kind: kind_to_wire(&reference.kind),
        })
    }

    /// Decode a wire reference back into [`ir::syntax::ResolvedReference`].
    pub fn into_reference(self) -> Result<ir::syntax::ResolvedReference, ProtocolError> {
        if self.span_start > self.span_end {
            return Err(ProtocolError::InvertedReferenceSpan);
        }
        Ok(ir::syntax::ResolvedReference {
            target: self.target.into_target(),
            span: (self.span_start as usize)..(self.span_end as usize),
            kind: kind_from_wire(self.kind)?,
        })
    }
}

impl WireTarget {
    fn from_target(target: &ir::entry::NudoxPath) -> Result<Self, ProtocolError> {
        let utf8 = |path: &std::path::Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or(ProtocolError::NonUtf8ReferencePath)
        };
        Ok(match target {
            ir::entry::NudoxPath::External { path, dependency } => Self::External {
                path: utf8(path)?,
                dependency: dependency.clone(),
            },
            ir::entry::NudoxPath::Local(path) => Self::Local(utf8(path)?),
        })
    }

    fn into_target(self) -> ir::entry::NudoxPath {
        match self {
            Self::External { path, dependency } => ir::entry::NudoxPath::External {
                path: path.into(),
                dependency,
            },
            Self::Local(path) => ir::entry::NudoxPath::Local(path.into()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ReferenceKind discriminant table
//
// Stable wire values: DO NOT reorder or renumber.  Any new variant gets the
// next available integer.  These must stay identical to the copy in
// crate::blob.
// ─────────────────────────────────────────────────────────────────────────────

/// Encode a [`ir::syntax::ReferenceKind`] as a stable wire byte.
fn kind_to_wire(kind: &ir::syntax::ReferenceKind) -> u8 {
    use ir::syntax::ReferenceKind::*;
    match kind {
        FunctionCall => 0,
        MethodCall => 1,
        TypeReference => 2,
        VariableUse => 3,
        MacroInvocation => 4,
        FieldAccess => 5,
        Import => 6,
    }
}

/// Decode a wire byte back into a [`ir::syntax::ReferenceKind`].
fn kind_from_wire(wire: u8) -> Result<ir::syntax::ReferenceKind, ProtocolError> {
    use ir::syntax::ReferenceKind::*;
    Ok(match wire {
        0 => FunctionCall,
        1 => MethodCall,
        2 => TypeReference,
        3 => VariableUse,
        4 => MacroInvocation,
        5 => FieldAccess,
        6 => Import,
        _ => return Err(ProtocolError::UnknownReferenceKindDiscriminant { wire }),
    })
}
