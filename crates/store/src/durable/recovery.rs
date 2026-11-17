//! Hash-chain publication recovery and selected-head persistence.

use super::{
    FileStore, HEAD_BYTES, HEAD_MAGIC, Hash, JOURNAL_MAGIC, JOURNAL_RECORD_BYTES, JOURNAL_VERSION,
    PREPARED_TAG, PUBLISHED_TAG, Path, PublicationBase, PublicationDescriptor, SelectedHead,
    StoreError, io_error, read_byte, read_hash, read_u64, sync_directory,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom},
};

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static TEST_FAULT: Mutex<Vec<(std::thread::ThreadId, u8)>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn set_test_fault(point: u8) {
    if let Ok(mut fault) = TEST_FAULT.lock() {
        let owner = std::thread::current().id();
        fault.retain(|(thread, _)| *thread != owner);
        fault.push((owner, point));
    }
}

#[cfg(test)]
pub(super) fn take_test_fault(point: u8) -> bool {
    let Ok(mut fault) = TEST_FAULT.lock() else {
        return false;
    };
    let current = std::thread::current().id();
    let Some(index) = fault
        .iter()
        .position(|(owner, pending)| *owner == current && *pending == point)
    else {
        return false;
    };
    fault.remove(index);
    true
}

/// The fixed-memory journal state retained by an open [`FileStore`].
///
/// The selected head is the only publication history needed by the store's
/// linearization checks. The checksum and byte offset let a subsequent append
/// validate and continue the chain without rereading prior frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct JournalTail {
    pub(super) sequence: u64,
    pub(super) checksum: Hash,
    pub(super) offset: u64,
    pub(super) selected: Option<SelectedHead>,
}

#[path = "recovery_head.rs"]
mod head;
#[path = "recovery_journal.rs"]
mod journal;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalRecord {
    pub(super) tag: u8,
    pub(super) sequence: u64,
    pub(super) paired_sequence: u64,
    pub(super) previous: Hash,
    pub(super) payload: PublicationDescriptor,
    pub(super) checksum: Hash,
}

/// Fixed-size receipt that checkpoints the validated journal tail alongside
/// the selected publication. A valid receipt makes warm reopen check only the
/// tail frame; a missing or malformed receipt explicitly falls back to a
/// streaming journal scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalCheckpoint {
    pub(super) head: SelectedHead,
    pub(super) tail: JournalTail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalState {
    pub(super) selected: Option<SelectedHead>,
}

/// Selected publication checked under a live owner lease, store process lock,
/// GC pin, and bounded journal replay. Its fields cannot be caller-forged.
pub struct RetirementSelectedHead<'a> {
    store: &'a FileStore,
    _authority: &'a super::StorePublicationAuthority,
    _pin: &'a super::RetirementGcPin,
    process_lock: File,
    selected: SelectedHead,
}

impl RetirementSelectedHead<'_> {
    /// The exact store paired with this capability.
    pub fn store(&self) -> &FileStore { self.store }
    /// The GC pin retained for this same store.
    pub fn pin(&self) -> &super::GcPinGuard { self._pin.pin() }
    /// Descriptor admitted from the selected HEAD and journal.
    pub fn selected(&self) -> SelectedHead { self.selected }
}

impl Drop for RetirementSelectedHead<'_> {
    fn drop(&mut self) { let _ = self.process_lock.unlock(); }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveredHead {
    Empty,
    Current(SelectedHead),
    RepairMissing(SelectedHead),
    RepairInterrupted {
        selected: SelectedHead,
        orphaned_temps: usize,
    },
}

pub(super) fn base_matches(
    current: Option<&PublicationDescriptor>,
    expected: Option<PublicationBase>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(current), Some(expected)) => {
            expected.target == current.target && expected.generation == current.target_generation
        }
    }
}

pub(super) fn descriptor_matches_base(
    current: Option<&PublicationDescriptor>,
    expected: &PublicationDescriptor,
) -> bool {
    match current {
        None => expected.base.is_none() && expected.base_generation == 0,
        Some(current) => {
            expected.base == Some(current.target)
                && current.target_generation == expected.base_generation
        }
    }
}

fn history_file_identity(file: &File, path: &Path) -> Result<backend_platform::FileIdentity, StoreError> {
    let held = backend_platform::FileIdentity::of_file(file).map_err(|error| io_error(&error))?;
    let named = backend_platform::FileIdentity::of_path_nofollow(path).map_err(|error| io_error(&error))?;
    if held != named { return Err(StoreError::WrongBase); }
    Ok(held)
}

/// Bounded owner-held selected-publication history lookup refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceHistoryError {
    /// Caller cancellation was observed.
    Cancelled,
    /// The explicit batch or journal-work budget was exceeded.
    Limit,
    /// A requested generation is not a selected PUBLISHED record.
    Missing,
    /// The selected head changed, the journal was malformed, or IO failed.
    Store(StoreError),
}
impl std::fmt::Display for WorkspaceHistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "workspace history: {self:?}") }
}
impl std::error::Error for WorkspaceHistoryError {}

impl FileStore {
    /// Admit the current selected head with fixed-frame I/O under the live
    /// publication authority and process lock. No caller-selected head is
    /// trusted. The returned capability retains the lock, authority borrow,
    /// GC pin and its shared-ledger reservation through the consumer's work.
    #[cfg(unix)]
    pub fn retirement_selected_head<'a>(
        &'a self,
        authority: &'a super::StorePublicationAuthority,
        pin: &'a super::RetirementGcPin,
        budget: &backend_version::retirement_budget::RetirementBudget,
        bookkeeping: usize,
        max_frames: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<backend_version::retirement_budget::Metered<RetirementSelectedHead<'a>>, StoreError> {
        use std::io::Read;
        if max_frames == 0 || max_frames > 2_000_000 || !pin.belongs_to(budget)
            || !pin.pin().covers_identity(self.gc_identity) { return Err(StoreError::Bounds); }
        let scratch_bytes = 4096usize.checked_add(2 * (HEAD_BYTES + 1))
            .and_then(|n| n.checked_add(2 * JOURNAL_RECORD_BYTES))
            .and_then(|n| n.checked_add(3 * std::mem::size_of::<JournalRecord>()))
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<blake3::Hasher>()))
            .and_then(|n| n.checked_add(3 * std::mem::size_of::<JournalCheckpoint>()))
            .and_then(|n| n.checked_add(3 * std::mem::size_of::<SelectedHead>()))
            .and_then(|n| n.checked_add(4 * std::mem::size_of::<backend_platform::FileIdentity>()))
            .and_then(|n| n.checked_add(6 * std::mem::size_of::<File>()))
            .and_then(|n| n.checked_add(4 * std::mem::size_of::<std::fs::Metadata>()))
            .and_then(|n| n.checked_add(bookkeeping))
            .ok_or(StoreError::Bounds)?;
        let _scratch = budget.reserve(scratch_bytes).map_err(|_| StoreError::Bounds)?;
        if !authority.permits_borrowed(&self.root) { return Err(StoreError::Bounds); }
        let retained = std::mem::size_of::<RetirementSelectedHead<'a>>().checked_add(bookkeeping).ok_or(StoreError::Bounds)?;
        let owner = budget.reserve(retained).map_err(|_| StoreError::Bounds)?;
        let per_frame = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?
            .checked_mul(2).and_then(|n| n.checked_add(8)).ok_or(StoreError::Bounds)?;
        let mut work = max_frames.checked_mul(per_frame)
            .and_then(|n| n.checked_add(2 * (HEAD_BYTES as u64 + 1)))
            .and_then(|n| n.checked_add(4096)).ok_or(StoreError::Bounds)?;
        let mut checkpoint = || {
            if cancelled() { return Err(StoreError::Bounds); }
            work = work.checked_sub(1).ok_or(StoreError::Bounds)?;
            Ok(())
        };
        let root = super::artifact_fs::BoundedReadDirectories::open_control_root(self, &mut checkpoint)?;
        let lock = super::artifact_fs::BoundedReadDirectories::open_control_file(&root, rustix::cstr!("LOCK"), true, &mut checkpoint)?;
        let mut locked = false;
        for _ in 0..1024 {
            checkpoint()?;
            match lock.try_lock() {
                Ok(()) => { locked = true; break; }
                Err(std::fs::TryLockError::WouldBlock) => std::thread::yield_now(),
                Err(std::fs::TryLockError::Error(error)) => return Err(io_error(&error)),
            }
        }
        if !locked { return Err(StoreError::Bounds); }
        let lock_identity = backend_platform::FileIdentity::of_file(&lock).map_err(|e| io_error(&e))?;
        fn read_checked(file: &mut File, output: &mut [u8], checkpoint: &mut dyn FnMut() -> Result<(), StoreError>) -> Result<(), StoreError> {
            let mut at = 0usize;
            while at < output.len() {
                checkpoint()?;
                match file.read(&mut output[at..]) {
                    Ok(0) => return Err(StoreError::Corrupt),
                    Ok(n) => at = at.checked_add(n).ok_or(StoreError::Bounds)?,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(io_error(&error)),
                }
            }
            Ok(())
        }
        fn read_head(root: &File, checkpoint: &mut dyn FnMut() -> Result<(), StoreError>) -> Result<(JournalCheckpoint, backend_platform::FileIdentity), StoreError> {
            let mut file = super::artifact_fs::BoundedReadDirectories::open_control_file(root, rustix::cstr!("HEAD"), false, checkpoint)?;
            let identity = backend_platform::FileIdentity::of_file(&file).map_err(|e| io_error(&e))?;
            if usize::try_from(file.metadata().map_err(|e| io_error(&e))?.len()).map_err(|_| StoreError::Bounds)? != HEAD_BYTES { return Err(StoreError::Corrupt); }
            let mut bytes = [0u8; HEAD_BYTES + 1];
            read_checked(&mut file, &mut bytes[..HEAD_BYTES], checkpoint)?;
            loop {
                checkpoint()?;
                match file.read(&mut bytes[HEAD_BYTES..]) {
                    Ok(0) => break,
                    Ok(_) => return Err(StoreError::Corrupt),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(io_error(&error)),
                }
            }
            if !bytes[..HEAD_BYTES].starts_with(HEAD_MAGIC) { return Err(StoreError::Corrupt); }
            checkpoint()?;
            Ok((head::decode_head(&bytes[..HEAD_BYTES])?, identity))
        }
        let (first, head_identity) = read_head(&root, &mut checkpoint)?;
        let mut journal_file = super::artifact_fs::BoundedReadDirectories::open_control_file(&root, rustix::cstr!("journal"), false, &mut checkpoint)?;
        let journal_identity = backend_platform::FileIdentity::of_file(&journal_file).map_err(|e| io_error(&e))?;
        let length = journal_file.metadata().map_err(|e| io_error(&e))?.len();
        let frame_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
        if length % frame_bytes != 0 { return Err(StoreError::Corrupt); }
        let frames = length / frame_bytes;
        if frames > max_frames { return Err(StoreError::Bounds); }
        if first.head.journal_sequence == 0 || first.head.journal_sequence > first.tail.sequence
            || first.tail.sequence > frames { return Err(StoreError::Corrupt); }
        let mut prior = [0u8; 32];
        let mut selected: Option<SelectedHead> = None;
        let mut previous_frame: Option<JournalRecord> = None;
        let mut frame_buf = [0u8; JOURNAL_RECORD_BYTES];
        for sequence in 1..=frames {
            checkpoint()?;
            read_checked(&mut journal_file, &mut frame_buf, &mut checkpoint)?;
            let frame = journal::decode_record(&frame_buf)?;
            if frame.sequence != sequence || frame.previous != prior { return Err(StoreError::Corrupt); }
            if sequence == first.tail.sequence && (frame.checksum != first.tail.checksum || first.tail.offset != sequence.checked_mul(frame_bytes).ok_or(StoreError::Bounds)?) {
                return Err(StoreError::Corrupt);
            }
            if frame.tag == PUBLISHED_TAG {
                // This narrow path admits adjacent prepare/publish pairs so
                // the prepared record is exactly the chain-checked one, not
                // a later reread of a mutable journal inode.
                if frame.paired_sequence.checked_add(1) != Some(sequence) { return Err(StoreError::Bounds); }
                let prepared = previous_frame.ok_or(StoreError::Corrupt)?;
                if prepared.tag != PREPARED_TAG || prepared.sequence != frame.paired_sequence
                    || prepared.paired_sequence != 0 || prepared.payload != frame.payload { return Err(StoreError::Corrupt); }
                let generation = selected.map_or(0, |head| head.generation());
                if frame.payload.base_generation != generation
                    || frame.payload.target_generation != generation.checked_add(1).ok_or(StoreError::Bounds)?
                    || frame.payload.base != selected.map(|head| head.descriptor.target) { return Err(StoreError::Corrupt); }
                selected = Some(SelectedHead { journal_sequence: frame.sequence, descriptor: frame.payload });
            } else if frame.tag != PREPARED_TAG {
                return Err(StoreError::Corrupt);
            }
            prior = frame.checksum;
            previous_frame = Some(frame);
        }
        if selected != Some(first.head) || journal_file.metadata().map_err(|e| io_error(&e))?.len() != length { return Err(StoreError::Corrupt); }
        let binding = first.head.descriptor.workspace().ok_or(StoreError::Corrupt)?;
        if binding.root() != &first.head.descriptor.target || binding.closure() != first.head.descriptor.closure {
            return Err(StoreError::Corrupt);
        }
        let named_journal = super::artifact_fs::BoundedReadDirectories::open_control_file(&root, rustix::cstr!("journal"), false, &mut checkpoint)?;
        if backend_platform::FileIdentity::of_file(&named_journal).map_err(|e| io_error(&e))? != journal_identity { return Err(StoreError::WrongBase); }
        let (last, last_head_identity) = read_head(&root, &mut checkpoint)?;
        if last != first || last_head_identity != head_identity { return Err(StoreError::WrongBase); }
        if !authority.permits_borrowed(&self.root) { return Err(StoreError::WrongBase); }
        let named_lock = super::artifact_fs::BoundedReadDirectories::open_control_file(&root, rustix::cstr!("LOCK"), true, &mut checkpoint)?;
        if backend_platform::FileIdentity::of_file(&named_lock).map_err(|e| io_error(&e))? != lock_identity { return Err(StoreError::WrongBase); }
        checkpoint()?;
        Ok(owner.meter(RetirementSelectedHead { store: self, _authority: authority, _pin: pin, process_lock: lock, selected: first.head }))
    }

    /// Resolves a workspace publication in the authenticated history of the
    /// exact currently selected head. The caller supplies a journal byte
    /// budget; a longer journal is refused rather than scanned under the
    /// publication lock without a bound.
    ///
    /// # Errors
    /// Returns `Bounds` when the journal exceeds `max_journal_bytes`, and
    /// `Corrupt` when the current head or requested ancestor is not selected
    /// by the verified workspace history.
    pub fn workspace_selected_ancestor(
        &self,
        current_root: Hash,
        current_generation: u64,
        ancestor_root: Hash,
        ancestor_generation: u64,
        max_journal_bytes: u64,
    ) -> Result<SelectedHead, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        let journal_path = self.root.join("journal");
        if journal_length(&journal_path)? > max_journal_bytes {
            return Err(StoreError::Bounds);
        }
        let current = self.read_state()?.selected.ok_or(StoreError::Corrupt)?;
        let descriptor = current.descriptor();
        let binding = descriptor.workspace().ok_or(StoreError::Corrupt)?;
        if binding.root() != &descriptor.target()
            || binding.closure() != descriptor.closure()
            || descriptor.target() != current_root
            || descriptor.target_generation() != current_generation
            || ancestor_generation == 0
            || ancestor_generation > current_generation
        {
            return Err(StoreError::Corrupt);
        }
        find_workspace_publication(&journal_path, ancestor_root, ancestor_generation)?
            .ok_or(StoreError::Corrupt)
    }

    /// Reopen a sorted batch of previously PUBLISHED workspace generations in
    /// one bounded hash-chain scan, anchored to the exact current selected
    /// head. A PREPARED-only frame is never returned. The caller must hold the
    /// owner lease and separately admit each historical closure/transaction.
    ///
    /// # Errors
    /// Refuses cancellation, a missing generation, stale head, malformed
    /// journal, or a batch/work budget exceeding the explicit hard limits.
    pub fn selected_workspace_history(
        &self, expected: SelectedHead, generations: &[u64], max_frames: u64,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<SelectedHead>, WorkspaceHistoryError> {
        use std::sync::atomic::Ordering;
        let check = || if cancellation.load(Ordering::Acquire) { Err(WorkspaceHistoryError::Cancelled) } else { Ok(()) };
        check()?;
        if generations.len() > 4096 || max_frames == 0 || max_frames > 2_000_000
            || generations.iter().any(|generation| *generation == 0)
            || generations.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(WorkspaceHistoryError::Limit);
        }
        let _lock = self.acquire_process_lock().map_err(WorkspaceHistoryError::Store)?;
        check()?;
        let selected = head::read_head_bounded(&self.root.join("HEAD")).map_err(WorkspaceHistoryError::Store)?
            .ok_or(WorkspaceHistoryError::Missing)?.head;
        if selected != expected { return Err(WorkspaceHistoryError::Store(StoreError::WrongBase)); }
        let path = self.root.join("journal");
        let mut file = backend_platform::durability::open_regular_file_nofollow(&path)
            .map_err(|error| WorkspaceHistoryError::Store(io_error(&error)))?;
        let identity = history_file_identity(&file, &path).map_err(WorkspaceHistoryError::Store)?;
        let length = file.metadata().map_err(|error| WorkspaceHistoryError::Store(io_error(&error)))?.len();
        let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| WorkspaceHistoryError::Limit)?;
        if length % record_bytes != 0 { return Err(WorkspaceHistoryError::Store(StoreError::Corrupt)); }
        let frames = length / record_bytes;
        if frames > max_frames { return Err(WorkspaceHistoryError::Limit); }
        if selected.journal_sequence() > frames { return Err(WorkspaceHistoryError::Store(StoreError::Corrupt)); }
        let mut previous = [0u8; 32];
        let mut current = None;
        let mut out = Vec::new();
        out.try_reserve_exact(generations.len()).map_err(|_| WorkspaceHistoryError::Limit)?;
        let mut wanted = 0usize;
        for sequence in 1..=frames {
            check()?;
            let frame = journal::read_record(&mut file).map_err(WorkspaceHistoryError::Store)?;
            if frame.sequence != sequence || frame.previous != previous { return Err(WorkspaceHistoryError::Store(StoreError::Corrupt)); }
            current = apply_frame(&path, &frame, current.as_ref()).map_err(WorkspaceHistoryError::Store)?;
            if frame.tag == PUBLISHED_TAG && generations.get(wanted) == Some(&frame.payload.target_generation) {
                let binding = frame.payload.workspace.ok_or(WorkspaceHistoryError::Store(StoreError::Corrupt))?;
                if binding.root() != &frame.payload.target || binding.closure() != frame.payload.closure {
                    return Err(WorkspaceHistoryError::Store(StoreError::Corrupt));
                }
                out.push(SelectedHead { journal_sequence: frame.sequence, descriptor: frame.payload });
                wanted += 1;
            }
            previous = frame.checksum;
        }
        check()?;
        if file.metadata().map_err(|error| WorkspaceHistoryError::Store(io_error(&error)))?.len() != length
            || history_file_identity(&file, &path).map_err(WorkspaceHistoryError::Store)? != identity
            || head::read_head_bounded(&self.root.join("HEAD")).map_err(WorkspaceHistoryError::Store)?.map(|checkpoint| checkpoint.head) != Some(selected) {
            return Err(WorkspaceHistoryError::Store(StoreError::WrongBase));
        }
        if current != Some(selected) { return Err(WorkspaceHistoryError::Store(StoreError::Corrupt)); }
        if wanted != generations.len() { return Err(WorkspaceHistoryError::Missing); }
        Ok(out)
    }

    /// Reopens the exact publication selected immediately before the named
    /// current workspace head. The current publication's authenticated
    /// journal descriptor binds its base root and generation; this method
    /// replays the hash-chained journal prefix to recover that base's exact
    /// closure descriptor.
    ///
    /// The scan uses constant memory and stops at the requested generation.
    /// It is intended for cold persisted-transition admission, not a hot
    /// publication path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the named workspace head is not
    /// currently selected or its publication history is malformed.
    pub fn workspace_base_publication(
        &self,
        target_root: Hash,
        target_generation: u64,
    ) -> Result<Option<SelectedHead>, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        let current = self.read_state()?.selected.ok_or(StoreError::Corrupt)?;
        let descriptor = current.descriptor();
        let binding = descriptor.workspace().ok_or(StoreError::Corrupt)?;
        if binding.root() != &descriptor.target() || binding.closure() != descriptor.closure() {
            return Err(StoreError::Corrupt);
        }
        if descriptor.target() != target_root || descriptor.target_generation() != target_generation
        {
            return Err(StoreError::Corrupt);
        }
        let base_generation = descriptor.base_generation();
        if base_generation == 0 {
            return Ok(None);
        }
        let base_root = descriptor.base().ok_or(StoreError::Corrupt)?;

        let publication =
            find_workspace_publication(&self.root.join("journal"), base_root, base_generation)?;
        publication.ok_or(StoreError::Corrupt).map(Some)
    }

    pub(super) fn read_state(&self) -> Result<JournalState, StoreError> {
        super::objects::scavenge_store_temps(&self.root)?;
        let head_file = head::read_head(&self.root.join("HEAD"))?;
        if let Some(receipt) = head_file {
            let mut cached = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
            if cached.offset < receipt.tail.offset {
                *cached = receipt.tail;
            }
        }
        let tail = self.refresh_journal_tail()?;
        let head = head_file.map(|receipt| receipt.head);
        if let Some(head) = head {
            head::validate_head_record(&self.root.join("journal"), &tail, &head)?;
        }
        let orphaned_temps = head::scavenge_head_temps(&self.root, &tail)?;
        let recovered = match (tail.selected, head) {
            (None, None) if orphaned_temps == 0 => RecoveredHead::Empty,
            (None, None | Some(_)) => return Err(StoreError::Corrupt),
            (Some(selected), Some(observed)) if selected == observed && orphaned_temps == 0 => {
                RecoveredHead::Current(selected)
            }
            (Some(selected), Some(observed)) if selected == observed => {
                RecoveredHead::RepairInterrupted {
                    selected,
                    orphaned_temps,
                }
            }
            (Some(selected), _) if orphaned_temps == 0 => RecoveredHead::RepairMissing(selected),
            (Some(selected), _) => RecoveredHead::RepairInterrupted {
                selected,
                orphaned_temps,
            },
        };
        match recovered {
            RecoveredHead::RepairMissing(selected) => {
                self.validate_recovered_workspace_membership(&selected)?;
                self.write_head(&selected)?;
            }
            RecoveredHead::RepairInterrupted {
                selected,
                orphaned_temps,
            } => {
                debug_assert_ne!(orphaned_temps, 0);
                self.validate_recovered_workspace_membership(&selected)?;
                self.write_head(&selected)?;
            }
            RecoveredHead::Current(selected) => debug_assert_eq!(tail.selected, Some(selected)),
            RecoveredHead::Empty => {}
        }
        Ok(JournalState {
            selected: tail.selected,
        })
    }

    fn validate_recovered_workspace_membership(
        &self,
        head: &SelectedHead,
    ) -> Result<(), StoreError> {
        let Some(binding) = head.descriptor.workspace() else {
            return Ok(());
        };
        binding.verify()?;
        if binding.root() != &head.descriptor.target()
            || binding.closure() != head.descriptor.closure()
        {
            return Err(StoreError::Corrupt);
        }
        let index = self.open_closure(binding.closure())?;
        let mut after = None;
        let mut seen = 0_u64;
        loop {
            let page = index.page_ids(after, 128)?;
            for &id in page.object_ids() {
                if after.is_some_and(|previous| id <= previous) {
                    return Err(StoreError::Corrupt);
                }
                self.verify_closure_member_limited(id, None)?;
                seen = seen.checked_add(1).ok_or(StoreError::Bounds)?;
                if seen > index.object_count() {
                    return Err(StoreError::Corrupt);
                }
                after = Some(id);
            }
            if page.next().is_none() {
                break;
            }
            if page.next() != after {
                return Err(StoreError::Corrupt);
            }
        }
        if seen != index.object_count() {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }

    pub(super) fn ensure_head_file(&self, head: &SelectedHead) -> Result<(), StoreError> {
        let tail = self.refresh_journal_tail()?;
        let current = head::read_head(&self.root.join("HEAD"))?;
        if current != Some(JournalCheckpoint { head: *head, tail }) {
            self.write_head(head)?;
        }
        Ok(())
    }

    fn refresh_journal_tail(&self) -> Result<JournalTail, StoreError> {
        let path = self.root.join("journal");
        let mut guard = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
        refresh_journal_tail(&path, &mut guard)
    }

    pub(super) fn published_sync_error(
        &self,
        head: &SelectedHead,
        error: StoreError,
    ) -> StoreError {
        let refreshed = self.refresh_journal_tail();
        if refreshed.is_ok_and(|tail| tail.selected == Some(*head)) {
            return StoreError::PublishedWithSyncPending(Box::new(*head));
        }
        match head::read_head(&self.root.join("HEAD")) {
            Ok(Some(observed)) if observed.head == *head => {
                StoreError::PublishedWithSyncPending(Box::new(*head))
            }
            Ok(Some(_) | None) => error,
            Err(_) => StoreError::PublishedWithSyncPending(Box::new(*head)),
        }
    }
}

fn refresh_journal_tail(path: &Path, cached: &mut JournalTail) -> Result<JournalTail, StoreError> {
    let length = journal_length(path)?;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let cached_valid = cached.offset <= length
        && cached.offset.is_multiple_of(record_bytes)
        && (cached.offset == 0 || tail_record_matches(path, cached)?);
    let refreshed = if cached_valid && cached.offset < length {
        extend_journal(path, cached, length)?
    } else if cached_valid && cached.offset == length {
        *cached
    } else {
        scan_journal(path, length)?
    };
    *cached = refreshed;
    Ok(refreshed)
}

fn journal_length(path: &Path) -> Result<u64, StoreError> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_error(&error)),
    };
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let complete = length / record_bytes * record_bytes;
    if complete != length {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| io_error(&error))?;
        file.set_len(complete).map_err(|error| io_error(&error))?;
        file.sync_all().map_err(|error| io_error(&error))?;
        sync_directory(path.parent().ok_or(StoreError::Corrupt)?)?;
    }
    Ok(complete)
}

fn tail_record_matches(path: &Path, tail: &JournalTail) -> Result<bool, StoreError> {
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let offset = tail
        .offset
        .checked_sub(record_bytes)
        .ok_or(StoreError::Corrupt)?;
    let record = journal::read_record_at_offset(path, offset)?;
    Ok(record.sequence == tail.sequence && record.checksum == tail.checksum)
}

fn scan_journal(path: &Path, length: u64) -> Result<JournalTail, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JournalTail::default());
        }
        Err(error) => return Err(io_error(&error)),
    };
    validate_stream(path, &mut file, 0, length, 1, [0; 32], None)
}

fn find_workspace_publication(
    path: &Path,
    workspace_root: Hash,
    generation: u64,
) -> Result<Option<SelectedHead>, StoreError> {
    let length = journal_length(path)?;
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    let mut offset = 0_u64;
    let mut expected_sequence = 1_u64;
    let mut previous = [0_u8; 32];
    let mut selected = None;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    while offset < length {
        let frame = journal::read_record(&mut file)?;
        if frame.sequence != expected_sequence || frame.previous != previous {
            return Err(StoreError::Corrupt);
        }
        selected = apply_frame(path, &frame, selected.as_ref())?;
        if frame.tag == PUBLISHED_TAG
            && frame.payload.target == workspace_root
            && frame.payload.target_generation == generation
        {
            let binding = frame.payload.workspace.ok_or(StoreError::Corrupt)?;
            if binding.root() != &workspace_root || binding.closure() != frame.payload.closure {
                return Err(StoreError::Corrupt);
            }
            return Ok(Some(SelectedHead {
                journal_sequence: frame.sequence,
                descriptor: frame.payload,
            }));
        }
        previous = frame.checksum;
        expected_sequence = expected_sequence.checked_add(1).ok_or(StoreError::Bounds)?;
        offset = offset.checked_add(record_bytes).ok_or(StoreError::Bounds)?;
    }
    if offset != length {
        return Err(StoreError::Corrupt);
    }
    Ok(None)
}

fn extend_journal(
    path: &Path,
    cached: &JournalTail,
    length: u64,
) -> Result<JournalTail, StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    validate_stream(
        path,
        &mut file,
        cached.offset,
        length,
        cached.sequence.checked_add(1).ok_or(StoreError::Bounds)?,
        cached.checksum,
        cached.selected,
    )
}

fn validate_stream(
    path: &Path,
    file: &mut File,
    mut offset: u64,
    length: u64,
    mut expected_sequence: u64,
    mut previous: Hash,
    mut selected: Option<SelectedHead>,
) -> Result<JournalTail, StoreError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(&error))?;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    while offset < length {
        let frame = journal::read_record(file)?;
        if frame.sequence != expected_sequence || frame.previous != previous {
            return Err(StoreError::Corrupt);
        }
        selected = apply_frame(path, &frame, selected.as_ref())?;
        previous = frame.checksum;
        expected_sequence = expected_sequence.checked_add(1).ok_or(StoreError::Bounds)?;
        offset = offset.checked_add(record_bytes).ok_or(StoreError::Bounds)?;
    }
    if offset != length {
        return Err(StoreError::Corrupt);
    }
    Ok(JournalTail {
        sequence: expected_sequence
            .checked_sub(1)
            .ok_or(StoreError::Corrupt)?,
        checksum: previous,
        offset,
        selected,
    })
}

fn apply_frame(
    path: &Path,
    frame: &JournalRecord,
    selected: Option<&SelectedHead>,
) -> Result<Option<SelectedHead>, StoreError> {
    match frame.tag {
        PREPARED_TAG => Ok(selected.copied()),
        PUBLISHED_TAG => {
            let prepared = journal::read_record_at(path, frame.paired_sequence)?;
            if prepared.tag != PREPARED_TAG
                || prepared.sequence != frame.paired_sequence
                || prepared.paired_sequence != 0
                || prepared.payload != frame.payload
            {
                return Err(StoreError::Corrupt);
            }
            let prior_generation = selected.map_or(0, |head| head.generation());
            let expected_base = selected.map(|head| head.descriptor.target);
            if frame.payload.base_generation != prior_generation
                || frame.payload.target_generation
                    != prior_generation.checked_add(1).ok_or(StoreError::Bounds)?
                || frame.payload.base != expected_base
            {
                return Err(StoreError::Corrupt);
            }
            Ok(Some(SelectedHead {
                journal_sequence: frame.sequence,
                descriptor: frame.payload,
            }))
        }
        _ => Err(StoreError::Corrupt),
    }
}

#[cfg(all(test, unix))]
mod sql_history_identity_tests {
    use super::*;
    #[test]
    fn named_journal_replacement_does_not_match_held_file() {
        let temp = std::env::temp_dir().join(format!("nudox-history-journal-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()));
        std::fs::create_dir(&temp).expect("private test root");
        let path = temp.join("journal");
        std::fs::write(&path, b"original").expect("original");
        let held = backend_platform::durability::open_regular_file_nofollow(&path).expect("held");
        assert!(history_file_identity(&held, &path).is_ok());
        std::fs::rename(&path, temp.join("old-journal")).expect("rename");
        std::fs::write(&path, b"replacement").expect("replacement");
        assert!(matches!(history_file_identity(&held, &path), Err(StoreError::WrongBase)));
        drop(held);
        std::fs::remove_dir_all(temp).expect("test cleanup");
    }
}
