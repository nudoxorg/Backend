#![deny(
    missing_docs,
    trivial_casts,
    trivial_numeric_casts,
    unused_import_braces,
    unused_qualifications
)]
//! Transactional, on-disk datastructures with concurrent readers and
//! writers (writers exclude each other).
//!
//! This crate is based on the no-std crate `sanakirja-core`, whose
//! goal is to implement different datastructures.
//!
//! Here's an example of how to use it (starting with 64 pages, 2
//! versions, see below for details about what that means). The file
//! grows automatically, as needed.
//!
//! ```
//! use sanakirja::*;
//! let dir = tempfile::tempdir().unwrap();
//! let path = dir.path().join("db");
//! let env = Env::new(&path, 1 << 20, 2).unwrap();
//! let mut txn = Env::mut_txn_begin(&env).unwrap();
//! let mut db = unsafe { btree::create_db::<_, u64, u64>(&mut txn).unwrap() };
//! for i in 0..100_000u64 {
//!     btree::put(&mut txn, &mut db, &i, &(i*i)).unwrap();
//! }
//! let root_db = 0;
//! txn.set_root(root_db, db.db.into());
//! txn.commit().unwrap();
//! let txn = Env::txn_begin(&env).unwrap();
//! let db: btree::Db<u64, u64> = txn.root_db(root_db).unwrap();
//! assert_eq!(btree::get(&txn, &db, &50_000, None).unwrap(), Some((&50_000, &(50_000 * 50_000))));
//! for entry in btree::iter(&txn, &db, None).unwrap() {
//!   let (k, v) = entry.unwrap();
//!   assert_eq!(*k * *k, *v)
//! }
//! ```
//!
//! The binary format of a Sanakirja database is the following:
//!
//! - There is a fixed number of "current versions", set at file
//! initialisation. If a file has n versions, then for all k between 0
//! and n-1 (included), the k^th page (i.e. the byte positions between
//! `k * 4096` and `(k+1) * 4096`, also written as `k << 12` and
//! `(k+1) << 12`) stores the data relative to that version, and is
//! called the "root page" of that version.
//!
//!   This is a way to handle concurrent access: indeed, mutable
//! transactions do not exclude readers, but readers that started
//! before the commit of a mutable transaction will keep reading the
//! database as it was before the commit. However, this means that
//! older versions of the database have to be kept "alive", and the
//! "number of current versions" here is the limit on the number of
//! versions that can be kept "alive" at the same time.
//!
//!   When a reader starts, it takes a shared file lock on the file
//! representing the youngest committed version. When a writer starts,
//! it takes an exclusive file lock on the file representing the
//! oldest committed version. This implies that if readers are still
//! reading that version, the writer will wait for the exclusive lock.
//!
//!   After taking a lock, the writer (also called "mutable
//! transaction" or [`MutTxn`]) copies the entire root page of the
//! youngest committed version onto the root page of the oldest
//! committed version, hence erasing the root page of the oldest
//! version.
//!
//! - Root pages have the following format: a 32-bytes header
//! (described below), followed by 4064 bytes, usable in a more or
//! less free format. The current implementation defines two methods
//! on [`MutTxn`], [`MutTxn::set_root`] and [`MutTxn::remove_root`],
//! treating that space as an array of type `[u64; 510]`. A reasonable
//! use for these is to point to different datastructures allocated in
//! the file, such as the offsets in the file to the root pages of B
//! trees.
//!
//!   Now, about the header, there's a version identifier on the first
//! 16 bytes, followed by two bytes: `root` is the version used by the
//! current mutable transaction (if there is current mutable
//! transaction), or by the next mutable transaction (else). The
//! `n_roots` field is the total number of versions.
//!
//!   ```
//!   #[repr(C)]
//!   pub struct GlobalHeader {
//!       /// Version of Sanakirja
//!       pub version: u16,
//!       /// Which page is currently the root page? (only valid for page 0).
//!       pub root: u8,
//!       /// Total number of versions (or "root pages")
//!       pub n_roots: u8,
//!       /// CRC of this page.
//!       pub crc: u32,
//!       /// First free page at the end of the file (only valid for page 0).
//!       pub length: u64,
//!       /// Offset of the free list.
//!       pub free_db: u64,
//!       /// Offset of the RC database.
//!       pub rc_db: u64,
//!   }
//!   ```

use thiserror::*;

mod environment;
pub use environment::{Commit, Env, MutTxn, RootDb, RootPage, RootPageMut, Txn};
pub use sanakirja_core::{
    btree, direct_repr, AllocPage, CowPage, LoadPage, MutPage, Page, Slice, Storable,
    UnsizedStorable,
};

#[cfg(test)]
mod tests;

#[doc(hidden)]
pub mod debug;

/// Errors that can occur while transacting.
#[derive(Debug, Error)]
pub enum Error {
    /// IO errors, from the `std::io` module.
    #[error(transparent)]
    IO(#[from] std::io::Error),
    /// Lock poisoning error.
    #[error("Lock poisoning")]
    Poison,
    /// Version mismatch
    #[error("Version mismatch")]
    VersionMismatch,
    /// CRC check failed
    #[error(transparent)]
    CRC(#[from] CRCError),
    /// Corruption error
    #[error("Corruption error: offset {0} is past the end of the file")]
    Corrupt(u64),
}

/// A CRC check failed
#[derive(Debug, Error)]
#[error("CRC check failed")]
pub struct CRCError {}

/// A 64-bit unsigned integer in little-endian ordering.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct L64(pub u64);

impl std::fmt::Debug for L64 {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "L64({})", u64::from_le(self.0))
    }
}

impl serde::Serialize for L64 {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(u64::from_le(self.0))
    }
}

use serde::de::{self, Visitor};

struct L64Visitor;

impl<'de> Visitor<'de> for L64Visitor {
    type Value = L64;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an unsigned, little-endian integer")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        log::debug!("visit u64 {:?}", value);
        Ok(L64(value.to_le()))
    }
}

impl<'de> serde::Deserialize<'de> for L64 {
    fn deserialize<D>(deserializer: D) -> Result<L64, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_u64(L64Visitor)
    }
}

impl From<u64> for L64 {
    fn from(u: u64) -> Self {
        L64(u.to_le())
    }
}

impl From<L64> for u64 {
    fn from(u: L64) -> Self {
        u64::from_le(u.0)
    }
}

impl Ord for L64 {
    fn cmp(&self, x: &Self) -> std::cmp::Ordering {
        u64::from_le(self.0).cmp(&u64::from_le(x.0))
    }
}

impl PartialOrd for L64 {
    fn partial_cmp(&self, x: &Self) -> Option<std::cmp::Ordering> {
        u64::from_le(self.0).partial_cmp(&u64::from_le(x.0))
    }
}

impl From<usize> for L64 {
    fn from(u: usize) -> Self {
        L64((u as u64).to_le())
    }
}

impl From<L64> for usize {
    fn from(u: L64) -> Self {
        u64::from_le(u.0) as usize
    }
}

impl L64 {
    /// Convert to machine 64-bit integers
    pub fn as_u64(&self) -> u64 {
        u64::from_le(self.0)
    }
    /// Convert to usize
    pub fn as_usize(&self) -> usize {
        u64::from_le(self.0) as usize
    }
}

impl std::fmt::Display for L64 {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        u64::from_le(self.0).fmt(fmt)
    }
}

impl std::ops::Add<L64> for L64 {
    type Output = Self;
    fn add(self, x: L64) -> L64 {
        L64((u64::from_le(self.0) + u64::from_le(x.0)).to_le())
    }
}

impl std::ops::Add<usize> for L64 {
    type Output = Self;
    fn add(self, x: usize) -> L64 {
        L64((u64::from_le(self.0) + x as u64).to_le())
    }
}

impl std::ops::SubAssign<usize> for L64 {
    fn sub_assign(&mut self, x: usize) {
        self.0 = ((u64::from_le(self.0)) - x as u64).to_le()
    }
}

#[allow(trivial_casts)]
impl L64 {
    /// Read an L64 from its binary representation.
    pub fn from_slice_le(s: &[u8]) -> Self {
        let mut u = 0u64;
        assert!(s.len() >= 8);
        unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), &mut u as *mut u64 as *mut u8, 8) }
        L64(u)
    }
    /// Write an L64 as its binary representation.
    pub fn to_slice_le(&self, s: &mut [u8]) {
        assert!(s.len() >= 8);
        unsafe {
            std::ptr::copy_nonoverlapping(&self.0 as *const u64 as *const u8, s.as_mut_ptr(), 8)
        }
    }
}

direct_repr!(L64);
impl debug::Check for L64 {}
