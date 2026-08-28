use super::*;
use crate::L64;
use sanakirja_core::{btree, CowPage, MutPage};
use std::borrow::Borrow;

impl<E: Borrow<Env>> std::fmt::Debug for MutTxn<E> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "MutTxn {{ }}")
    }
}

/// A mutable transaction.
pub struct MutTxn<E: Borrow<Env>> {
    pub(crate) env: E,
    /// The root page of this transaction, which is 1 + the root
    /// written on page 0. The root written on page 0 changes at
    /// commit time.
    pub(crate) root: usize,
    pub(crate) length: u64,

    /// Offset to the root of the B tree of free pages.
    pub(crate) free: u64,

    /// Reference counts use a strange encoding, meant to avoid code
    /// bloat: indeed, the list of free pages uses `Db<u64, ()>`, so
    /// we're just reusing the same code here, encoding the reference
    /// counts in the 12 least significant bits of the keys, and the
    /// actual pages in the 52 most significant bits.
    pub(crate) rc: Option<btree::Db<L64, ()>>,

    /// RC for child transactions. Unmodified RCs compared to the
    /// parent aren't stored.
    child_rc: std::collections::HashMap<u64, usize>,

    /// root page if we need to modify it.
    child_page: Option<Box<[u8; 4064]>>,

    /// Offsets of pages that were allocated by this transaction, and
    /// have not been freed since. The boolean indicates whether this
    /// was allocated with a dirty bit or not.
    pub(crate) occupied_owned_pages: Vec<(MutPage, bool)>,

    /// Offsets of pages that were allocated by this transaction, and
    /// then freed.
    pub(crate) free_owned_pages: Vec<u64>,

    /// Offsets of old pages freed by this transaction. These were
    /// *not* allocated by this transaction.
    ///
    /// Since we can't reuse them in the same transaction, another
    /// option would be to put them directly into the table of free
    /// pages. However, since calls to `put` may allocate and free
    /// pages, this could recurse infinitely, which is why we store
    /// them outside of the file.
    pub(crate) free_pages: Vec<u64>,

    /// Offsets of pages that were free at the start of the
    /// transaction, and are still free.
    initial_free: Vec<u64>,
    /// Offsets of pages that were free at the start of the
    /// transaction, but have been allocated since then.
    initial_allocated: Vec<u64>,

    roots: Vec<u64>,

    to_delete: Vec<u64>,
}

/// A mutable transaction.
pub struct SubTxn<E: Borrow<Env>, Parent> {
    length: u64,
    parent: Parent,
    child_rc: std::collections::HashMap<u64, usize>,
    child_page: Option<Box<[u8; 4064]>>,
    occupied_owned_pages: std::collections::HashMap<u64, (MutPage, bool)>,
    free_owned_pages: Vec<u64>,
    free_pages: Vec<u64>,
    initial_free: Vec<u64>,
    initial_allocated: Vec<u64>,
    roots: Vec<u64>,
    env: std::marker::PhantomData<E>,
}

impl<E: Borrow<Env>> MutTxn<E> {
    /// Borrow env
    pub fn env_borrow(&self) -> &Env {
        self.env.borrow()
    }

    fn set_rc(&mut self, off: u64, rc: usize) -> Result<(), Error> {
        /*if self.parent.is_some() {
                self.child_rc.insert(off, rc);
        } else*/
        if let Some(mut rc_db) = self.rc.take() {
            let x = if rc >= 2 {
                btree::put(self, &mut rc_db, &L64((off | (rc as u64)).to_le()), &())
            } else {
                let mut curs = btree::cursor::Cursor::new(self, &rc_db)?;
                curs.set(self, &L64(off.to_le()), None)?;
                btree::del::del_at_cursor(self, &mut rc_db, &mut curs, true)
            };
            self.rc = Some(rc_db);
            x?;
        }
        Ok(())
    }
}

impl<E: Borrow<Env>> Borrow<Env> for MutTxn<E> {
    /// Borrow env
    fn borrow(&self) -> &Env {
        self.env.borrow()
    }
}

impl<E: Borrow<Env>, T> Borrow<T> for SubTxn<E, T> {
    /// Borrow env
    fn borrow(&self) -> &T {
        self.parent.borrow()
    }
}

impl<'a, E: Borrow<Env>> Borrow<Env> for &'a MutTxn<E> {
    fn borrow(&self) -> &Env {
        self.env.borrow()
    }
}

impl<'a, E: Borrow<Env>> Borrow<Env> for &'a mut MutTxn<E> {
    fn borrow(&self) -> &Env {
        self.env.borrow()
    }
}

/// When dropping a transaction, we need to unlock the read-write
/// locks internal to this process, and possibly the file locks.
impl<E: Borrow<Env>> Drop for MutTxn<E> {
    fn drop(&mut self) {
        let env = self.env.borrow();
        unsafe {
            env.mut_txn_unlock().unwrap_or(());
            env.roots[self.root].rw.unlock_exclusive();
            env.unlock(self.root).unwrap_or(())
        }
    }
}

impl<E: Borrow<Env>, T> SubTxn<E, T> {
    pub fn abort(self) -> T {
        self.parent
    }
}

/// Transactions that can be committed. This trait is an abstraction
/// over mutable transactions and their subtransactions.
pub trait Commit {
    /// Parent of this transaction, if any (and `()` on `MutTxn`).
    type Parent;
    /// Commit the transaction.
    fn commit(self) -> Result<Self::Parent, Error>;
}

/// The following is very easy, we're just extending all values of the
/// current transaction with values of the subtransaction.
impl<E: Borrow<Env>> Commit for SubTxn<E, MutTxn<E>> {
    type Parent = MutTxn<E>;
    fn commit(mut self) -> Result<MutTxn<E>, Error> {
        self.parent.length = self.length;
        self.parent
            .occupied_owned_pages
            .extend(self.occupied_owned_pages.drain().map(|(_, x)| x));
        self.parent
            .free_owned_pages
            .extend(self.free_owned_pages.iter());
        self.parent.free_pages.extend(self.free_pages.iter());
        self.parent.initial_free = std::mem::replace(&mut self.initial_free, Vec::new());
        self.parent.initial_allocated = std::mem::replace(&mut self.initial_allocated, Vec::new());

        // Trimmed pages re-allocated by this subtransaction's file
        // extensions: the subtransaction can't pop its parent's
        // `to_delete` when allocating, so apply the same accounting as
        // `MutTxn::alloc_page_` now that the parent's `length` has
        // caught up.
        while self
            .parent
            .to_delete
            .last()
            .map_or(false, |&p| p < self.parent.length)
        {
            let p = self.parent.to_delete.pop().unwrap();
            self.parent.initial_allocated.push(p);
        }

        if let Some(ref root_page) = self.child_page {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    root_page.as_ptr(),
                    self.parent.root_page_mut().as_mut_ptr(),
                    4064,
                );
            }
        }

        for (page, rc) in self.child_rc.drain() {
            // If the RC reaches 0, the page has already been freed in the relevant call to decr_rc.
            self.parent.set_rc(page, rc)?;
        }

        for (u, v) in self.roots.iter().enumerate() {
            if *v != 0 {
                self.parent.roots[u] = *v
            }
        }

        for (n, &r) in self.roots.iter().enumerate() {
            if r > 0 {
                if self.parent.roots.get(n).is_none() {
                    self.parent.roots.resize(n + 1, 0u64)
                }
                self.parent.roots[n] = r
            }
        }
        Ok(self.parent)
    }
}

impl<E: Borrow<Env> + Clone> MutTxn<E> {
    /// Begin a mutable sub-transaction.
    pub fn mut_txn_begin(self) -> SubTxn<E, Self> {
        SubTxn {
            length: self.length,
            child_rc: std::collections::HashMap::new(),
            child_page: None,
            occupied_owned_pages: std::collections::HashMap::new(),
            free_owned_pages: Vec::new(),
            free_pages: Vec::new(),
            initial_free: self.initial_free.clone(),
            initial_allocated: self.initial_allocated.clone(),
            roots: self.roots.clone(),
            parent: self,
            env: std::marker::PhantomData,
        }
    }
}

impl<E: Borrow<Env> + Clone, T> SubTxn<E, T> {
    /// Begin a mutable sub-transaction.
    pub fn mut_txn_begin(self) -> SubTxn<E, Self> {
        SubTxn {
            length: self.length,
            child_rc: std::collections::HashMap::new(),
            child_page: None,
            occupied_owned_pages: std::collections::HashMap::new(),
            free_owned_pages: Vec::new(),
            free_pages: Vec::new(),
            initial_free: self.initial_free.clone(),
            initial_allocated: self.initial_allocated.clone(),
            roots: self.roots.clone(),
            parent: self,
            env: std::marker::PhantomData,
        }
    }
}

impl Env {
    #[cfg(feature = "mmap")]
    fn mut_txn_lock(&self) -> Result<(), Error> {
        self.mut_txn_lock.lock();
        if let Some(ref f) = self.file {
            f.lock()?;
        }
        Ok(())
    }

    #[cfg(not(feature = "mmap"))]
    fn mut_txn_lock(&self) -> Result<(), Error> {
        self.mut_txn_lock.lock();
        Ok(())
    }

    #[cfg(feature = "mmap")]
    fn mut_txn_unlock(&self) -> Result<(), Error> {
        unsafe {
            self.mut_txn_lock.unlock();
        }
        if let Some(ref f) = self.file {
            f.unlock()?
        }
        Ok(())
    }

    #[cfg(not(feature = "mmap"))]
    fn mut_txn_unlock(&self) -> Result<(), Error> {
        unsafe {
            self.mut_txn_lock.unlock();
        }
        Ok(())
    }

    /// Start a mutable transaction. Mutable transactions that go out
    /// of scope are automatically aborted.
    pub fn mut_txn_begin<E: Borrow<Self>>(env: E) -> Result<MutTxn<E>, Error> {
        unsafe {
            let env_ = env.borrow();

            // First, take an exclusive file lock on the whole file to
            // make sure that no other process is starting a mutable
            // transaction at the same time. The worst that can happen
            // here is if the other process commits while we're still
            // waiting for a lock on the current page, because if that
            // happens, this new transaction will erase the
            // transaction in the other process.
            env_.mut_txn_lock()?;

            // Then, we can lock the root page of this transaction.
            let maps = env_.mmaps.lock()[0].ptr;
            let root = (&*(maps as *const GlobalHeader)).root as usize;
            debug!("BEGIN_TXN root = {:?}", root);
            env_.roots[root].rw.lock_exclusive();
            env_.lock_exclusive(root)?;
            // Root of the last MutTxn.
            let v0 = (root + env_.roots.len() - 1) % env_.roots.len();
            env_.check_crc(v0)?;
            // Copy the root page of the last transaction onto this
            // one.
            let page_ptr = maps.offset((v0 * PAGE_SIZE) as isize);
            let next_page_ptr = maps.offset((root * PAGE_SIZE) as isize);

            // Iff n_roots == 1 when creating the environment, the two pointers are equal.
            if page_ptr != next_page_ptr {
                std::ptr::copy_nonoverlapping(page_ptr.add(8), next_page_ptr.add(8), PAGE_SIZE - 8);
            }

            // Finally, read the header and start the transaction.
            let header = GlobalHeader::from_le(&*(next_page_ptr as *const GlobalHeader));
            debug!("n_roots = {:?}", header.n_roots);
            debug!("initial free_page {:x}", header.free_db);
            let mut txn = MutTxn {
                env,
                root,
                rc: if header.rc_db == 0 {
                    None
                } else {
                    Some(btree::Db::from_page(header.rc_db))
                },
                child_rc: std::collections::HashMap::new(),
                child_page: None,
                length: if header.length == 0 {
                    (PAGE_SIZE as u64) * (header.n_roots as u64)
                } else {
                    header.length
                },
                free: header.free_db,
                occupied_owned_pages: Vec::with_capacity(100),
                free_owned_pages: Vec::new(),
                free_pages: Vec::new(),
                initial_free: Vec::new(),
                initial_allocated: Vec::new(),
                roots: Vec::new(),
                to_delete: Vec::new(),
            };
            if txn.free > 0 {
                let free_db: btree::Db<L64, ()> = btree::Db::from_page(txn.free);
                let mut init = Vec::new();
                let mut length = txn.length;
                let mut to_delete = Vec::new();
                debug!("length {:x} {:?}", length, length);
                for p in btree::rev_iter(&txn, &free_db, None)? {
                    let (p, _) = p?;
                    let p = u64::from(*p);
                    if p == length - PAGE_SIZE as u64 {
                        if txn.free_for_all(p)? {
                            debug!("shrink {:x}", p);
                            length -= PAGE_SIZE as u64;
                            to_delete.push(p);
                            continue;
                        }
                    }

                    init.push(p);
                }
                txn.initial_free = init;
                txn.to_delete = to_delete;
                // for p in free_pages.drain(..) {
                //     debug!("delete free page {:x}", p);
                //     btree::del(&mut txn, &mut free_db, &L64(p.to_le()), None)?;
                // }
                debug!("init {:?}", txn.initial_free);
                debug!("free {:?}", txn.free_pages);
                // txn.free = free_db.db.into();
                txn.length = length;
            }

            Ok(txn)
        }
    }
}

/// # Safety
/// `p` must be a valid, fully-written mutable page owned by the current transaction.
#[cfg(feature = "crc32")]
unsafe fn clear_dirty(p: &mut MutPage) {
    p.clear_dirty(&HASHER)
}

/// # Safety
/// `p` must be a valid, fully-written mutable page owned by the current transaction.
#[cfg(not(feature = "crc32"))]
unsafe fn clear_dirty(p: &mut MutPage) {
    p.clear_dirty()
}

impl<E: Borrow<Env>> Commit for MutTxn<E> {
    type Parent = ();
    fn commit(mut self) -> Result<Self::Parent, Error> {
        debug!("COMMIT");

        // If there's no tree of free pages, and no pages to free,
        // don't bother with free pages at all (don't even allocate a
        // tree).
        let free_db =
            if self.free == 0 && self.free_owned_pages.is_empty() && self.free_pages.is_empty() {
                assert!(self.initial_free.is_empty());
                assert!(self.initial_allocated.is_empty());
                None
            } else {
                // Else, allocate or load the tree of free pages.
                self.free_owned_pages.sort_by(|a, b| b.cmp(a));
                debug!(
                    "free owned {:?} {:?}",
                    self.free_owned_pages.len(),
                    self.free_pages.len()
                );
                let mut free_db: btree::Db<L64, ()> = if self.free == 0 {
                    unsafe { btree::create_db(&mut self)? }
                } else {
                    unsafe { btree::Db::from_page(self.free) }
                };
                debug!("free_db = {:x}", free_db.db);
                if cfg!(debug_assertions) {
                    for p in self.initial_free.iter() {
                        trace!("initial_free {:x}", p);
                    }
                    for p in self.initial_allocated.iter() {
                        trace!("initial_alloc {:x}", p);
                    }
                }

                let mut changed = true;
                // Fix point on the freed and allocated pages.
                while changed {
                    changed = false;

                    // Pages resulting from the shrinking at the
                    // beginning of the transaction: they still have an
                    // entry in the tree of free pages, which must go.
                    // This runs inside the fix point (and `to_delete`
                    // stays live) because these `del`s — like any edit
                    // below — may themselves allocate by extending the
                    // file, re-acquiring trimmed pages: the allocator
                    // then pops them off the *back* of `to_delete`
                    // (they are the smallest, and extension is
                    // contiguous from `length`) into
                    // `initial_allocated`, while we consume the
                    // beyond-end-of-file ones from the *front*
                    // (largest first). See
                    // `tests::read_txn_after_shrink` and
                    // `tests::read_txn_after_shrink_regrow_cycles`.
                    let cut = self
                        .to_delete
                        .iter()
                        .take_while(|&&p| p >= self.length)
                        .count();
                    let dels: Vec<u64> = self.to_delete.drain(..cut).collect();
                    for p in dels {
                        btree::del(&mut self, &mut free_db, &L64(p.to_le()), None)?;
                        changed = true;
                    }
                    // Delete the pages allocated during this transaction
                    // from the free db. If these pages have been freed
                    // again, they will be reinserted below.
                    while let Some(p) = self.initial_allocated.pop() {
                        btree::del(&mut self, &mut free_db, &L64(p.to_le()), None)?;
                        changed = true;
                    }

                    // Adding all the pages freed during the transaction to the
                    // tree of free pages. If this call to `btree::put` frees
                    // pages, add them again. This converges in at most log n
                    // iterations (where n is the total number of free pages).
                    while !self.free_pages.is_empty() || !self.free_owned_pages.is_empty() {
                        while let Some(p) = self.free_pages.pop() {
                            let p = p & !0xfff;
                            btree::put(&mut self, &mut free_db, &L64(p.to_le()), &())?;
                            changed = true;
                        }
                        while let Some(p) = self.free_owned_pages.pop() {
                            let p = p & !0xfff;
                            btree::put(&mut self, &mut free_db, &L64(p.to_le()), &())?;
                            changed = true;
                        }
                    }
                }
                // Every trimmed page was either consumed above
                // (beyond the end of the file) or re-allocated by
                // extension (and popped into `initial_allocated`).
                debug_assert!(self.to_delete.is_empty());

                Some(free_db)
            };
        // Clear the dirty bit of all pages we've touched. If they've
        // been freed and have already been flushed by the kernel, we
        // don't want to resurrect them to the main memory, so we
        // check that.
        let mut occ = std::mem::replace(&mut self.occupied_owned_pages, Vec::new());
        for (p, uses_dirty) in occ.iter_mut() {
            if *uses_dirty {
                if let Some(ref free_db) = free_db {
                    if let Some((pp, ())) =
                        btree::get(&self, free_db, &L64(p.0.offset.to_le()), None)?
                    {
                        if u64::from_le(pp.0) == p.0.offset {
                            continue;
                        }
                    }
                }
                unsafe {
                    trace!(
                        "commit page {:x}: {:?}",
                        p.0.offset,
                        std::slice::from_raw_parts(p.0.data, 32)
                    );
                }
                unsafe {
                    clear_dirty(p);
                }
            }
        }

        let env = self.env.borrow();
        let mut maps = env.mmaps.lock();

        // Flush all the maps.
        for m in maps.iter_mut() {
            m.flush()?
        }

        // Get this transaction's root page.
        let globptr =
            unsafe { &mut *(maps[0].ptr.add(self.root * PAGE_SIZE) as *mut GlobalHeader) };
        // Set the length and free database.
        globptr.length = self.length.to_le();
        if let Some(free_db) = free_db {
            debug!("COMMIT: free_db = 0x{:x}", free_db.db);
            let free: u64 = free_db.db.into();
            globptr.free_db = free.to_le();
        }
        if let Some(ref rc_db) = self.rc {
            debug!("COMMIT: rc_db = 0x{:x}", rc_db.db);
            let rc: u64 = rc_db.db.into();
            globptr.rc_db = rc.to_le();
        }
        // Set the "root databases" modified by this transaction.
        let root_dbs = unsafe {
            std::slice::from_raw_parts_mut(
                maps[0].ptr.add(self.root * PAGE_SIZE + GLOBAL_HEADER_SIZE) as *mut u64,
                N_ROOTS,
            )
        };
        for (&r, rr) in self.roots.iter().zip(root_dbs.iter_mut()) {
            if r > 0 {
                *rr = r.to_le()
            }
        }

        // Set the root page's CRC.
        unsafe {
            set_crc(maps[0].ptr.add(self.root * PAGE_SIZE));
        }

        // Move the current global root page by one page on page 0.
        unsafe {
            (&mut *(maps[0].ptr as *mut GlobalHeader)).root =
                (self.root as u8 + 1) % (env.roots.len() as u8);
        }

        let max_len = (0..env.roots.len())
            .map(|root| {
                let hdr = unsafe { &mut *(maps[0].ptr.add(root * PAGE_SIZE) as *mut GlobalHeader) };
                hdr.length
            })
            .max()
            .unwrap_or(0);

        // Flush all the maps.
        maps[0].flush_range(0, env.roots.len() * PAGE_SIZE)?;

        let mut len = 0;
        for i in 0..maps.len() {
            len += maps[i].length;
            debug!("{:?} {:?}", len, max_len);
            if len >= max_len {
                if let Some(ref file) = self.env.borrow().file {
                    debug!("set len {:?}", len);
                    maps.truncate(i + 1);
                    file.set_len(len)?;
                }
                break;
            }
        }

        // And finally, unlock the root page in the environment.
        debug!("commit: unlock {:?}", self.root);
        debug!("max length {:?}", max_len);
        unsafe { env.roots[self.root].rw.unlock_exclusive() };
        // Unlock the root page on the file lock (if relevant).
        env.unlock(self.root)?;

        // And unlock the global mutable transaction mutex.
        env.mut_txn_unlock()?;
        debug!("/COMMIT");
        Ok(())
    }
}

impl<E: Borrow<Env>> MutTxn<E> {
    /// Setting the `num`th element of the initial page, treated as a
    /// [u64; 510].
    pub fn remove_root(&mut self, num: usize) {
        if self.roots.get(num).is_none() {
            self.roots.resize(num + 1, 0u64);
        }
        self.roots[num] = 0;
    }

    /// Add the page at offset `offset` to the list of free pages that
    /// were allocated by this `MutTxn` (and hence can be reallocated
    /// by the same transaction).
    fn free_owned_page(&mut self, offset: u64) {
        debug!("FREEING OWNED PAGE {:?} {:x}", offset, offset);
        assert_ne!(offset, 0);
        self.free_owned_pages.push(offset);
    }

    /// Add the page at offset `offset` to the list of free pages
    /// allocated by a previous transaction, and hence may still be
    /// accessible by other transactions.
    fn free_page(&mut self, offset: u64) {
        debug!("FREEING PAGE {:?} {:x}", offset, offset);
        assert_ne!(offset, 0);
        self.free_pages.push(offset)
    }

    /// Pop a free page from the B tree of free pages.
    fn free_pages_pop(&mut self) -> Result<Option<u64>, Error> {
        while let Some(p) = self.initial_free.pop() {
            if self.free_for_all(p)? {
                self.initial_allocated.push(p);
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    // Check whether this page is also free for the other
    // versions.
    fn free_for_all(&self, f: u64) -> Result<bool, Error> {
        let env = self.env.borrow();
        // We already know it's free for the youngest previous
        // transaction and for the current one (because the tree of
        // free pages was copied from there), so we only have
        // `self.roots.len() - 2` root pages to check.
        for i in 1..env.roots.len() - 1 {
            debug!("free_for_all {:?}", i);
            let db: btree::Db<L64, ()> = unsafe {
                let p = &*(env.mmaps.lock()[0]
                    .ptr
                    .add(((self.root + i) % env.roots.len()) * PAGE_SIZE)
                    as *const GlobalHeader);
                if f >= u64::from_le(p.length) {
                    // Page `f` was allocated strictyl after
                    // transaction `i`.
                    continue;
                }
                if p.free_db == 0 {
                    // This version doesn't have any free page.
                    return Ok(false);
                }
                btree::Db::from_page(u64::from_le(p.free_db))
            };
            if let Some((&f_, ())) = btree::get(self, &db, &L64(f.to_le()), None)? {
                if f_.as_u64() != f {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

impl<E: Borrow<Env>> MutTxn<E> {
    /// # Safety
    /// The caller must initialise the returned page before exposing it to safe code.
    unsafe fn alloc_page_(&mut self, dirty: bool) -> Result<MutPage, Error> {
        // If we have allocated and freed a page in this transaction,
        // use it first.
        if let Some(offset) = self.free_owned_pages.pop() {
            assert_ne!(offset, 0);
            debug!("free owned pop 0x{:x}", offset);
            let data = unsafe { self.env.borrow().find_offset(offset)? };
            let page = MutPage(CowPage { data, offset });
            self.occupied_owned_pages
                .push((MutPage(CowPage { data, offset }), dirty));
            Ok(page)
        } else {
            // Else, if there are free pages, take one.
            if let Some(offset) = self.free_pages_pop()? {
                assert_ne!(offset, 0);
                debug!("free pages pop 0x{:x}", offset);
                let data = unsafe { self.env.borrow().find_offset(offset)? };
                self.occupied_owned_pages
                    .push((MutPage(CowPage { data, offset }), dirty));
                Ok(MutPage(CowPage { data, offset }))
            } else {
                // Else, allocate in the free space.
                debug!("allocate in the free space 0x{:x}", self.length);
                let offset = self.length;
                self.length += PAGE_SIZE as u64;
                // If this offset was trimmed off the end of the file at
                // the beginning of the transaction, it is now being
                // re-allocated: take it out of `to_delete`. Since
                // `to_delete` is sorted in decreasing order (it was
                // filled by a reverse iteration over the contiguous
                // trailing run of free pages), the only candidate is
                // its last element. The page also joins
                // `initial_allocated`: it still has a (stale) entry in
                // the tree of free pages, which the commit removes for
                // all of `initial_allocated` — this also covers pages
                // re-allocated by the commit's own edits to that tree.
                if self.to_delete.last() == Some(&offset) {
                    self.to_delete.pop();
                    self.initial_allocated.push(offset);
                }
                let data = unsafe { self.env.borrow().find_offset(offset)? };
                self.occupied_owned_pages
                    .push((MutPage(CowPage { data, offset }), dirty));
                Ok(MutPage(CowPage { data, offset }))
            }
        }
    }
}

impl<E: Borrow<Env>, T: Borrow<MutTxn<E>>> SubTxn<E, T> {
    fn free_pages_pop(&mut self) -> Result<Option<u64>, Error> {
        while let Some(p) = self.initial_free.pop() {
            if self.parent.borrow().free_for_all(p)? {
                self.initial_allocated.push(p);
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// # Safety
    /// The caller must initialise the returned page before exposing it to safe code.
    unsafe fn alloc_page_(&mut self, dirty: bool) -> Result<MutPage, Error> {
        // If we have allocated and freed a page in this transaction,
        // use it first.
        if let Some(offset) = self.free_owned_pages.pop() {
            assert_ne!(offset, 0);
            debug!("free owned pop 0x{:x}", offset);
            let data = unsafe { self.parent.borrow().env.borrow().find_offset(offset)? };
            let page = MutPage(CowPage { data, offset });
            self.occupied_owned_pages
                .insert(offset, (MutPage(CowPage { data, offset }), dirty));
            Ok(page)
        } else {
            // Else, if there are free pages, take one.
            if let Some(offset) = self.free_pages_pop()? {
                assert_ne!(offset, 0);
                debug!("free pages pop 0x{:x}", offset);
                let data = unsafe { self.parent.borrow().env.borrow().find_offset(offset)? };
                self.occupied_owned_pages
                    .insert(offset, (MutPage(CowPage { data, offset }), dirty));
                Ok(MutPage(CowPage { data, offset }))
            } else {
                // Else, allocate in the free space.
                debug!("allocate in the free space 0x{:x}", self.length);
                let offset = self.length;
                self.length += PAGE_SIZE as u64;
                let data = unsafe { self.parent.borrow().env.borrow().find_offset(offset)? };
                self.occupied_owned_pages
                    .insert(offset, (MutPage(CowPage { data, offset }), dirty));
                Ok(MutPage(CowPage { data, offset }))
            }
        }
    }
}

impl<E: Borrow<Env>, T: Borrow<MutTxn<E>>> sanakirja_core::AllocPage for SubTxn<E, T>
where
    Error: From<<MutTxn<E> as sanakirja_core::LoadPage>::Error>,
{
    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_page`].
    unsafe fn alloc_page(&mut self) -> Result<MutPage, Error> {
        self.alloc_page_(true)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_page_no_dirty`].
    unsafe fn alloc_page_no_dirty(&mut self) -> Result<MutPage, Error> {
        self.alloc_page_(false)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_contiguous`].
    unsafe fn alloc_contiguous(&mut self, _length: u64) -> Result<MutPage, Error> {
        unimplemented!()
    }

    /// Increment the reference count for page `off`.
    fn incr_rc(&mut self, off: u64) -> Result<usize, Error> {
        assert!(off > 0);
        // If this is a child transaction, don't update the table.
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut e) = self.child_rc.entry(off) {
            // If the page has already been changed, update it in
            // `child_rc`
            *e.get_mut() += 1;
            Ok(*e.get())
        } else {
            // Else, take its value from the parent and update the
            // hashmap.
            use sanakirja_core::LoadPage;
            let rc = self.parent.borrow().rc(off)? as usize;
            self.child_rc.insert(off, rc + 1);
            Ok(rc + 1)
        }
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::decr_rc`].
    unsafe fn decr_rc(&mut self, off: u64) -> Result<usize, Error> {
        let rc = self.decr_rc_(off)?;
        if rc == 0 {}
        Ok(rc)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::decr_rc_owned`].
    unsafe fn decr_rc_owned(&mut self, off: u64) -> Result<usize, Error> {
        let rc = self.decr_rc_(off)?;
        if rc == 0 {
            debug!("FREEING OWNED PAGE {:?} {:x}", off, off);
            assert_ne!(off, 0);
            self.free_owned_pages.push(off);
        }
        Ok(rc)
    }

    fn is_dirty(&self, off: &sanakirja_core::CowPage) -> bool {
        self.occupied_owned_pages.contains_key(&off.offset)
    }
}

impl<E: Borrow<Env>> sanakirja_core::AllocPage for MutTxn<E> {
    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_page`].
    unsafe fn alloc_page(&mut self) -> Result<MutPage, Error> {
        self.alloc_page_(true)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_page_no_dirty`].
    unsafe fn alloc_page_no_dirty(&mut self) -> Result<MutPage, Error> {
        self.alloc_page_(false)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::alloc_contiguous`].
    unsafe fn alloc_contiguous(&mut self, length: u64) -> Result<MutPage, Error> {
        // Check that length is a multiple of the page size.
        assert_eq!(length & (PAGE_SIZE as u64 - 1), 0);
        self.free_owned_pages.sort_by(|a, b| b.cmp(a));
        self.initial_free.sort_by(|a, b| b.cmp(a));
        let mut i = self.free_owned_pages.len();
        let mut ni = 0;
        let mut j = self.initial_free.len();
        let mut nj = 0;

        let mut result = 0u64;
        let mut current = 0u64;
        let mut current_p = std::ptr::null_mut();
        while current + PAGE_SIZE as u64 - result < length {
            // page allocated, consumed in i, consumed in j
            let (m, ic, jc) = if i > 0 && j > 0 {
                let a = self.free_owned_pages[i - 1];
                let b = self.initial_free[j - 1];
                if a < b || !self.free_for_all(b)? {
                    i -= 1;
                    (a, 1, 0)
                } else {
                    j -= 1;
                    (b, 0, 1)
                }
            } else if i > 0 {
                i -= 1;
                (self.free_owned_pages[i], 1, 0)
            } else if j > 0 {
                j -= 1;
                let p = self.initial_free[j];
                // Check whether p is available for all txns
                if !self.free_for_all(p)? {
                    // Reset the current block, no free page was consumed.
                    ni = 0;
                    nj = 0;
                    current = result;
                    current_p = unsafe { self.env.borrow().find_offset(current)? };
                    continue;
                }
                (p, 0, 1)
            } else if current == result {
                // No current region, and we've reached the end of the file, just allocate there.
                let offset = self.length;
                let data = unsafe { self.env.borrow().find_offset(offset)? };
                self.length += length;
                return Ok(MutPage(CowPage { offset, data }));
            } else if current + PAGE_SIZE as u64 == self.length {
                // We've reached the end of the file, grow just one last time.
                self.length += length - (current + PAGE_SIZE as u64 - result);
                break;
            } else {
                unreachable!()
            };
            if current > 0 && m == current + PAGE_SIZE as u64 {
                // We only have to check whether `current` is actually
                // contiguous in terms of pointers.
                let next_p = unsafe { self.env.borrow().find_offset(m)? };
                if next_p as usize == current_p as usize + PAGE_SIZE {
                    ni += ic;
                    nj += jc;
                } else {
                    // `m` is the first page in a new map, reset the block
                    result = m;
                    ni = ic;
                    nj = jc;
                }
                current = m;
                current_p = next_p
            } else {
                // Initial region
                result = m;
                current = m;
                current_p = unsafe { self.env.borrow().find_offset(m)? };
                ni = ic;
                nj = jc;
            }
        }
        for offset in self
            .free_owned_pages
            .drain(i..i + ni)
            .chain(self.initial_free.drain(j..j + nj))
        {
            let data = unsafe { self.env.borrow().find_offset(offset)? };
            self.occupied_owned_pages
                .push((MutPage(CowPage { data, offset }), false))
        }

        let data = unsafe { self.env.borrow().find_offset(result)? };
        Ok(MutPage(CowPage {
            data,
            offset: result,
        }))
    }

    /// Increment the reference count for page `off`.
    fn incr_rc(&mut self, off: u64) -> Result<usize, Error> {
        assert!(off > 0);

        // Else, update the table.
        if let Some(mut rc_) = self.rc.take() {
            let mut curs = btree::cursor::Cursor::new(self, &rc_)?;
            curs.set(self, &L64(off.to_le()), None)?;
            let rc = if let Some((rc, _)) = curs.current(self)? {
                let rc = rc.as_u64();
                if rc & !0xfff == off {
                    rc & 0xfff
                } else {
                    1
                }
            } else {
                1
            };
            if rc > 1 {
                btree::del::del_at_cursor(self, &mut rc_, &mut curs, true)?;
            }
            debug!("incr rc 0x{:x} {:?}", off, rc + 1);
            assert!(rc + 1 <= 0xfff);
            btree::put(self, &mut rc_, &L64((off | (rc + 1)).to_le()), &())?;
            self.rc = Some(rc_);
            Ok(rc as usize + 1)
        } else {
            let mut rc = unsafe { btree::create_db(self)? };
            btree::put(self, &mut rc, &L64((off | 2).to_le()), &())?;
            self.rc = Some(rc);
            Ok(2)
        }
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::decr_rc`].
    unsafe fn decr_rc(&mut self, off: u64) -> Result<usize, Error> {
        let rc = self.decr_rc_(off)?;
        if rc == 0 {
            self.free_page(off);
        }
        Ok(rc)
    }

    /// # Safety
    /// See [`sanakirja_core::AllocPage::decr_rc_owned`].
    unsafe fn decr_rc_owned(&mut self, off: u64) -> Result<usize, Error> {
        let rc = self.decr_rc_(off)?;
        if rc == 0 {
            self.free_owned_page(off);
        }
        Ok(rc)
    }
}

impl<E: Borrow<Env>, T: Borrow<MutTxn<E>>> SubTxn<E, T> {
    /// Decrement the reference count of page `off`, freeing that page
    /// if the RC reaches 0 after decrementing it.
    fn decr_rc_(&mut self, off: u64) -> Result<usize, Error> {
        // If the child is already known, decrement it.
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut e) = self.child_rc.entry(off) {
            let rc = *e.get();
            assert!(rc > 1);
            // Here, even if the RC ends up equal to 0 or 1, we
            // still have to keep it in the store to update the
            // parent txn.
            *e.get_mut() -= 1;
            Ok(*e.get())
        } else if let Some(ref rc_) = self.parent.borrow().rc {
            let mut curs = btree::cursor::Cursor::new(self, rc_)?;
            curs.set(self, &L64(off.to_le()), None)?;
            if let Some((rc, _)) = curs.current(self)? {
                let rc = rc.as_u64();
                if rc & !0xfff == off {
                    let rc = (rc & 0xfff) as usize;
                    if rc > 2 {
                        self.child_rc.insert(off, rc - 1);
                    }
                    Ok(rc - 1)
                } else {
                    Ok(0)
                }
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }
}

impl<E: Borrow<Env>> MutTxn<E> {
    /// Decrement the reference count of page `off`, freeing that page
    /// if the RC reaches 0 after decrementing it.
    fn decr_rc_(&mut self, off: u64) -> Result<usize, Error> {
        debug!("decr_rc 0x{:x} {:?}", off, self.rc);
        // If there's no RC table, free the page. Also, in order to
        // avoid infinite recursion (since `del` and `put` below might
        // free pages), we `take` the reference counter table.
        if let Some(mut rc_) = self.rc.take() {
            let mut curs = btree::cursor::Cursor::new(self, &rc_)?;
            curs.set(self, &L64(off.to_le()), None)?;
            // The reference count is stored as the 12 LSBs of the
            // keys. If the page isn't in the RC table, the count is
            // 1.
            let rc = if let Some((rc, ())) = curs.next(self)? {
                let rc = rc.as_u64();
                if rc & !0xfff == off {
                    rc
                } else {
                    1
                }
            } else {
                1
            };

            debug!("decr_rc, rc = 0x{:x}", rc);
            if rc > 1 {
                // If the reference count is strictly more than 2,
                // replace the reference count with a decremented
                // value.
                btree::del(self, &mut rc_, &L64(rc.to_le()), None)?;
                if rc & 0xfff > 2 {
                    btree::put(self, &mut rc_, &L64((rc - 1).to_le()), &())?;
                    self.rc = Some(rc_);
                } else {
                    // Else, we don't free the page, but don't add the
                    // page back, since this is an implicit value of
                    // "1" for the reference count.
                    self.rc = Some(rc_)
                }
                return Ok((rc & 0xfff) as usize - 1);
            } else {
                self.rc = Some(rc_)
            }
        }
        Ok(0)
    }
}

impl<E: Borrow<Env>> sanakirja_core::LoadPage for MutTxn<E> {
    type Error = Error;
    /// # Safety
    /// See [`sanakirja_core::LoadPage::load_page`].
    unsafe fn load_page(&self, off: u64) -> Result<CowPage, Self::Error> {
        let data = self.env.borrow().find_offset(off)?;
        Ok(CowPage { data, offset: off })
    }

    fn rc(&self, page: u64) -> Result<u64, Self::Error> {
        if let Some(rc) = self.child_rc.get(&page) {
            return Ok(*rc as u64);
        }
        if let Some(ref rc) = self.rc {
            if let Some((rc, _)) = btree::get(self, rc, &L64(page.to_le()), None)? {
                let rc = rc.as_u64();
                if rc & !0xfff == page {
                    let r = rc & 0xfff;
                    if r >= 2 {
                        return Ok(r);
                    }
                }
            }
        }
        Ok(0)
    }
}

impl<E: Borrow<Env>, T: Borrow<MutTxn<E>>> sanakirja_core::LoadPage for SubTxn<E, T> {
    type Error = Error;
    /// # Safety
    /// See [`sanakirja_core::LoadPage::load_page`].
    unsafe fn load_page(&self, off: u64) -> Result<CowPage, Self::Error> {
        let data = self.parent.borrow().env.borrow().find_offset(off)?;
        Ok(CowPage { data, offset: off })
    }

    fn rc(&self, page: u64) -> Result<u64, Self::Error> {
        if let Some(rc) = self.child_rc.get(&page) {
            return Ok(*rc as u64);
        }
        if let Some(ref rc) = self.parent.borrow().rc {
            if let Some((rc, _)) = btree::get(self, rc, &L64(page.to_le()), None)? {
                let rc = rc.as_u64();
                if rc & !0xfff == page {
                    let r = rc & 0xfff;
                    if r >= 2 {
                        return Ok(r);
                    }
                }
            }
        }
        Ok(0)
    }
}

impl<E: Borrow<Env>> RootPage for MutTxn<E> {
    /// # Safety
    /// See [`RootPage::root_page`].
    unsafe fn root_page(&self) -> &[u8; 4064] {
        if let Some(ref p) = self.child_page {
            return p;
        }
        let env = self.env.borrow();
        let maps = env.mmaps.lock();
        let ptr = maps[0].ptr.add(self.root * PAGE_SIZE + GLOBAL_HEADER_SIZE);
        &*(ptr as *const [u8; 4064])
    }
}

impl<E: Borrow<Env>> RootPageMut for MutTxn<E> {
    /// # Safety
    /// See [`RootPageMut::root_page_mut`].
    unsafe fn root_page_mut(&mut self) -> &mut [u8; 4064] {
        if let Some(ref mut p) = self.child_page {
            return p.as_mut();
        }
        let env = self.env.borrow();
        let maps = env.mmaps.lock();
        let ptr = maps[0].ptr.add(self.root * PAGE_SIZE + GLOBAL_HEADER_SIZE);
        &mut *(ptr as *mut [u8; 4064])
    }

    fn set_root(&mut self, num: usize, value: u64) {
        if self.roots.get(num).is_none() {
            self.roots.resize(num + 1, 0u64);
        }
        self.roots[num] = value;
    }
}

impl<E: Borrow<Env>> MutTxn<E> {
    /// Low-level method to get the root page number `n`, if that page
    /// isn't a B tree (use the [`RootDb`] trait else).
    pub fn root(&self, n: usize) -> Option<u64> {
        if let Some(db) = self.roots.get(n) {
            if *db == 0 {
                None
            } else {
                Some(*db)
            }
        } else {
            unsafe {
                let env = self.env.borrow();
                let db = {
                    let maps = env.mmaps.lock();
                    u64::from_le(
                        *(maps[0]
                            .ptr
                            .add(self.root * PAGE_SIZE + GLOBAL_HEADER_SIZE + 8 * n)
                            as *mut u64),
                    )
                };
                if db != 0 {
                    Some(db)
                } else {
                    None
                }
            }
        }
    }
}

impl<E: Borrow<Env>> RootDb for MutTxn<E> {
    // Just call method `root` and convert the result to a `Db`.
    fn root_db<K: Storable + ?Sized, V: Storable + ?Sized, P: btree::BTreePage<K, V>>(
        &self,
        n: usize,
    ) -> Option<btree::Db_<K, V, P>> {
        if let Some(db) = self.root(n) {
            Some(unsafe { btree::Db_::from_page(db) })
        } else {
            None
        }
    }
}

impl<E: Borrow<Env>, T> SubTxn<E, T> {
    /// Low-level method to get the root page number `n`, if that page
    /// isn't a B tree (use the [`RootDb`] trait else).
    pub fn root(&self, n: usize) -> Option<u64> {
        if let Some(db) = self.roots.get(n) {
            if *db == 0 {
                None
            } else {
                Some(*db)
            }
        } else {
            unimplemented!()
        }
    }
}

impl<E: Borrow<Env>, T> RootDb for SubTxn<E, T> {
    // Just call method `root` and convert the result to a `Db`.
    fn root_db<K: Storable + ?Sized, V: Storable + ?Sized, P: btree::BTreePage<K, V>>(
        &self,
        n: usize,
    ) -> Option<btree::Db_<K, V, P>> {
        if let Some(db) = self.root(n) {
            Some(unsafe { btree::Db_::from_page(db) })
        } else {
            None
        }
    }
}
