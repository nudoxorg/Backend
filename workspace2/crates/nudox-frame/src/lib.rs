#![no_std]
#![forbid(unsafe_code)]
//! Canonical bounded frame writing into caller-owned storage.

mod encode;

pub use encode::{
    EncodeError, EncodedFrameLength, InputSectionCount, NativeByteCount, PreparedFrame,
    SectionInput, encode_into, encoded_len,
};
