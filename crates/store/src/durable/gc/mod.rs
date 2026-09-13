//! Bounded, crash-safe reachability collection for immutable store files.
//!
//! Collection is driven by typed roots and explicit object edges. It never
//! interprets product payloads to guess reachability.

use super::super::StoreError;
use super::super::digest;
use super::{FileStore, io_error};
use std::fs;
use std::io;
use std::path::Path;

pub(super) const GC_DIR: &str = "gc";
pub(super) const GC_STATE: &str = "state";
pub(super) const GC_ROOTS: &str = "roots";
pub(super) const GC_QUEUE: &str = "queue";
pub(super) const GC_MARK: &str = "mark";
pub(super) const GC_QUARANTINE: &str = "quarantine";
pub(super) const GC_STATE_MAGIC: &[u8] = b"LUNA_GC_STATE_V2\0";
pub(super) const GC_STATE_DIGEST_DOMAIN: &[u8] = b"store.gc.state.v2\0";
pub(super) const GC_ROOT_MAGIC: &[u8] = b"LUNA_GC_ROOTS_V1\0";
pub(super) const GC_QUEUE_DIGEST_DOMAIN: &[u8] = b"store.gc.queue.v1\0";
pub(super) const GC_MARK_DIGEST_DOMAIN: &[u8] = b"store.gc.mark.v1\0";
pub(super) const GC_CANDIDATE_DIGEST_DOMAIN: &[u8] = b"store.gc.candidate.v1\0";
pub(super) const GC_STATE_MAX_CURSOR: usize = 512;
pub(super) const GC_QUEUE_RECORD_BYTES: usize = 65;
pub(super) const GC_MARK_RECORD_BYTES: usize = 33;
pub(super) const GC_CANDIDATE_RECORD_BYTES: usize = 34;
pub(super) const GC_ROOT_RECORD_BYTES: usize = GC_QUEUE_RECORD_BYTES;
pub(super) const GC_STATE_FIXED_BYTES: usize =
    GC_STATE_MAGIC.len() + 1 + 8 + 8 + 8 + 8 + 8 + 8 + 8 + 4 + 32 + 32 + 32 + 32 + 32;
pub(super) const MAX_ROOTS: usize = 16_384;
pub(super) const MAX_PAGE_ITEMS: usize = 16_384;
pub(super) const MAX_MARK_PAGE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const GC_STATE_MAX_BYTES: usize = GC_STATE_FIXED_BYTES + GC_STATE_MAX_CURSOR;
pub(super) const MAX_MARK_ITEMS: u64 = 8_000_000;
pub(super) const MAX_QUEUE_ITEMS: u64 = 8_000_000;

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static TEST_FAULT: Mutex<Vec<(std::thread::ThreadId, u8)>> = Mutex::new(Vec::new());

mod index;
mod mark;
mod roots;
mod state;
mod state_io;
mod sweep;

use index::{MarkIndex, QueueIndex};
use roots::QueueItem;
pub use roots::{GcRoot, GcRoots};
pub use state::{GcLimits, GcReport};
use state::{GcState, Phase, validate_limits};
use state_io::{
    empty_log_digest, encode_roots, extend_log_digest, persist_state, read_bounded,
    validate_gc_files, write_atomic_store,
};
use sweep::{GcPaths, SweepKind};
use sweep::{
    build_candidate_index, candidate_index_length, quarantine_file, read_candidate,
    remove_gc_files, remove_quarantine, restore_quarantine,
};

impl FileStore {
    /// Runs a bounded, durable mark-and-sweep using caller-supplied roots and
    /// the currently selected durable head.
    ///
    /// The process lock is held for the full collection.  Every page commits
    /// its queue offset, mark counters, or sweep cursor before the next page;
    /// if the process exits during a rename, opening the store restores the
    /// quarantined source.  A complete mark with a missing child returns
    /// [`StoreError::Corrupt`] before any file is quarantined.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Bounds`] for an overlarge queue/mark or page,
    /// [`StoreError::Corrupt`] for a missing or invalid reachable object, and
    /// [`StoreError::Io`] for filesystem failures.
    pub fn collect_garbage(
        &self,
        roots: &GcRoots,
        limits: GcLimits,
    ) -> Result<GcReport, StoreError> {
        let _guard = self.lock.lock().map_err(|_| StoreError::Corrupt)?;
        let _process_lock = self.acquire_process_lock()?;
        let mut roots = roots.clone();
        if let Some(head) = self.read_state()?.selected {
            roots.add_selected_head(head);
        }
        self.collect_garbage_locked(&roots, limits)
    }

    /// Collects from the currently selected durable head plus any additional
    /// pinned or leased roots.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Bounds`] for an overlarge collection and
    /// [`StoreError::Corrupt`] when the selected head or a reachable object is
    /// invalid.
    pub fn collect_garbage_from_head(
        &self,
        mut roots: GcRoots,
        limits: GcLimits,
    ) -> Result<GcReport, StoreError> {
        let _guard = self.lock.lock().map_err(|_| StoreError::Corrupt)?;
        let _process_lock = self.acquire_process_lock()?;
        if let Some(head) = self.read_state()?.selected {
            roots.add_selected_head(head);
        }
        self.collect_garbage_locked(&roots, limits)
    }

    fn collect_garbage_locked(
        &self,
        roots: &GcRoots,
        limits: GcLimits,
    ) -> Result<GcReport, StoreError> {
        validate_limits(limits)?;
        let items = roots.normalized_items(limits.max_roots)?;
        let mut state = self.open_or_start_gc(&items, limits)?;
        let mut mark_index = if state.phase == Phase::Mark {
            let index = MarkIndex::load(&GcPaths::new(&self.root).mark, limits)?;
            state.marked_items = u64::try_from(index.len()).map_err(|_| StoreError::Bounds)?;
            Some(index)
        } else {
            None
        };
        let mut queue_index = if state.phase == Phase::Mark {
            Some(QueueIndex::load(&GcPaths::new(&self.root).queue, limits)?)
        } else {
            None
        };
        loop {
            match state.phase {
                Phase::Mark => {
                    self.mark_page(
                        &mut state,
                        limits,
                        mark_index.as_mut().ok_or(StoreError::Corrupt)?,
                        queue_index.as_mut().ok_or(StoreError::Corrupt)?,
                    )?;
                    if state.phase != Phase::Mark {
                        // Sweep uses the durable sorted index directly; do
                        // not retain the potentially large mark/queue hash
                        // tables for the remainder of this collection.
                        mark_index = None;
                        queue_index = None;
                    }
                }
                Phase::SweepPacks | Phase::SweepClosures | Phase::SweepObjects => {
                    self.sweep_page(&mut state, limits)?;
                }
                Phase::Finished => {
                    self.finish_gc(&state)?;
                    return Ok(GcReport {
                        epoch: state.epoch,
                        marked_items: state.marked_items,
                        swept_items: state.swept_items,
                        reclaimed_bytes: state.reclaimed_bytes,
                        examined_items: state.examined_items,
                    });
                }
            }
        }
    }

    fn open_or_start_gc(
        &self,
        items: &[QueueItem],
        limits: GcLimits,
    ) -> Result<GcState, StoreError> {
        let paths = GcPaths::new(&self.root);
        fs::create_dir_all(&paths.dir).map_err(|error| io_error(&error))?;
        let roots = encode_roots(items, limits)?;
        let roots_digest = digest(b"store.gc.roots.v1\0", &roots);
        let existing = read_bounded(&paths.state, GC_STATE_MAX_BYTES)?
            .map(|bytes| GcState::decode(&bytes))
            .transpose()?;
        if let Some(state) = existing.as_ref() {
            if state.phase != Phase::Finished && state.roots_digest == roots_digest {
                if validate_gc_files(&paths, state, limits).is_ok() {
                    return Ok(state.clone());
                }
                // A complete append followed by an acknowledgement error can
                // leave a valid extra queue/mark record beside the old state.
                // It cannot be resumed under that state digest; restore any
                // quarantine and restart the epoch without deleting data.
                restore_quarantine(&paths)?;
                remove_gc_files(&paths)?;
            }
            if state.phase != Phase::Finished && paths.state.exists() {
                restore_quarantine(&paths)?;
            } else if state.phase == Phase::Finished {
                remove_quarantine(&paths)?;
            }
            if paths.state.exists() {
                remove_gc_files(&paths)?;
            }
        } else if paths.quarantine.exists() {
            // A missing state cannot authorize deletion.  Restore all files
            // before beginning a fresh epoch.
            restore_quarantine(&paths)?;
            remove_gc_files(&paths)?;
        }

        let epoch = existing
            .as_ref()
            .and_then(|state| state.epoch.checked_add(1))
            .unwrap_or(1);
        let mut queue = Vec::with_capacity(
            items
                .len()
                .checked_mul(GC_QUEUE_RECORD_BYTES)
                .ok_or(StoreError::Bounds)?,
        );
        let mut queue_digest = empty_log_digest(GC_QUEUE_DIGEST_DOMAIN);
        for item in items {
            queue.extend_from_slice(&item.encode());
            queue_digest = extend_log_digest(queue_digest, GC_QUEUE_DIGEST_DOMAIN, &item.encode());
        }
        if u64::try_from(items.len()).map_err(|_| StoreError::Bounds)? > limits.max_queue_items {
            return Err(StoreError::Bounds);
        }
        write_atomic_store(&paths.roots, &roots, &paths.dir)?;
        write_atomic_store(&paths.queue, &queue, &paths.dir)?;
        write_atomic_store(&paths.mark, &[], &paths.dir)?;
        let state = GcState {
            phase: Phase::Mark,
            epoch,
            queue_offset: 0,
            marked_items: 0,
            swept_items: 0,
            reclaimed_bytes: 0,
            examined_items: 0,
            candidate_length: 0,
            cursor: Vec::new(),
            roots_digest,
            queue_digest,
            mark_digest: empty_log_digest(GC_MARK_DIGEST_DOMAIN),
            candidate_digest: empty_log_digest(GC_CANDIDATE_DIGEST_DOMAIN),
        };
        persist_state(&paths, &state)?;
        Ok(state)
    }

    fn prepare_sweep_phase(
        &self,
        state: &mut GcState,
        phase: Phase,
        limits: GcLimits,
    ) -> Result<(), StoreError> {
        let kind = match phase {
            Phase::SweepPacks => SweepKind::Pack,
            Phase::SweepClosures => SweepKind::Closure,
            Phase::SweepObjects => SweepKind::Object,
            _ => return Err(StoreError::Corrupt),
        };
        let paths = GcPaths::new(self.root());
        let digest =
            build_candidate_index(&paths, kind, &self.root().join(kind.directory()), limits)?;
        let length = candidate_index_length(&paths.candidate_index(kind))?;
        state.phase = phase;
        state.cursor.clear();
        state.candidate_digest = digest;
        state.candidate_length = length;
        Ok(())
    }

    fn sweep_page(&self, state: &mut GcState, limits: GcLimits) -> Result<(), StoreError> {
        let paths = GcPaths::new(self.root());
        let phase = sweep_kind(state.phase)?;
        let index = paths.candidate_index(phase);
        let index_len = candidate_index_length(&index)?;
        if index_len != state.candidate_length {
            return Err(StoreError::Corrupt);
        }
        let offset = decode_sweep_offset(&state.cursor, index_len)?;
        let offset = self.examine_sweep_page(state, limits, &paths, phase, &index, offset)?;
        if offset == index_len {
            self.advance_sweep_phase(state, limits)?;
        } else {
            state.cursor = offset.to_be_bytes().to_vec();
        }
        persist_state(&paths, state)?;
        #[cfg(test)]
        if state.phase == Phase::SweepObjects && !state.cursor.is_empty() && take_test_fault(4) {
            return Err(StoreError::Io(
                "injected GC failure after sweep cursor commit".to_owned(),
            ));
        }
        Ok(())
    }

    fn examine_sweep_page(
        &self,
        state: &mut GcState,
        limits: GcLimits,
        paths: &GcPaths,
        phase: SweepKind,
        index: &Path,
        mut offset: u64,
    ) -> Result<u64, StoreError> {
        let record_bytes =
            u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
        let mut examined = 0usize;
        while examined < limits.sweep_page_items {
            let Some(candidate) = read_candidate(index, offset)? else {
                break;
            };
            offset = offset.checked_add(record_bytes).ok_or(StoreError::Bounds)?;
            examined = examined.checked_add(1).ok_or(StoreError::Bounds)?;
            state.examined_items = state
                .examined_items
                .checked_add(1)
                .ok_or(StoreError::Bounds)?;
            if state.examined_items
                > limits
                    .max_queue_items
                    .checked_mul(3)
                    .ok_or(StoreError::Bounds)?
            {
                return Err(StoreError::Bounds);
            }
            let source = candidate.path(self.root());
            let metadata = match fs::symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error(&error)),
            };
            if !metadata.file_type().is_file()
                || state_io::mark_contains_sorted(&paths.mark, candidate.mark_key())?
            {
                continue;
            }
            let name = candidate.filename();
            let bytes = metadata.len();
            quarantine_file(paths, phase, &name, &source)?;
            #[cfg(test)]
            if take_test_fault(1) {
                return Err(StoreError::Io(
                    "injected GC failure after quarantine rename".to_owned(),
                ));
            }
            state.swept_items = state.swept_items.checked_add(1).ok_or(StoreError::Bounds)?;
            state.reclaimed_bytes = state
                .reclaimed_bytes
                .checked_add(bytes)
                .ok_or(StoreError::Bounds)?;
        }
        Ok(offset)
    }

    fn advance_sweep_phase(&self, state: &mut GcState, limits: GcLimits) -> Result<(), StoreError> {
        let next = match state.phase {
            Phase::SweepPacks => Some(Phase::SweepClosures),
            Phase::SweepClosures => Some(Phase::SweepObjects),
            Phase::SweepObjects => None,
            _ => return Err(StoreError::Corrupt),
        };
        if let Some(next) = next {
            self.prepare_sweep_phase(state, next, limits)
        } else {
            state.phase = Phase::Finished;
            state.cursor.clear();
            state.candidate_digest = empty_log_digest(GC_CANDIDATE_DIGEST_DOMAIN);
            state.candidate_length = 0;
            Ok(())
        }
    }

    fn finish_gc(&self, state: &GcState) -> Result<(), StoreError> {
        if state.phase != Phase::Finished {
            return Err(StoreError::Corrupt);
        }
        let paths = GcPaths::new(&self.root);
        // The Finished state is durable before this deletion.  Any crash in
        // the loop leaves a Finished state and a quarantine directory, which
        // open-time recovery can safely remove.
        remove_quarantine(&paths)?;
        // A completed epoch has no resumable work.  Retaining its queue and
        // mark log would make every later open carry the full prior history;
        // remove all collector metadata only after the quarantine is gone.
        remove_gc_files(&paths)
    }

    pub(super) fn recover_gc_on_open(&self) -> Result<(), StoreError> {
        let paths = GcPaths::new(&self.root);
        if !paths.quarantine.exists() {
            // A failed mark append can leave a malformed queue/mark sidecar
            // without having moved any store file.  Such metadata cannot be
            // resumed safely; discard only the collector files and leave all
            // immutable data untouched so the next collection starts fresh.
            let has_metadata = [
                &paths.state,
                &paths.roots,
                &paths.queue,
                &paths.mark,
                &paths.mark.with_file_name("mark.index"),
                &paths.mark.with_file_name("queue.index"),
                &paths.mark.with_file_name("mark.index.tmp"),
                &paths.mark.with_file_name("queue.index.tmp"),
                &paths.mark.with_file_name("mark.freeze.tmp"),
            ]
            .iter()
            .any(|path| path.exists())
                || paths.candidate_files().iter().any(|path| path.exists());
            if has_metadata {
                let valid = read_bounded(&paths.state, GC_STATE_MAX_BYTES)
                    .ok()
                    .flatten()
                    .and_then(|bytes| GcState::decode(&bytes).ok())
                    .is_some_and(|state| {
                        validate_gc_files(&paths, &state, GcLimits::default()).is_ok()
                    });
                if !valid {
                    remove_gc_files(&paths)?;
                }
            }
            return Ok(());
        }
        let finished = read_bounded(&paths.state, GC_STATE_MAX_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| GcState::decode(&bytes).ok())
            .is_some_and(|state| state.phase == Phase::Finished);
        if finished {
            remove_quarantine(&paths)?;
        } else {
            restore_quarantine(&paths)?;
            remove_gc_files(&paths)?;
        }
        Ok(())
    }
}

fn sweep_kind(phase: Phase) -> Result<SweepKind, StoreError> {
    match phase {
        Phase::SweepPacks => Ok(SweepKind::Pack),
        Phase::SweepClosures => Ok(SweepKind::Closure),
        Phase::SweepObjects => Ok(SweepKind::Object),
        Phase::Mark | Phase::Finished => Err(StoreError::Corrupt),
    }
}

fn decode_sweep_offset(cursor: &[u8], index_len: u64) -> Result<u64, StoreError> {
    if !cursor.is_empty() && cursor.len() != 8 {
        return Err(StoreError::Corrupt);
    }
    let offset = if cursor.is_empty() {
        0
    } else {
        u64::from_be_bytes(cursor.try_into().map_err(|_| StoreError::Corrupt)?)
    };
    let record_bytes = u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if offset > index_len || !offset.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    Ok(offset)
}

#[cfg(test)]
pub(crate) fn set_test_fault(point: u8) {
    if let Ok(mut fault) = TEST_FAULT.lock() {
        let thread = std::thread::current().id();
        fault.retain(|(owner, _)| *owner != thread);
        fault.push((thread, point));
    }
}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut fault) = TEST_FAULT.lock() else {
        return false;
    };
    let thread = std::thread::current().id();
    if let Some(index) = fault
        .iter()
        .position(|(owner, fault_point)| *owner == thread && *fault_point == point)
    {
        fault.remove(index);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
