//! Crash-safe persistence for the compiled product view.
//!
//! Workspace ownership and the product view are separate durable objects:
//! the engine owns the former while locald owns the latter's certificate
//! grammar.  This journal stores a checked snapshot plus a bounded suffix of
//! checked view events.  A torn final frame is discarded on open; a complete
//! frame with a bad checksum is treated as corruption.

use backend_engine::{
    Boundary, CoverageCapability, Cursor, CursorEvent, Faults, ViewPersistence, ViewRoot,
    WorkspaceRoot,
};
use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[path = "view_journal/envelope.rs"]
mod envelope;
use envelope::{
    admit_snapshot, apply_event, capability_fingerprint, decode_envelope, encode_envelope,
};

const MAGIC: &[u8; 8] = b"BVIEWJ01";
const VERSION: u8 = 3;
const SNAPSHOT: u8 = 1;
const EVENT: u8 = 2;
const HEADER_BYTES: usize = 8 + 1 + 1 + 8 + 32;
// A complete view snapshot is the durable recovery unit. The wire protocol
// pages this same root into 4 MiB responses, while the private local journal
// retains one bounded snapshot so restart never reparses the workspace.
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// How far recovery has got for the selected workspace root.
///
/// The three states are not one `Option`: an event with no snapshot at all is
/// malformed and fails closed, while an event whose snapshot was a stale cache
/// generation is skipped with it.  Collapsing them is what turned one
/// superseded frame into a service that would not start.
#[expect(
    clippy::large_enum_variant,
    reason = "one value lives on one stack frame for the length of one scan; \
              boxing the root would allocate per recovered snapshot to save \
              nothing"
)]
enum Scoped {
    /// No snapshot for the selected root has been seen.
    Empty,
    /// The last snapshot for the selected root was certified against another
    /// capability; it and its events are superseded.
    Stale,
    /// A snapshot admitted against the live capability, plus its chain.
    Accepted {
        root: ViewRoot,
        cursor: Cursor,
        workspace_root: [u8; 32],
    },
}

/// A view recovered from the durable product journal.
#[derive(Debug)]
pub(super) struct RecoveredView {
    pub(super) view: ViewRoot,
    pub(super) cursor: Cursor,
    pub(super) events: Vec<CursorEvent>,
    pub(super) base_sequence: u64,
}

/// Product view journal with bounded append and snapshot compaction.
#[derive(Debug)]
pub(super) struct ViewJournal {
    path: PathBuf,
    faults: Arc<Faults>,
    workspace: Cell<Option<[u8; 32]>>,
}

impl ViewJournal {
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::open_with_faults(path, Arc::new(Faults::default()))
    }

    fn open_with_faults(path: impl AsRef<Path>, faults: Arc<Faults>) -> Result<Self, String> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        Ok(Self {
            path,
            faults,
            workspace: Cell::new(None),
        })
    }

    fn load_scoped(
        &self,
        capability: &CoverageCapability,
        expected_workspace: [u8; 32],
    ) -> Result<Option<RecoveredView>, String> {
        let live = capability_fingerprint(capability);
        let mut state = Scoped::Empty;
        let mut events = Vec::new();
        self.scan_frames(|kind, payload| {
            let envelope = decode_envelope(payload)?;
            if expected_workspace != envelope.workspace_root {
                // A valid snapshot for an older selected workspace carries a
                // deliberately different producer capability. Ignore that
                // cache generation before decoding its certificate and wait
                // for a snapshot bound to the selected store HEAD.
                state = Scoped::Empty;
                events.clear();
                return Ok(());
            }
            match kind {
                SNAPSHOT => {
                    state = admit_snapshot(&envelope, capability, live)?;
                    events.clear();
                    Ok(())
                }
                EVENT => apply_event(&mut state, envelope, &mut events),
                _ => Err("view journal has an unknown record kind".to_owned()),
            }
        })?;
        self.workspace.set(Some(expected_workspace));
        let Scoped::Accepted {
            root: view, cursor, ..
        } = state
        else {
            return Ok(None);
        };
        let base_sequence = cursor
            .sequence()
            .checked_sub(
                u64::try_from(events.len()).map_err(|_| "view event count overflow".to_owned())?,
            )
            .ok_or_else(|| "view journal event sequence underflow".to_owned())?;
        Ok(Some(RecoveredView {
            view,
            cursor,
            events,
            base_sequence,
        }))
    }

    /// Loads a view only when its publication certificate names the exact
    /// owner head selected by the workspace store.  A valid journal for an
    /// older head is discarded as a cache miss so the caller can rebuild from
    /// the newly selected authoritative intent; malformed bytes still fail
    /// closed. The comparison happens before the caller can construct a
    /// `Library`, which closes the crash window between physical head
    /// selection and view binding.
    pub(super) fn load_for_workspace(
        &self,
        workspace_root: WorkspaceRoot,
        capability: &CoverageCapability,
    ) -> Result<Option<RecoveredView>, String> {
        self.load_scoped(capability, workspace_root.to_bytes())
    }

    fn persist_snapshot(
        &mut self,
        workspace_root: WorkspaceRoot,
        cursor: Cursor,
        view: &ViewRoot,
    ) -> Result<(), String> {
        let payload = encode_envelope(workspace_root, cursor, view, None)?;
        if self.would_cross_file_bound(payload.len())? {
            return self.replace_with_snapshot(&payload);
        }
        self.append(SNAPSHOT, &payload)?;
        self.compact_if_needed(workspace_root, cursor, view)
    }

    fn persist_event(
        &mut self,
        workspace_root: WorkspaceRoot,
        view: &ViewRoot,
        cursor: Cursor,
        event: &CursorEvent,
    ) -> Result<(), String> {
        let payload = encode_envelope(workspace_root, cursor, view, Some(event))?;
        // A compaction publishes the target as a snapshot and intentionally
        // omits this event. If the caller loses its acknowledgement after
        // the rename, the retry must recognize that exact target snapshot;
        // appending the event would make the recovered cursor start after the
        // snapshot and fail closed as an invalid chain.
        let snapshot = encode_envelope(workspace_root, cursor, view, None)?;
        if self.tail_matches(SNAPSHOT, &snapshot)? {
            return Ok(());
        }
        // Compact before the append crosses the scan bound. A crash after a
        // successful append must leave a file that `scan_frames` can accept;
        // appending first would create a valid frame in an oversized file
        // that recovery rejects before it can compact.
        if self.would_cross_file_bound(payload.len())? {
            return self.replace_with_snapshot(&snapshot);
        }
        self.append(EVENT, &payload)?;
        self.compact_if_needed(workspace_root, cursor, view)
    }

    fn would_cross_file_bound(&self, payload_len: usize) -> Result<bool, String> {
        let current = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(io_error(error)),
        };
        let frame = u64::try_from(HEADER_BYTES)
            .ok()
            .and_then(|header| {
                u64::try_from(payload_len)
                    .ok()
                    .and_then(|payload| header.checked_add(payload))
            })
            .ok_or_else(|| "view journal frame length overflow".to_owned())?;
        Ok(current
            .checked_add(frame)
            .is_none_or(|length| length > MAX_FILE_BYTES))
    }

    fn append(&self, kind: u8, payload: &[u8]) -> Result<(), String> {
        if payload.len() > MAX_RECORD_BYTES {
            return Err("view journal record exceeds bound".to_owned());
        }
        if self.would_cross_file_bound(payload.len())? {
            return Err("view journal append would exceed its file bound".to_owned());
        }
        let was_missing = !self.path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io_error)?;
        // A caller may retry after the frame was written and synced but the
        // acknowledgement was lost. Treat an exact tail match as the
        // successful prior append instead of creating a duplicate event.
        if self.tail_matches(kind, payload)? {
            self.sync_parent_dir()?;
            return Ok(());
        }
        write_frame(&mut file, kind, payload)?;
        file.sync_data().map_err(io_error)?;
        self.faults
            .trip(Boundary::JournalFlush)
            .map_err(|error| error.to_string())?;
        if was_missing {
            self.sync_parent_dir()?;
            self.faults
                .trip(Boundary::DirSync)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn compact_if_needed(
        &self,
        workspace_root: WorkspaceRoot,
        cursor: Cursor,
        view: &ViewRoot,
    ) -> Result<(), String> {
        let length = fs::metadata(&self.path).map_err(io_error)?.len();
        if length <= MAX_FILE_BYTES {
            return Ok(());
        }
        let payload = encode_envelope(workspace_root, cursor, view, None)?;
        self.replace_with_snapshot(&payload)
    }

    fn replace_with_snapshot(&self, payload: &[u8]) -> Result<(), String> {
        if payload.len() > MAX_RECORD_BYTES {
            return Err("view journal snapshot exceeds record bound".to_owned());
        }
        let frame_length = u64::try_from(HEADER_BYTES)
            .ok()
            .and_then(|header| {
                u64::try_from(payload.len())
                    .ok()
                    .and_then(|payload| header.checked_add(payload))
            })
            .ok_or_else(|| "view journal snapshot length overflow".to_owned())?;
        if frame_length > MAX_FILE_BYTES {
            return Err("view journal snapshot exceeds file bound".to_owned());
        }
        let temporary = self.path.with_extension("view.journal.tmp");
        let mut file = File::create(&temporary).map_err(io_error)?;
        self.faults
            .trip(Boundary::TempCreate)
            .map_err(|error| error.to_string())?;
        write_frame(&mut file, SNAPSHOT, payload)?;
        self.faults
            .trip(Boundary::TempWrite)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(io_error)?;
        self.faults
            .trip(Boundary::FileSync)
            .map_err(|error| error.to_string())?;
        fs::rename(&temporary, &self.path).map_err(io_error)?;
        self.faults
            .trip(Boundary::Rename)
            .map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            backend_platform::durability::open_directory(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(io_error)?;
        }
        self.faults
            .trip(Boundary::DirSync)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn sync_parent_dir(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            backend_platform::durability::open_directory(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(io_error)?;
        }
        Ok(())
    }

    fn tail_matches(&self, kind: u8, payload: &[u8]) -> Result<bool, String> {
        let frame_length = u64::try_from(HEADER_BYTES)
            .ok()
            .and_then(|header| {
                u64::try_from(payload.len())
                    .ok()
                    .and_then(|payload| header.checked_add(payload))
            })
            .ok_or_else(|| "view journal frame length overflow".to_owned())?;
        let length = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(error)),
        };
        if length < frame_length {
            return Ok(false);
        }
        let mut file = File::open(&self.path).map_err(io_error)?;
        file.seek(SeekFrom::Start(length - frame_length))
            .map_err(io_error)?;
        let frame_length = usize::try_from(frame_length)
            .map_err(|_| "view journal frame length exceeds platform bounds".to_owned())?;
        let mut frame = vec![0_u8; frame_length];
        file.read_exact(&mut frame).map_err(io_error)?;
        if frame[..8] != *MAGIC || frame[8] != VERSION || frame[9] != kind {
            return Ok(false);
        }
        let stored_length = u64::from_be_bytes(
            frame[10..18]
                .try_into()
                .map_err(|_| "view journal length".to_owned())?,
        );
        if stored_length != u64::try_from(payload.len()).unwrap_or(u64::MAX)
            || frame[18..50] != *blake3::hash(payload).as_bytes()
        {
            return Ok(false);
        }
        Ok(frame[HEADER_BYTES..] == *payload)
    }

    fn scan_frames<F>(&self, mut visit: F) -> Result<(), String>
    where
        F: FnMut(u8, &[u8]) -> Result<(), String>,
    {
        let length = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
        };
        if length > MAX_FILE_BYTES {
            return Err("view journal exceeds bounded file size".to_owned());
        }
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => {
                if error.kind() == io::ErrorKind::NotFound {
                    return Ok(());
                }
                return Err(io_error(error));
            }
        };
        let mut offset = 0u64;
        loop {
            let mut header = [0_u8; HEADER_BYTES];
            match file.read_exact(&mut header) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    self.truncate(
                        usize::try_from(offset)
                            .map_err(|_| "view journal offset overflow".to_owned())?,
                    )?;
                    break;
                }
                Err(error) => return Err(io_error(error)),
            }
            if &header[..8] != MAGIC {
                return Err("view journal magic mismatch".to_owned());
            }
            let version = header[8];
            let kind = header[9];
            if version != VERSION {
                return Err("view journal version mismatch".to_owned());
            }
            let record_length = u64::from_be_bytes(
                header[10..18]
                    .try_into()
                    .map_err(|_| "view journal length".to_owned())?,
            );
            let record_length_usize = usize::try_from(record_length)
                .map_err(|_| "view journal length overflow".to_owned())?;
            if record_length_usize > MAX_RECORD_BYTES {
                return Err("view journal record exceeds bound".to_owned());
            }
            let end = offset
                .checked_add(u64::try_from(HEADER_BYTES).unwrap_or(u64::MAX))
                .and_then(|value| value.checked_add(record_length))
                .ok_or_else(|| "view journal offset overflow".to_owned())?;
            let mut payload = vec![0_u8; record_length_usize];
            if let Err(error) = file.read_exact(&mut payload) {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    self.truncate(
                        usize::try_from(offset)
                            .map_err(|_| "view journal offset overflow".to_owned())?,
                    )?;
                    break;
                }
                return Err(io_error(error));
            }
            let actual = blake3::hash(&payload);
            if header[18..50] != *actual.as_bytes() {
                if end == length {
                    self.truncate(
                        usize::try_from(offset)
                            .map_err(|_| "view journal offset overflow".to_owned())?,
                    )?;
                    break;
                }
                return Err("view journal checksum mismatch".to_owned());
            }
            visit(kind, &payload)?;
            offset = end;
        }
        Ok(())
    }

    fn truncate(&self, length: usize) -> Result<(), String> {
        let file = OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(io_error)?;
        file.set_len(u64::try_from(length).unwrap_or(u64::MAX))
            .map_err(io_error)?;
        // Tail repair is itself a durable state transition. Without the
        // sync, a crash after recovery can resurrect the torn suffix and
        // make the next startup take a different journal history.
        file.sync_all().map_err(io_error)?;
        if let Some(parent) = self.path.parent() {
            backend_platform::durability::open_directory(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(io_error)?;
        }
        Ok(())
    }
}

impl ViewPersistence for ViewJournal {
    fn persist(
        &mut self,
        workspace_root: WorkspaceRoot,
        view: &ViewRoot,
        cursor: Cursor,
        event: Option<&CursorEvent>,
    ) -> Result<(), String> {
        let workspace = workspace_root.to_bytes();
        if self.workspace.get() != Some(workspace) {
            // A workspace HEAD change starts a new independently recoverable
            // view generation. An event is relative to the prior in-memory
            // view, whose snapshot is certified for another workspace root;
            // retain the checked target as the first snapshot for this root.
            self.persist_snapshot(workspace_root, cursor, view)?;
            self.workspace.set(Some(workspace));
            return Ok(());
        }
        let result = match event {
            Some(CursorEvent::View { delta })
                if matches!(delta.delta(), backend_engine::ViewDelta::Reset { .. }) =>
            {
                // A reset is a cold replacement. Persist its checked root
                // once as the new base snapshot; no compact event should
                // ever smuggle the complete relation into a per-delta
                // certificate.
                self.persist_snapshot(workspace_root, cursor, view)
            }
            Some(event @ CursorEvent::View { .. }) => {
                self.persist_event(workspace_root, view, cursor, event)
            }
            // Intent certificates require the original operation preimage,
            // which is intentionally not recoverable from an opaque ID. A
            // complete snapshot still advances the durable cursor and gives
            // old subscribers a typed reset rather than inventing an event.
            Some(CursorEvent::Intent { .. }) | None => {
                self.persist_snapshot(workspace_root, cursor, view)
            }
        };
        if result.is_ok() {
            self.workspace.set(Some(workspace));
        }
        result
    }
}

fn write_frame(file: &mut File, kind: u8, payload: &[u8]) -> Result<(), String> {
    file.write_all(MAGIC).map_err(io_error)?;
    file.write_all(&[VERSION, kind]).map_err(io_error)?;
    file.write_all(
        &u64::try_from(payload.len())
            .map_err(|_| "view journal payload length overflow".to_owned())?
            .to_be_bytes(),
    )
    .map_err(io_error)?;
    file.write_all(blake3::hash(payload).as_bytes())
        .map_err(io_error)?;
    file.write_all(payload).map_err(io_error)
}

fn io_error(error: io::Error) -> String {
    let message = error.to_string();
    drop(error);
    message
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
#[path = "view_journal_tests.rs"]
mod tests;
