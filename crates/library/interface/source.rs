//! Defines source behavior for `backend-library`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the source invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded owned source admitted once at the application boundary.

use core::ops::Deref;

/// Maximum UTF-8 source size accepted by the portable local compiler request.
///
/// One MiB admits a complete single-file editor buffer without making the portable client retain
/// a project-sized working set. Larger units are split into independently admitted compilation
/// units; file and stream ingress avoid shell-sized argument copies but obey this same budget.
pub const PORTABLE_LOCAL_SOURCE_LIMIT: SourceByteLimit = SourceByteLimit { bytes: 1 << 20 };

/// One named source-byte budget used at an application admission boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceByteLimit {
    /// Maximum accepted UTF-8 bytes.
    pub bytes: usize,
}

/// Exact source-width rejection before a source becomes application-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceTextLimit {
    /// UTF-8 bytes in the rejected source.
    pub observed: usize,
    /// Product source budget that rejected the input.
    pub limit: SourceByteLimit,
}

/// An oversize source and its exact admission rejection.
///
/// The caller retains this linear value so a transport can choose whether to report, save, or
/// stream the exact source without a second copy.
#[derive(Debug, Eq, PartialEq)]
pub struct RejectedSourceText {
    /// Original UTF-8 source owner, returned unchanged.
    pub source: String,
    /// Exact admission fact.
    pub error: SourceTextLimit,
}

/// One application-owned source whose backing allocation has exact string width.
///
/// Adapters already own a `String` while decoding JSON, CLI text, a file, or standard input.
/// Converting that owner to `Box<str>` may reallocate once, but removes spare `String` capacity
/// for the full request lifetime. The compiler only receives the borrowed [`str`] view below.
#[derive(Debug, Eq, PartialEq)]
pub struct SourceText {
    value: Box<str>,
}

impl TryFrom<String> for SourceText {
    type Error = RejectedSourceText;

    fn try_from(source: String) -> Result<Self, Self::Error> {
        let observed = source.len();
        if observed > PORTABLE_LOCAL_SOURCE_LIMIT.bytes {
            return Err(RejectedSourceText {
                source,
                error: SourceTextLimit {
                    observed,
                    limit: PORTABLE_LOCAL_SOURCE_LIMIT,
                },
            });
        }
        Ok(Self {
            value: source.into_boxed_str(),
        })
    }
}

impl Deref for SourceText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl AsRef<str> for SourceText {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

impl AsRef<[u8]> for SourceText {
    fn as_ref(&self) -> &[u8] {
        self.value.as_bytes()
    }
}
