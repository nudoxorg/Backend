//! Defines contract behavior for `server-operation`, whose purpose is to compose hydration, compilation, publication, and storage into public operations.
//! This module owns the contract invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Static synchronous operation/cursor algebra.
#![allow(
    clippy::missing_errors_doc,
    clippy::type_complexity,
    reason = "GAT batch borrowing exposes the exact static operation result without erasure"
)]

use backend_version::schema::OperationId;

/// A statically known operation contract.
pub trait Operation {
    /// Request admitted for this operation.
    type Request;
    /// One result item.
    type Item;
    /// Exact unavailable coverage for normal partial completion.
    type Missing;
    /// Request/provider binding failure before a source exists.
    type StartError;
    /// Failure while advancing an already validated source.
    type SourceError;
    /// Closed Wave 1 operation identity.
    const ID: OperationId;
}

/// Starts one concrete cursor for one concrete operation.
pub trait Provider<ConcreteOperation: Operation> {
    /// Concrete source, never a boxed stream/future.
    type Run: BatchSource<ConcreteOperation>;
    /// Validates and starts the source.
    fn start(
        &self,
        request: ConcreteOperation::Request,
    ) -> Result<Self::Run, ConcreteOperation::StartError>;
}

/// One synchronous cursor outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourcePoll<Batch, Missing> {
    /// One bounded borrowed batch.
    Batch(Batch),
    /// The sole explicit terminal fact.
    Terminal(TerminalSummary<Missing>),
    /// Terminal was already observed; a fused cursor cannot invent a second fact.
    Finished,
}

/// Lending synchronous source, deliberately free of scheduling/wakeup semantics.
pub trait BatchSource<ConcreteOperation: Operation> {
    /// Batch borrowing the source.
    type Batch<'source>: core::ops::Deref<Target = [ConcreteOperation::Item]>
        + AsRef<[ConcreteOperation::Item]>
    where
        Self: 'source;
    /// Advances once.
    fn next_batch(
        &mut self,
    ) -> Result<
        SourcePoll<Self::Batch<'_>, ConcreteOperation::Missing>,
        ConcreteOperation::SourceError,
    >;
}

/// Exact terminal coverage for a typed operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalSummary<Missing> {
    /// Complete requested coverage.
    Complete {
        /// Exact emitted rows.
        emitted: u32,
    },
    /// Normal partial coverage with exact absence.
    Partial {
        /// Exact emitted rows.
        emitted: u32,
        /// What was unavailable and why.
        missing: Missing,
    },
    /// Cancellation before normal terminal coverage.
    Cancelled {
        /// Exact rows emitted before cancellation.
        emitted: u32,
    },
}
