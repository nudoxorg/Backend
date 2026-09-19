//! The two-lane answer contract for local product answers.
//!
//! ## Contract: tree-sitter is the lightest possible layer
//!
//! This workspace answers symbol, definition, reference, hover, and document
//! questions from two lanes:
//!
//! 1. **Semantic lane (authority).** Language authorities (rust-analyzer, the
//!    TSZ checker, Pyrefly, go-types, javac, Roslyn, libclang) compile each
//!    package/profile target into published semantic images
//!    (`ProductSemanticPublicationRecord::Published`). Every GUI and MCP
//!    answer for a file that has a *published, complete* semantic image MUST
//!    come from this lane: its enum variants, fields, properties, constants,
//!    and trait-method signatures are visible here and invisible to the
//!    structural baseline.
//! 2. **Structural lane (baseline fallback).** The per-frontend tree-sitter
//!    extraction (`syntax_frontend`, stock `TAGS_QUERY` only) is the
//!    zero-toolchain fallback. It exists ONLY for discovery and local
//!    browsing before the first compile, and for files a semantic authority
//!    cannot answer (non-UTF-8 sources, parsing failures, extensions with no
//!    semantic profile). It sees functions, methods, types, and modules —
//!    and nothing nested inside a type.
//!
//! The fallback direction is semantic → structural, never the reverse, and
//! never silently structural-only when semantics exist:
//!
//! * No complete publication for the file's profile → the structural lane
//!   answers, and the cause is retained
//!   ([`StructuralCause`]).
//! * A complete publication exists → the semantic lane answers. Structural
//!   rows for that file are suppressed entirely rather than merged, so a
//!   client never sees a half-structural half-semantic outline.
//! * A complete publication exists but disagrees with the current scan (a
//!   `content_version` mismatch, e.g. an older compiler generation was
//!   selected after sources changed) → the answer is still semantic but is
//!   **typed stale** ([`SemanticFreshness::Stale`]): every projected row
//!   carries a staleness note in its document and the projection ledger
//!   records the mismatch. The answer is never silently rewritten from
//!   structural rows, and never silently presented as fresh.
//!
//! ## Provenance: how a client tells which layer answered
//!
//! * Structural rows use `project::path:line::Name` coordinates and a prose
//!   document (`"<language> in <path>:<line>"` or the extracted doc comment).
//! * Semantic rows use `project::semantic::<identity>::<Name>` coordinates
//!   and a rendered code fragment document, and may carry typed type
//!   signatures the structural lane cannot produce.
//! * The search corpus tags every fact with its evidence class
//!   (`Compiler`, `CompilerExternalTarget`, or `StructuralFallback`), and
//!   the view coverage vector reports the semantic lane as complete, partial,
//!   or unavailable per package/profile.
//! * Within this crate the [`ProjectionLedger`] is the typed record of which
//!   lane answered for every file; tests and diagnostics read it rather than
//!   inferring lanes from coordinates.

use std::collections::BTreeMap;

use backend_engine::builtin::SemanticUnavailableReason;

/// Freshness of a semantic answer against the current scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticFreshness {
    /// The published image was compiled from exactly the current file set.
    Fresh,
    /// The published image exists but was compiled from a different source
    /// snapshot than the current scan (`content_version` mismatch). The answer
    /// stays semantic and is tagged stale; it is never replaced by structural
    /// rows and never presented as fresh.
    Stale,
}

/// Why the structural baseline answered for one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StructuralCause {
    /// No selected semantic publication exists for the file's profile yet
    /// (first scan, compile never ran, or compile still pending).
    NoCompletePublication,
    /// The selected semantic publication is an explicit terminal; the reason
    /// is retained so the client sees why semantics will not answer.
    PublicationUnavailable(SemanticUnavailableReason),
    /// The file's extension has no semantic profile at all, so no compile
    /// could ever cover it.
    NoSemanticProfile,
}

/// The typed lane decision for one source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileLane {
    /// The semantic lane answered with the stated freshness.
    Semantic {
        /// Whether the image matches the current scan.
        freshness: SemanticFreshness,
    },
    /// The structural baseline answered for the stated cause.
    Structural {
        /// Why semantics did not answer.
        cause: StructuralCause,
    },
}

/// Typed record of which lane answered for every projected source file.
///
/// The projection (view rows and query facts) consults this ledger instead of
/// inferring lanes from coordinates, so the contract above is enforced in one
/// place and every lane decision is assertable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ProjectionLedger {
    files: BTreeMap<String, FileLane>,
}

impl ProjectionLedger {
    /// Records the lane decision for one file.
    pub(super) fn record(&mut self, path: &str, lane: FileLane) {
        self.files.insert(path.to_owned(), lane);
    }

    /// Extends the ledger with decisions for whole files at once.
    pub(super) fn extend(&mut self, other: ProjectionLedger) {
        self.files.extend(other.files);
    }

    /// Returns the recorded lane decision for one file.
    #[allow(dead_code)]
    pub(super) fn file_lane(&self, path: &str) -> Option<FileLane> {
        self.files.get(path).copied()
    }

    /// Returns every recorded file lane decision.
    #[allow(dead_code)]
    pub(super) fn files(&self) -> impl Iterator<Item = (&str, FileLane)> {
        self.files.iter().map(|(path, lane)| (path.as_str(), *lane))
    }

    /// Returns whether every recorded file was answered semantically fresh.
    #[allow(dead_code)]
    pub(super) fn all_semantic_fresh(&self) -> bool {
        self.files.values().all(|lane| {
            matches!(
                lane,
                FileLane::Semantic {
                    freshness: SemanticFreshness::Fresh
                }
            )
        })
    }
}
