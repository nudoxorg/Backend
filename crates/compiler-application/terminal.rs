//! Defines terminal behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed setup errors and terminal projection module boundaries.

mod common;
mod native;
mod publication;

use thiserror::Error;

pub(crate) use common::source_authority;
pub(crate) use native::compile_terminal;
pub(crate) use publication::{
    semantic_artifact_terminal, semantic_publication_terminal, semantic_reopen_absent,
    semantic_reopen_cardinality, semantic_reopen_terminal,
};

/// Rejection while creating the explicit durable publication owner.
#[derive(Debug, Error)]
pub enum LocalCompilerOpenError {
    /// A local storage path was relative and would consult ambient process state.
    #[error("local compiler {path:?} path is not absolute")]
    RelativePath {
        /// Named local storage role whose path must be absolute.
        path: LocalCompilerPath,
    },
    /// The supplied durable journal directory could not create its local publication owner.
    #[error("could not create local durable compiler publication owner")]
    Publisher(#[source] server_journal::PublicationOpenError),
}

/// Named explicit local storage path required by one compiler capability instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCompilerPath {
    /// Immutable compact IR artifact directory.
    Artifacts,
    /// Durable journal/fact/head directory.
    Journal,
    /// Empty native compiler work directory.
    NativeWork,
}
