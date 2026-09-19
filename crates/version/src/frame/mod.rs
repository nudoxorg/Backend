//! The `frame` module exists to encode canonical bounded frames into caller-owned storage.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Canonical bounded frame writing into caller-owned storage.

mod encode;

pub use encode::{
    EncodeError, EncodedFrameLength, InputSectionCount, NativeByteCount, PreparedFrame,
    SectionInput, encode_into, encoded_len,
};
