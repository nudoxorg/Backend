#![no_std]

//! This crate defines tools to implement datastructures that can live
//! in main memory or on-disk, meaning that their natural habitat is
//! memory-mapped files, but if that environment is threatened, they
//! might seek refuge in lower-level environments.
//!
//! One core building block of this library is the notion of virtual
//! memory pages, which are allocated and freed by an
//! externally-provided allocator (see how the `sanakirja` crate does
//! this). The particular implementation used here is meant to allow a
//! transactional system with readers reading the structures
//! concurrently with one writer at a time.
//!
//! At the moment, only B trees are implemented, as well as the
//! following general traits:
//!
//! - [`LoadPage`] is a trait used to get a pointer to a page. In the
//! most basic version, this may just return a pointer to the file,
//! offset by the requested offset. In more sophisticated versions,
//! this can be used to encrypt and compress pages.
//! - [`AllocPage`] allocates and frees pages, because as
//! datastructures need to be persisted on disk, we can't rely on
//! Rust's memory management to do it for us. Users of this crate
//! don't have to worry about this though.
//!
//! Moreover, two other traits can be used to store things on pages:
//! [`Storable`] is a simple trait that all `Sized + Ord` types
//! without references can readily implement (the [`direct_repr!`]
//! macro does that). For types containing references to pages
//! allocated in the database, the comparison function can be
//! customised. Moreover, these types must supply an iterator over
//! these references, in order for reference-counting to work properly
//! when the datastructures referencing these types are forked.
//!
//! Dynamically-sized types, or types that need to be represented in a
//! dynamically-sized way, can use the [`UnsizedStorable`] format.

pub mod btree;

/// There's a hard-coded assumption that pages have 4K bytes. This is
/// true for normal memory pages on almost all platforms.
pub const PAGE_SIZE: usize = 4096;

/// Types that can be stored on disk. This trait may be used in
/// conjunction with `Sized` in order to determine the on-disk size,
/// or with [`UnsizedStorable`] when special arrangements are needed.
///
/// The size limit for an entry depends on the datastructure. For B
/// trees, nodes are guaranteed to be at least half-full (i.e. at
/// least 2kiB), and we need at least two entries in each page in
/// order to be able to rebalance in a deletion. A sufficient
/// condition for this is that the size of any entry is less than
/// 1/4th of the blocks. Now, internal nodes need 16 bytes of block
/// header, and then 8 extra bytes for each entry. Entries must
/// therefore not exceed 4080/4 - 8 = 1012 bytes.

pub trait Storable: core::fmt::Debug {
    /// This is required for B trees, not necessarily for other
    /// structures. The default implementation panics.
    fn compare<T: LoadPage>(&self, _txn: &T, _b: &Self) -> core::cmp::Ordering {
        unimplemented!()
    }

    /// If this value is an offset to another page at offset `offset`,
    /// return `Some(offset)`. Return `None` else.
    fn page_references(&self) -> Self::PageReferences;

    /// If this value is an offset to another page at offset `offset`,
    /// return `Some(offset)`. Return `None` else.
    ///
    /// # Safety
    ///
    /// The caller must not keep any reference to the deleted value.
    unsafe fn drop<T: AllocPage>(&self, txn: &mut T) -> Result<(), T::Error> {
        for p in self.page_references() {
            txn.decr_rc(p)?;
        }
        Ok(())
    }

    /// An iterator over the offsets to pages contained in this
    /// value. Only values from this crate can generate non-empty
    /// iterators, but combined values (like tuples) must chain the
    /// iterators returned by method `page_offsets`.
    type PageReferences: Iterator<Item = u64>;
}

/// A trait to serialize the identity of complex types and check the
/// validity of database schemas.
#[cfg(feature = "typeids")]
pub trait TypeId {
    fn type_id() -> [u8; 32];
}

#[cfg(feature = "typeids")]
pub use sha2;
#[cfg(feature = "typeids")]
use sha2::Digest;

#[cfg(feature = "typeids")]
impl TypeId for () {
    fn type_id() -> [u8; 32] {
        [0; 32]
    }
}

/// A macro to implement [`Storable`] on "plain" types,
/// i.e. fixed-sized types that are `repr(C)` and don't hold
/// references.
#[macro_export]
macro_rules! direct_repr {
    ($t: ty) => {
        impl $crate::Storable for $t {
            type PageReferences = core::iter::Empty<u64>;
            fn page_references(&self) -> Self::PageReferences {
                core::iter::empty()
            }
            fn compare<T>(&self, _: &T, b: &Self) -> core::cmp::Ordering {
                self.cmp(b)
            }
        }
        impl $crate::UnsizedStorable for $t {
            const ALIGN: usize = core::mem::align_of::<$t>();

            /// If `Self::SIZE.is_some()`, this must return the same
            /// value. The default implementation is `Self;:SIZE.unwrap()`.
            fn size(&self) -> usize {
                core::mem::size_of::<Self>()
            }

            /// Read the size from an on-page entry.
            unsafe fn onpage_size(_: *const u8) -> usize {
                core::mem::size_of::<Self>()
            }

            unsafe fn write_to_page(&self, p: *mut u8) {
                core::ptr::copy_nonoverlapping(self, p as *mut Self, 1)
            }

            unsafe fn from_raw_ptr<'a, T>(_: &T, p: *const u8) -> &'a Self {
                &*(p as *const Self)
            }
        }
    };
}

direct_repr!(());
direct_repr!(u8);
direct_repr!(i8);
direct_repr!(u16);
direct_repr!(i16);
direct_repr!(u32);
direct_repr!(i32);
direct_repr!(u64);
direct_repr!(i64);
direct_repr!([u8; 16]);

#[cfg(feature = "std")]
extern crate std;
#[cfg(feature = "std")]
direct_repr!(std::net::Ipv4Addr);
#[cfg(feature = "std")]
direct_repr!(std::net::Ipv6Addr);
#[cfg(feature = "std")]
direct_repr!(std::net::IpAddr);
#[cfg(feature = "std")]
direct_repr!(std::net::SocketAddr);
#[cfg(feature = "std")]
direct_repr!(std::time::SystemTime);
#[cfg(feature = "std")]
direct_repr!(std::time::Duration);
#[cfg(feature = "uuid")]
direct_repr!(uuid::Uuid);
#[cfg(feature = "ed25519")]
direct_repr!(ed25519_zebra::VerificationKeyBytes);

/// Types that can be stored on disk.
pub trait UnsizedStorable: Storable {
    const ALIGN: usize;

    /// If `Self::SIZE.is_some()`, this must return the same
    /// value. The default implementation is `Self;:SIZE.unwrap()`.
    fn size(&self) -> usize;

    /// Read the size from an on-page entry. If `Self::SIZE.is_some()`
    /// this must be the same value.
    ///
    /// # Safety
    ///
    /// `p` must point to a valid, `Self::ALIGN`-aligned on-page representation of `Self`.
    unsafe fn onpage_size(_: *const u8) -> usize;

    /// Write to a page. Must not overwrite the allocated size, but
    /// this isn't checked (which is why it's unsafe).
    ///
    /// # Safety
    ///
    /// `p` must point to writable memory of at least `self.size()` bytes,
    /// aligned to `Self::ALIGN`.
    unsafe fn write_to_page(&self, _: *mut u8) {
        unimplemented!()
    }

    /// Write to a page. Must not overwrite the allocated size, but
    /// this isn't checked (which is why it's unsafe).
    ///
    /// This is similar to `write_to_page`, but allows the user to
    /// allocate a value as needed when inserting the value into the
    /// base; do not implement both methods, since only
    /// `write_to_page_alloc` gets called by the library.
    ///
    /// The default implementation just calls `write_to_page`.
    ///
    /// # Safety
    ///
    /// `p` must point to writable memory of at least `self.size()` bytes,
    /// aligned to `Self::ALIGN`.
    unsafe fn write_to_page_alloc<T: AllocPage>(&self, _: &mut T, p: *mut u8) {
        self.write_to_page(p)
    }

    /// Reconstruct a reference to `Self` from a raw on-page pointer.
    ///
    /// # Safety
    ///
    /// `p` must point to a valid, `Self::ALIGN`-aligned on-page representation of `Self`
    /// whose data is live for at least `'a`.
    unsafe fn from_raw_ptr<'a, T>(_: &T, p: *const u8) -> &'a Self;
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct Ref {
    p: u64,
    len: u64,
}

pub union Slice<'b> {
    len: u16,
    page: Ref,
    mem: Mem<'b>,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Mem<'b> {
    _len: u16,
    m: &'b [u8],
}

impl<'a> core::fmt::Debug for Slice<'a> {
    fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(fmt, "Slice({:?})", unsafe { self.len })
    }
}

impl<'a> core::convert::From<&'a [u8]> for Slice<'a> {
    fn from(m: &'a [u8]) -> Slice<'a> {
        let s = Slice {
            mem: Mem { _len: 513, m },
        };
        s
    }
}

impl<'a> Slice<'a> {
    pub fn as_bytes<T: LoadPage>(&self, txn: &T) -> Result<&[u8], T::Error> {
        Ok(unsafe {
            let len = u16::from_le(self.len) & 0xfff;
            if len == 512 {
                // Stored externally
                let p = txn.load_page(u64::from_le(self.page.p) & !0xfff)?;
                core::slice::from_raw_parts(p.data, u64::from_le(self.page.len) as usize)
            } else if len == 513 {
                // Stored in memory, not on any page
                self.mem.m
            } else {
                core::slice::from_raw_parts(
                    (&self.len as *const u16 as *const u8).add(2),
                    len as usize,
                )
            }
        })
    }
}

#[cfg(feature = "typeids")]
impl<'a> TypeId for Slice<'a> {
    fn type_id() -> [u8; 32] {
        let mut h = sha2::Sha256::new();
        h.update(b"sanakirja-core::Slice");
        h.finalize().into()
    }
}

impl<'a> Storable for Slice<'a> {
    type PageReferences = Pages;
    fn page_references(&self) -> Self::PageReferences {
        unsafe {
            let len = u16::from_le(self.len);
            if len == 512 {
                let plen = u64::from_le(self.page.len);
                let len_up = ((plen + PAGE_SIZE as u64 - 1) / PAGE_SIZE as u64) * PAGE_SIZE as u64;
                let offset = u64::from_le(self.page.p) & !0xfff;
                Pages {
                    offset,
                    limit: offset + len_up,
                }
            } else {
                Pages {
                    offset: 0,
                    limit: 0,
                }
            }
        }
    }
    fn compare<T: LoadPage>(&self, t: &T, b: &Self) -> core::cmp::Ordering {
        self.as_bytes(t).unwrap().cmp(b.as_bytes(t).unwrap())
    }
}

pub struct Pages {
    offset: u64,
    limit: u64,
}

impl Iterator for Pages {
    type Item = u64;
    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.limit {
            None
        } else {
            let o = self.offset;
            self.offset += PAGE_SIZE as u64;
            Some(o)
        }
    }
}

impl<'b> UnsizedStorable for Slice<'b> {
    const ALIGN: usize = 8;
    fn size(&self) -> usize {
        let s = unsafe {
            if u16::from_le(self.len) == 512 {
                // Stored externally
                16
            } else if u16::from_le(self.len) == 513 {
                // Stored in memory, not on any page
                if self.mem.m.len() > 510 {
                    16
                } else {
                    2 + self.mem.m.len()
                }
            } else {
                u16::from_le(self.len) as usize
            }
        };
        s
    }
    unsafe fn from_raw_ptr<'a, T>(_: &T, p: *const u8) -> &'a Self {
        &*(p as *const Self)
    }
    unsafe fn onpage_size(p: *const u8) -> usize {
        let p = &*(p as *const Self);
        if u16::from_le(p.len) == 512 {
            // Stored externally
            16
        } else if u16::from_le(p.len) == 513 {
            // Stored in memory, not on any page
            2 + p.mem.m.len()
        } else {
            u16::from_le(p.len) as usize
        }
    }

    unsafe fn write_to_page_alloc<T: AllocPage>(&self, t: &mut T, p: *mut u8) {
        if u16::from_le(self.len) == 512 {
            // Stored externally
            core::ptr::copy(&self.page as *const Ref as *const u8, p, 16)
        } else if self.len == 513 {
            // Alloc ?
            if self.mem.m.len() > 510 {
                let len = self.mem.m.len();
                let page = t
                    .alloc_contiguous((((len + PAGE_SIZE - 1) / PAGE_SIZE) * PAGE_SIZE) as u64)
                    .unwrap();
                assert!(page.0.offset > 0);
                core::ptr::copy_nonoverlapping(self.mem.m.as_ptr(), page.0.data, len);
                let p = &mut *(p as *mut Ref);
                p.len = (self.mem.m.len() as u64).to_le();
                p.p = (page.0.offset | 512).to_le();
            } else {
                let len = self.mem.m.len();
                *(p as *mut u16) = (len as u16).to_le();
                core::ptr::copy_nonoverlapping(self.mem.m.as_ptr(), p.add(2), len)
            }
        } else {
            core::ptr::copy(
                &self.len as *const u16 as *const u8,
                p,
                2 + u16::from_le(self.len) as usize,
            )
        }
    }
}

impl Storable for [u8] {
    type PageReferences = core::iter::Empty<u64>;
    fn page_references(&self) -> Self::PageReferences {
        core::iter::empty()
    }
    fn compare<T>(&self, _: &T, b: &Self) -> core::cmp::Ordering {
        self.cmp(b)
    }
}

impl UnsizedStorable for [u8] {
    const ALIGN: usize = 2;
    fn size(&self) -> usize {
        2 + self.len()
    }
    unsafe fn from_raw_ptr<'a, T>(_: &T, p: *const u8) -> &'a Self {
        let len = u16::from_le(*(p as *const u16));
        assert_ne!(len, 0);
        assert_eq!(len & 0xf000, 0);
        core::slice::from_raw_parts(p.add(2), len as usize)
    }
    unsafe fn onpage_size(p: *const u8) -> usize {
        let len = u16::from_le(*(p as *const u16));
        2 + len as usize
    }
    unsafe fn write_to_page_alloc<T: AllocPage>(&self, _txn: &mut T, p: *mut u8) {
        assert!(self.len() <= 510);
        *(p as *mut u16) = (self.len() as u16).to_le();
        core::ptr::copy_nonoverlapping(self.as_ptr(), p.add(2), self.len())
    }
}

/// # Safety
/// `k` must point to a valid, `K::ALIGN`-aligned on-page `K` entry immediately
/// followed (with `V::ALIGN` padding) by a valid `V` entry.
unsafe fn read<T: LoadPage, K: UnsizedStorable + ?Sized, V: UnsizedStorable + ?Sized>(
    _txn: &T,
    k: *const u8,
) -> (*const u8, *const u8) {
    let s = K::onpage_size(k);
    let v = k.add(s);
    let al = v.align_offset(V::ALIGN);
    let v = v.add(al);
    (k, v)
}

/// # Safety
/// `k` must point to a valid, `K::ALIGN`-aligned on-page `K` entry immediately
/// followed by a valid `V` entry. `K::ALIGN` must be a power of two.
unsafe fn entry_size<K: UnsizedStorable + ?Sized, V: UnsizedStorable + ?Sized>(
    k: *const u8,
) -> usize {
    assert_eq!(k.align_offset(K::ALIGN), 0);
    let ks = K::onpage_size(k);
    // next multiple of va, assuming va is a power of 2.
    let v_off = (ks + V::ALIGN - 1) & !(V::ALIGN - 1);
    let v_ptr = k.add(v_off);
    let vs = V::onpage_size(v_ptr);
    let ka = K::ALIGN.max(V::ALIGN);
    let size = v_off + vs;
    (size + ka - 1) & !(ka - 1)
}

/// Representation of a mutable or shared page. This is an owned page
/// (like `Vec` in Rust's std), but we do not know whether we can
/// mutate it or not.
///
/// The least-significant bit of the first byte of each page is 1 if
/// and only if the page was allocated by the current transaction (and
/// hence isn't visible to any other transaction, meaning we can write
/// on it).
#[derive(Debug)]
#[repr(C)]
pub struct CowPage {
    pub data: *mut u8,
    pub offset: u64,
}

/// Representation of a borrowed, or immutable page, like a slice in
/// Rust.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Page<'a> {
    pub data: &'a [u8; PAGE_SIZE],
    pub offset: u64,
}

impl CowPage {
    /// Borrows the page.
    pub fn as_page<'a>(&'a self) -> Page<'a> {
        Page {
            data: unsafe { &*(self.data as *const [u8; PAGE_SIZE]) },
            offset: self.offset,
        }
    }

    /// # Safety
    ///
    /// `self.data` must point to a valid, fully-written `PAGE_SIZE`-byte page.
    #[cfg(feature = "crc32")]
    pub unsafe fn crc(&self, hasher: &crc32fast::Hasher) -> u32 {
        crc(self.data, hasher)
    }

    /// # Safety
    ///
    /// `self.data` must point to a valid, fully-written `PAGE_SIZE`-byte page.
    #[cfg(feature = "crc32")]
    pub unsafe fn crc_check(&self, hasher: &crc32fast::Hasher) -> bool {
        crc_check(self.data, hasher)
    }
}

/// An owned page on which we can write. This is just a wrapper around
/// `CowPage` to avoid checking the "dirty" bit at runtime.
#[derive(Debug)]
pub struct MutPage(pub CowPage);

impl MutPage {
    #[cfg(not(feature = "crc32"))]
    pub unsafe fn clear_dirty(&mut self) {
        *self.0.data &= 0xfe
    }

    #[cfg(feature = "crc32")]
    pub unsafe fn clear_dirty(&mut self, hasher: &crc32fast::Hasher) {
        *self.0.data &= 0xfe;
        let crc_ = (self.0.data as *mut u32).add(1);
        *crc_ = crc(self.0.data, hasher)
    }
}

/// # Safety
///
/// `data` must point to a valid, fully-written `PAGE_SIZE`-byte page header.
#[cfg(feature = "crc32")]
pub unsafe fn crc(data: *mut u8, hasher: &crc32fast::Hasher) -> u32 {
    let mut hasher = hasher.clone();
    hasher.reset();
    // Hash the beginning and the end of the page (i.e. remove
    // the CRC).
    unsafe {
        // Remove the dirty bit.
        let x = [(*data) & 0xfe];
        hasher.update(&x[..]);
        hasher.update(core::slice::from_raw_parts(data.add(1), 3));
        hasher.update(core::slice::from_raw_parts(data.add(8), PAGE_SIZE - 8));
    }
    hasher.finalize()
}

/// # Safety
///
/// `data` must point to a valid, fully-written `PAGE_SIZE`-byte page header,
/// with the CRC stored at bytes 4–7 in little-endian order.
#[cfg(feature = "crc32")]
pub unsafe fn crc_check(data: *mut u8, hasher: &crc32fast::Hasher) -> bool {
    let crc_ = unsafe { u32::from_le(*(data as *const u32).add(1)) };
    crc(data, hasher) == crc_
}

#[cfg(not(feature = "crc32"))]
/// # Safety
/// `p` must point to a valid, writable page header (at least 1 byte).
pub unsafe fn clear_dirty(p: *mut u8) {
    unsafe { *p &= 0xfe }
}

#[cfg(feature = "crc32")]
/// # Safety
/// `p` must point to a valid, writable page header of at least `PAGE_SIZE` bytes,
/// with the CRC field at bytes 4–7.
pub unsafe fn clear_dirty(p: *mut u8, hasher: &crc32fast::Hasher) {
    unsafe {
        *p &= 0xfe;
        let crc_ = (p as *mut u32).add(1);
        *crc_ = crc(p, hasher)
    }
}

unsafe impl Sync for CowPage {}
unsafe impl Send for CowPage {}

/*
impl CowPage {
    /// Checks the dirty bit of a page.
    pub fn is_dirty(&self) -> bool {
        unsafe { (*self.data) & 1 != 0 }
    }
}
 */

/// Trait for loading a page.
pub trait LoadPage {
    type Error: core::fmt::Debug;
    /// Loading a page.
    ///
    /// # Safety
    ///
    /// `off` must be a valid page offset previously returned by `alloc_page`
    /// or `alloc_contiguous`, within the bounds of this environment.
    unsafe fn load_page(&self, off: u64) -> Result<CowPage, Self::Error>;

    /// Loading multiple pages written contiguously in the underlying
    /// storage media.
    ///
    /// If the type also implements `AllocPage`, attention must be
    /// paid to the compatibility with `alloc_contiguous`.
    ///
    /// # Safety
    ///
    /// `off` must be a valid offset previously returned by `alloc_contiguous`,
    /// and `len` must match the size of that contiguous allocation.
    unsafe fn load_page_contiguous(&self, _off: u64, _len: u64) -> Result<CowPage, Self::Error> {
        unimplemented!()
    }

    /// Reference-counting. Since reference-counts are designed to be
    /// storable into B trees by external allocators, pages referenced
    /// once aren't stored, and hence are indistinguishable from pages
    /// that are never referenced. The default implementation returns
    /// 0.
    ///
    /// This has the extra benefit of requiring less disk space, and
    /// isn't more unsafe than storing the reference count, since we
    /// aren't supposed to hold a reference to a page with "logical
    /// RC" 0, so storing "1" for that page would be redundant anyway.
    fn rc(&self, _off: u64) -> Result<u64, Self::Error> {
        Ok(0)
    }
}

/// Trait for allocating and freeing pages.
pub trait AllocPage: LoadPage {
    /// Allocate a new page.
    ///
    /// # Safety
    ///
    /// The returned page's contents are uninitialized (except for the dirty bit).
    /// The caller must initialize it before exposing it to safe code.
    unsafe fn alloc_page(&mut self) -> Result<MutPage, Self::Error>;

    /// Allocate a new page, in a context where we cannot use the
    /// "dirty bit" trick directly on the page.
    ///
    /// # Safety
    ///
    /// The returned page's contents are uninitialized.
    /// The caller must initialize it before exposing it to safe code.
    unsafe fn alloc_page_no_dirty(&mut self) -> Result<MutPage, Self::Error> {
        unimplemented!()
    }

    /// Allocate many contiguous pages, return the first one. The
    /// dirty bit is not needed.
    ///
    /// # Safety
    ///
    /// `length` must be a multiple of `PAGE_SIZE`. The returned pages are
    /// contiguous and their contents are uninitialized.
    unsafe fn alloc_contiguous(&mut self, length: u64) -> Result<MutPage, Self::Error>;

    /// Increment the page's reference count.
    fn incr_rc(&mut self, off: u64) -> Result<usize, Self::Error>;

    /// Decrement the page's reference count, assuming the page was
    /// first allocated by another transaction. If the RC reaches 0,
    /// free the page. Must return the new RC (0 if freed).
    ///
    /// # Safety
    ///
    /// `off` must be a valid page offset allocated by a **previous** transaction
    /// with a reference count of at least 1. Use [`Self::decr_rc_owned`] for
    /// pages allocated by the current transaction.
    unsafe fn decr_rc(&mut self, off: u64) -> Result<usize, Self::Error>;

    /// Same as [`Self::decr_rc`], but for pages allocated by the current
    /// transaction. This is an important distinction, as pages
    /// allocated by the current transaction can be reused immediately
    /// after being freed.
    ///
    /// # Safety
    ///
    /// `off` must be a valid page offset allocated by the **current** transaction.
    unsafe fn decr_rc_owned(&mut self, off: u64) -> Result<usize, Self::Error>;

    fn is_dirty(&self, p: &CowPage) -> bool {
        unsafe { (*p.data) & 1 != 0 }
    }
}
