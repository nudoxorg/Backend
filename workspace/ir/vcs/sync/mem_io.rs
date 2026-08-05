//! In-memory `ContentIo` test double.

use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
};

use heart::sync::{ContentIo, VerifyError};

use super::types::ChangeId;

/// In-memory [`ContentIo`] test double.
///
/// Stores change bytes in a `HashMap` keyed by `ChangeId`. The `verify` method
/// uses BLAKE3 — test change IDs must be the 64-hex BLAKE3 hash of their bytes.
#[derive(Clone, Debug, Default)]
pub struct MemChangeIo {
    inner: Arc<Mutex<HashMap<ChangeId, Vec<u8>>>>,
}

impl MemChangeIo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: ChangeId, bytes: Vec<u8>) {
        self.inner.lock().expect("poisoned").insert(id, bytes);
    }

    pub fn keys(&self) -> Vec<ChangeId> {
        self.inner
            .lock()
            .expect("poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

impl ContentIo for MemChangeIo {
    type Id = ChangeId;

    fn read(&self, id: &ChangeId) -> io::Result<Vec<u8>> {
        self.inner
            .lock()
            .expect("poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("change {id} not found"))
            })
    }

    fn write(&self, id: &ChangeId, bytes: &[u8]) -> io::Result<()> {
        self.inner
            .lock()
            .expect("poisoned")
            .insert(id.clone(), bytes.to_vec());
        Ok(())
    }

    fn has(&self, id: &ChangeId) -> io::Result<bool> {
        Ok(self.inner.lock().expect("poisoned").contains_key(id))
    }

    fn verify(&self, id: &ChangeId, bytes: &[u8]) -> Result<(), VerifyError> {
        let hash = blake3::hash(bytes);
        let hex = hex_of_blake3(hash.as_bytes());
        if hex != id.as_str() {
            return Err(VerifyError::HashMismatch {
                expected: id.to_string(),
                got: hex,
            });
        }
        Ok(())
    }

    fn max_item_bytes(&self) -> usize {
        super::MAX_CHANGE_BYTES
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn hex_of_blake3(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        let hi = char::from_digit((b >> 4) as u32, 16).expect("hex digit");
        let lo = char::from_digit((b & 0xf) as u32, 16).expect("hex digit");
        out.push(hi);
        out.push(lo);
    }
    out
}

pub fn change_id_for_bytes(bytes: &[u8]) -> ChangeId {
    let hash = blake3::hash(bytes);
    ChangeId(hex_of_blake3(hash.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use heart::sync::ContentIo;

    #[test]
    fn mem_change_io_roundtrip() {
        let store = MemChangeIo::new();
        let data = b"hello pijul";
        let id = change_id_for_bytes(data);

        assert!(!store.has(&id).unwrap());
        store.verify(&id, data).unwrap();
        store.write(&id, data).unwrap();
        assert!(store.has(&id).unwrap());
        assert_eq!(store.read(&id).unwrap(), data);
    }

    #[test]
    fn verify_tampered_bytes_fails() {
        let store = MemChangeIo::new();
        let data = b"good bytes";
        let id = change_id_for_bytes(data);
        let tampered = b"bad bytes!!";
        let err = store.verify(&id, tampered).unwrap_err();
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }
}
