//! Cryptographically secure random bytes from the Windows system generator.
//! This is the counterpart of reading `/dev/urandom` on Unix, and the same source the Rust standard library uses.
//! The call cannot fail on supported Windows versions, but its status is still checked.
#![allow(
    unsafe_code,
    reason = "one call into the system random generator with a checked buffer length"
)]

use std::io;
use windows_sys::Win32::Security::Cryptography::ProcessPrng;

/// Fills `bytes` from the system cryptographic random generator.
///
/// # Errors
/// Returns an error if the generator reports a failure.
pub fn fill(bytes: &mut [u8]) -> io::Result<()> {
    // SAFETY: the pointer and length describe exactly the writable slice.
    if unsafe { ProcessPrng(bytes.as_mut_ptr(), bytes.len()) } == 0 {
        return Err(io::Error::other("system random generator failed"));
    }
    Ok(())
}
