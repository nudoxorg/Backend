//! `ir-stream` — guest→host IR streaming protocol (SMOLVM-PLAN §6.1).
//!
//! This crate defines the postcard frame protocol that runs over a dedicated
//! vsock port from a guest producer to the host recording session. It is a
//! **leaf crate** with no runtime or async dependencies — everything is generic
//! over `std::io::{Read, Write}`.
//!
//! # K27 discipline (SV-10)
//!
//! Every frame carries **only** stable identities ([`nudox_ir::change::StableRef`]),
//! payload wire twins ([`nudox_ir::wire::OwnedEntryPayload`]), and content
//! digests ([`heart::content::ContentHash`]). Arena indices and interned string
//! ids cannot be expressed in any of the types in this crate — the constraint is
//! enforced by construction, not convention.
//!
//! # Protocol handshake
//!
//! 1. Producer sends [`StreamFrame::Hello`] (version + job + producer id).
//! 2. Producer streams any number of [`StreamFrame::Symbols`],
//!    [`StreamFrame::Links`], [`StreamFrame::SourceDigest`],
//!    [`StreamFrame::Occurrences`], [`StreamFrame::Bodies`], and
//!    [`StreamFrame::Progress`] frames.
//! 3. Producer terminates with exactly one [`StreamFrame::Finish`] or
//!    [`StreamFrame::Abort`].
//!
//! The host validates ordering and the emitted-count invariant; all violations
//! surface as typed [`StreamError`] variants.
//!
//! # Framing
//!
//! Each frame is encoded with postcard, then written as:
//! `[length: u32 LE][postcard bytes]`
//!
//! Frames larger than [`MAX_FRAME_BYTES`] are rejected at both ends — the writer
//! returns [`StreamError::FrameTooLarge`] before writing; the reader returns the
//! same error if the declared length exceeds the cap, without allocating.

pub mod error;
pub mod frame;
pub mod io;
pub mod receiver;
pub mod sink;

pub use error::StreamError;
pub use frame::{
    BodyWire, FailureKindWire, PhaseWire, ProducerId, StreamFrame, WireEntry, WireLink,
    IR_STREAM_VERSION, IR_STREAM_VSOCK_PORT, MAX_FRAME_BYTES,
};
pub use io::{FrameReader, FrameWriter};
pub use receiver::{Received, StreamReceiver};
pub use sink::SymbolSink;

#[cfg(test)]
mod tests;

