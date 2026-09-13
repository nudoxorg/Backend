//! Open, restart, and bounded recovery façade for hash-chain journals.

use super::frame::read_frame_at_path;
use super::scan::{collect_recovery, repair_tail, scan_path};
use super::{
    File, HashChainJournal, JournalCheckpoint, JournalCodec, JournalError, JournalFrame,
    JournalFrameRef, JournalLimits, JournalReceipt, JournalRecovery, JournalScan, JournalState,
    Mutex, OpenOptions, Path, containing_directory, validate_limits,
};

impl<D: JournalCodec> HashChainJournal<D> {
    /// Opens a journal and repairs only a torn final frame.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, JournalRecovery<D>), JournalError> {
        Self::open_with_limits(path, JournalLimits::default())
    }

    /// Opens a journal with an explicit bounded recovery envelope.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_with_limits(
        path: impl AsRef<Path>,
        limits: JournalLimits,
    ) -> Result<(Self, JournalRecovery<D>), JournalError> {
        validate_limits(limits)?;
        let path = path.as_ref().to_owned();
        std::fs::create_dir_all(containing_directory(&path))?;
        let existed = path.exists();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let (recovery, scan) = collect_recovery::<D>(&path, limits, None)?;
        repair_tail(&path, &scan)?;
        if !existed {
            File::open(containing_directory(&path))?.sync_all()?;
        }
        let next_sequence = recovery.last_sequence.map_or(Ok(0), |sequence| {
            sequence.checked_add(1).ok_or(JournalError::Bounds)
        })?;
        let chain = recovery.chain;
        Ok((
            Self {
                path,
                state: Mutex::new(JournalState {
                    file,
                    next_sequence,
                    chain,
                    next_offset: scan.valid_offset,
                    unusable: false,
                }),
            },
            recovery,
        ))
    }

    /// Opens a journal while retaining only the authenticated tail
    /// observation.  This is the normal owner path when a fixed HEAD
    /// checkpoint is available; callers can then use
    /// [`Self::open_from_checkpoint`] and [`Self::fold_from_checkpoint`] for
    /// the small suffix that followed the selected frame.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_streaming(
        path: impl AsRef<Path>,
        limits: JournalLimits,
    ) -> Result<(Self, JournalScan<D>), JournalError> {
        Self::open_streaming_with(path, limits, |_frame| Ok(()))
    }

    /// Opens a journal and visits each validated frame without retaining the
    /// history.  The visitor runs before the append cursor is returned, which
    /// lets a domain-specific owner derive a fixed-width watermark in the
    /// same bounded scan used to authenticate the file.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_streaming_with<F>(
        path: impl AsRef<Path>,
        limits: JournalLimits,
        visitor: F,
    ) -> Result<(Self, JournalScan<D>), JournalError>
    where
        F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        validate_limits(limits)?;
        let path = path.as_ref().to_owned();
        std::fs::create_dir_all(containing_directory(&path))?;
        let existed = path.exists();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let scan = scan_path(&path, limits, None, visitor)?;
        repair_tail(&path, &scan)?;
        if !existed {
            File::open(containing_directory(&path))?.sync_all()?;
        }
        let next_sequence = scan.last_sequence.map_or(Ok(0), |sequence| {
            sequence.checked_add(1).ok_or(JournalError::Bounds)
        })?;
        Ok((
            Self {
                path,
                state: Mutex::new(JournalState {
                    file,
                    next_sequence,
                    chain: scan.chain,
                    next_offset: scan.valid_offset,
                    unusable: false,
                }),
            },
            scan,
        ))
    }

    /// Opens a journal from a previously synced frame identity.
    ///
    /// The checkpoint frame is fetched at its exact offset and checked
    /// against every fixed-width field in `checkpoint`. Only the suffix after
    /// that frame is then read, validated, and optionally retained in the
    /// returned compatibility recovery value. A torn suffix is truncated at
    /// the last verified boundary before the append cursor is initialized.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_from_checkpoint(
        path: impl AsRef<Path>,
        checkpoint: JournalCheckpoint<D>,
        limits: JournalLimits,
    ) -> Result<(Self, JournalRecovery<D>), JournalError> {
        validate_limits(limits)?;
        let path = path.as_ref().to_owned();
        let file = OpenOptions::new()
            .create(false)
            .read(true)
            .append(true)
            .open(&path)?;
        let (recovery, scan) = collect_recovery(&path, limits, Some(checkpoint))?;
        repair_tail(&path, &scan)?;
        let next_sequence = recovery.last_sequence.map_or(Ok(0), |sequence| {
            sequence.checked_add(1).ok_or(JournalError::Bounds)
        })?;
        Ok((
            Self {
                path,
                state: Mutex::new(JournalState {
                    file,
                    next_sequence,
                    chain: recovery.chain,
                    next_offset: scan.valid_offset,
                    unusable: false,
                }),
            },
            recovery,
        ))
    }

    /// Opens a journal from a checked frame while retaining no suffix
    /// history. This is the restart path for compacted owners that keep a
    /// typed snapshot plus only the frames after its checkpoint.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_from_checkpoint_streaming(
        path: impl AsRef<Path>,
        checkpoint: JournalCheckpoint<D>,
        limits: JournalLimits,
    ) -> Result<(Self, JournalScan<D>), JournalError> {
        Self::open_from_checkpoint_streaming_with(path, checkpoint, limits, |_frame| Ok(()))
    }

    /// Opens from a checked frame and visits the bounded suffix while
    /// retaining no frame history.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_from_checkpoint_streaming_with<F>(
        path: impl AsRef<Path>,
        checkpoint: JournalCheckpoint<D>,
        limits: JournalLimits,
        visitor: F,
    ) -> Result<(Self, JournalScan<D>), JournalError>
    where
        F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        validate_limits(limits)?;
        let path = path.as_ref().to_owned();
        let file = OpenOptions::new()
            .create(false)
            .read(true)
            .append(true)
            .open(&path)?;
        let scan = scan_path(&path, limits, Some(checkpoint), visitor)?;
        repair_tail(&path, &scan)?;
        let next_sequence = scan.last_sequence.map_or(Ok(0), |sequence| {
            sequence.checked_add(1).ok_or(JournalError::Bounds)
        })?;
        Ok((
            Self {
                path,
                state: Mutex::new(JournalState {
                    file,
                    next_sequence,
                    chain: scan.chain,
                    next_offset: scan.valid_offset,
                    unusable: false,
                }),
            },
            scan,
        ))
    }

    /// Opens a journal from an append receipt without making callers spell
    /// out the receipt-to-checkpoint conversion.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_from_receipt(
        path: impl AsRef<Path>,
        receipt: JournalReceipt<D>,
        limits: JournalLimits,
    ) -> Result<(Self, JournalRecovery<D>), JournalError> {
        Self::open_from_checkpoint(path, receipt.checkpoint(), limits)
    }

    /// Returns the backing path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<D: JournalCodec> HashChainJournal<D> {
    /// Reconciles an uncertain append of one typed record.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn retry(
        &self,
        receipt: JournalReceipt<D>,
        record: &D::Record,
    ) -> Result<JournalReceipt<D>, JournalError> {
        self.retry_encoded(receipt, |payload| D::encode(record, payload))
    }

    /// Replays and validates every frame currently on disk.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn recover(&self) -> Result<JournalRecovery<D>, JournalError> {
        self.recover_with_limits(JournalLimits::default())
    }

    /// Replays and validates every frame with a bounded retained history.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn recover_with_limits(
        &self,
        limits: JournalLimits,
    ) -> Result<JournalRecovery<D>, JournalError> {
        validate_limits(limits)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        let (recovery, scan) = match collect_recovery::<D>(&self.path, limits, None) {
            Ok(value) => value,
            Err(error) => {
                state.unusable = true;
                return Err(error);
            }
        };
        if scan.truncated_tail
            && let Err(error) = (|| -> Result<(), JournalError> {
                state.file.set_len(scan.valid_offset)?;
                state.file.sync_all()?;
                let length = state.file.metadata()?.len();
                if length != scan.valid_offset {
                    return Err(JournalError::Corrupt("tail repair"));
                }
                File::open(containing_directory(&self.path))?.sync_all()?;
                Ok(())
            })()
        {
            state.unusable = true;
            return Err(error);
        }
        state.next_sequence = match recovery.last_sequence.map_or(Ok(0), |sequence| {
            sequence.checked_add(1).ok_or(JournalError::Bounds)
        }) {
            Ok(sequence) => sequence,
            Err(error) => {
                state.unusable = true;
                return Err(error);
            }
        };
        state.chain = recovery.chain;
        state.next_offset = scan.valid_offset;
        state.unusable = false;
        Ok(recovery)
    }

    /// Folds validated frames with one reusable payload buffer.
    ///
    /// The visitor must finish using the payload before returning. The scan
    /// retains no frame or payload history, so its memory is bounded by the
    /// largest frame and the domain decoder's own temporary allocation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn scan_stream<F>(
        &self,
        limits: JournalLimits,
        visitor: F,
    ) -> Result<JournalScan<D>, JournalError>
    where
        F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        validate_limits(limits)?;
        let mut guard = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        let result = scan_path(&self.path, limits, None, visitor);
        if result.is_err() {
            guard.unusable = true;
        }
        result
    }

    /// Folds validated frames into caller-owned state without retaining the
    /// journal history.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fold_stream<T, F>(
        &self,
        limits: JournalLimits,
        mut state: T,
        mut fold: F,
    ) -> Result<(T, JournalScan<D>), JournalError>
    where
        F: FnMut(&mut T, JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        let scan = self.scan_stream(limits, |frame| fold(&mut state, frame))?;
        Ok((state, scan))
    }

    /// Folds only the suffix after a checked checkpoint.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn scan_from_checkpoint<F>(
        &self,
        checkpoint: JournalCheckpoint<D>,
        limits: JournalLimits,
        visitor: F,
    ) -> Result<JournalScan<D>, JournalError>
    where
        F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        validate_limits(limits)?;
        let mut guard = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        let result = scan_path(&self.path, limits, Some(checkpoint), visitor);
        if result.is_err() {
            guard.unusable = true;
        }
        result
    }

    /// Folds only the suffix after a checked checkpoint.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fold_from_checkpoint<T, F>(
        &self,
        checkpoint: JournalCheckpoint<D>,
        limits: JournalLimits,
        mut state: T,
        mut fold: F,
    ) -> Result<(T, JournalScan<D>), JournalError>
    where
        F: FnMut(&mut T, JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        let scan =
            self.scan_from_checkpoint(checkpoint, limits, |frame| fold(&mut state, frame))?;
        Ok((state, scan))
    }

    /// Folds only the suffix after an append receipt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn scan_from_receipt<F>(
        &self,
        receipt: JournalReceipt<D>,
        limits: JournalLimits,
        visitor: F,
    ) -> Result<JournalScan<D>, JournalError>
    where
        F: FnMut(JournalFrameRef<'_, D>) -> Result<(), JournalError>,
    {
        self.scan_from_checkpoint(receipt.checkpoint(), limits, visitor)
    }

    /// Reads and validates one frame directly from a durable byte offset.
    ///
    /// The workspace HEAD stores this offset together with the selected chain
    /// and record identity. Recovery can therefore fetch the exact Prepared
    /// payload without rebuilding a second in-memory copy of every frame.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn read_frame_at(&self, offset: u64) -> Result<JournalFrame<D>, JournalError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        let result = read_frame_at_path(&self.path, offset);
        if result.is_err() {
            guard.unusable = true;
        }
        result
    }

    /// Looks up a frame at an exact durable offset.
    ///
    /// An offset at or beyond the current end is an ordinary lookup miss and
    /// leaves the append state usable.  If bytes exist at the requested
    /// offset, however, they must form a complete authenticated frame;
    /// validation failures poison the journal just like [`Self::read_frame_at`]
    /// so callers cannot continue from a potentially divergent history.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn lookup_frame_at(&self, offset: u64) -> Result<Option<JournalFrame<D>>, JournalError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| JournalError::Corrupt("poisoned journal"))?;
        let file_len = match guard.file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                guard.unusable = true;
                return Err(JournalError::Io(error));
            }
        };
        if offset >= file_len {
            return Ok(None);
        }
        let result = read_frame_at_path(&self.path, offset).map(Some);
        if result.is_err() {
            guard.unusable = true;
        }
        result
    }

    pub(crate) fn mark_unusable(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.unusable = true;
        }
    }
}
