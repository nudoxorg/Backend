//! Local-first client index control plane.
//!
//! The client pins one remote manifest, keeps an independent overlay, and
//! records which immutable ranges it already holds.

use arrayvec::ArrayVec;

use super::{
    ClientIndex, ClientState, ClientSyncError, ClientSyncPhase, CompleteLocalSelection,
    DemandSelection, DisposableProjection, EffectiveSearchResult, LocalDelta, LocalQueryTerminal,
    LocalSegmentSelection, LocalSelection, MAX_CLIENT_DEMANDS, MAX_CLIENT_OVERLAY_ENTRIES,
    MAX_CLIENT_PROJECTIONS, MAX_CLIENT_RESIDENT_RANGES, ManifestSegment, OverlayEntry, OverlayFact,
    OverlayGeneration, OverlayKey, OverlayObservation, PinnedState, RemoteManifest, RemoteSearch,
    RemoteSearchTerminal, ResidentRange, SegmentDemand, SelectionOutput, SelectionScratch,
    SyncCancellation, SyncTerminal, push_resident, residence_covers,
};

impl ClientIndex {
    /// Creates an empty client that owns no remote manifest but can retain local deltas immediately.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ClientState::AwaitingManifest,
            overlay_generation: OverlayGeneration::first(),
            overlay: ArrayVec::new(),
            residence: ArrayVec::new(),
            projections: ArrayVec::new(),
        }
    }

    /// Returns the closed synchronization phase without exposing mutable state combinations.
    #[must_use]
    pub const fn phase(&self) -> ClientSyncPhase {
        match &self.state {
            ClientState::AwaitingManifest => ClientSyncPhase::AwaitingManifest,
            ClientState::Pinned { manifest, state } => state.phase(manifest.generation.accepted()),
        }
    }

    /// Returns the current local overlay revision independent from the accepted remote base.
    #[must_use]
    pub const fn overlay_generation(&self) -> OverlayGeneration {
        self.overlay_generation
    }

    /// Applies one local code or environment delta without altering remote canonical state.
    ///
    /// # Errors
    ///
    /// Returns a typed generation or bounded-overlay capacity error before changing controller
    /// state.
    pub fn record_local_delta(
        &mut self,
        delta: LocalDelta,
    ) -> Result<OverlayGeneration, ClientSyncError> {
        let next_generation = self.overlay_generation.next()?;
        let key = delta.key();
        let fact = match delta {
            LocalDelta::Upsert { value, .. } => OverlayFact::Upsert(value),
            LocalDelta::Tombstone { .. } => OverlayFact::Tombstone,
        };
        let insertion = self.overlay.binary_search_by_key(&key, |entry| entry.key);
        if insertion.is_err() && self.overlay.len() == MAX_CLIENT_OVERLAY_ENTRIES {
            return Err(ClientSyncError::OverlayCapacity {
                maximum: MAX_CLIENT_OVERLAY_ENTRIES,
            });
        }
        match insertion {
            Ok(index) => {
                let Some(entry) = self.overlay.get_mut(index) else {
                    return Err(ClientSyncError::OverlayCapacity {
                        maximum: MAX_CLIENT_OVERLAY_ENTRIES,
                    });
                };
                entry.fact = fact;
            }
            Err(index) => {
                if self
                    .overlay
                    .try_insert(index, OverlayEntry { key, fact })
                    .is_err()
                {
                    return Err(ClientSyncError::OverlayCapacity {
                        maximum: MAX_CLIENT_OVERLAY_ENTRIES,
                    });
                }
            }
        }
        self.overlay_generation = next_generation;
        Ok(self.overlay_generation)
    }

    /// Looks up the local overlay without consulting or mutating the remote base manifest.
    #[must_use]
    pub fn overlay(&self, key: OverlayKey) -> OverlayObservation {
        let Ok(index) = self.overlay.binary_search_by_key(&key, |entry| entry.key) else {
            return OverlayObservation::Absent;
        };
        let Some(entry) = self.overlay.get(index) else {
            return OverlayObservation::Absent;
        };
        match entry.fact {
            OverlayFact::Upsert(value) => OverlayObservation::Upsert { value },
            OverlayFact::Tombstone => OverlayObservation::Tombstone,
        }
    }

    /// Applies one complete remote manifest terminal without discarding local overlay or receipts.
    pub fn accept_manifest(
        &mut self,
        remote: RemoteManifest,
        cancellation: SyncCancellation,
    ) -> SyncTerminal {
        if cancellation == SyncCancellation::Cancelled {
            return SyncTerminal::Cancelled;
        }
        match (&self.state, remote) {
            (_, RemoteManifest::Partial { generation }) => SyncTerminal::Partial { generation },
            (ClientState::AwaitingManifest, RemoteManifest::Initial(manifest)) => {
                let base = manifest.generation.accepted();
                self.state = ClientState::Pinned {
                    manifest,
                    state: PinnedState::Synchronizing,
                };
                SyncTerminal::AcceptedInitial { base }
            }
            (
                ClientState::Pinned {
                    manifest: accepted, ..
                },
                RemoteManifest::Initial(manifest),
            ) => SyncTerminal::Stale {
                accepted: accepted.generation.accepted(),
                observed: manifest.generation,
            },
            (ClientState::AwaitingManifest, RemoteManifest::Advance { .. }) => {
                SyncTerminal::AwaitingManifest
            }
            (
                ClientState::Pinned {
                    manifest: accepted, ..
                },
                RemoteManifest::Advance { previous, next },
            ) => {
                let current = accepted.generation.accepted();
                if !previous.matches(current) {
                    return SyncTerminal::OutOfOrder {
                        expected: current,
                        observed: previous,
                    };
                }
                if next.generation.matches(current) || next.epoch <= accepted.epoch {
                    return SyncTerminal::Stale {
                        accepted: current,
                        observed: next.generation,
                    };
                }
                let successor = next.generation.accepted();
                self.state = ClientState::Pinned {
                    manifest: next,
                    state: PinnedState::Synchronizing,
                };
                SyncTerminal::AcceptedAdvance {
                    previous: current,
                    next: successor,
                }
            }
        }
    }

    /// Marks the currently pinned manifest as locally current after its background range work ends.
    ///
    /// This transition changes no canonical manifest, local overlay, or range receipt.
    pub fn complete_sync(
        &mut self,
        proof: CompleteLocalSelection,
        cancellation: SyncCancellation,
    ) -> SyncTerminal {
        if cancellation == SyncCancellation::Cancelled {
            return SyncTerminal::Cancelled;
        }
        match &mut self.state {
            ClientState::AwaitingManifest => SyncTerminal::AwaitingManifest,
            ClientState::Pinned { manifest, state } => {
                let base = manifest.generation.accepted();
                if proof.base != base {
                    return SyncTerminal::LocalSelectionStale {
                        accepted: base,
                        proved: proof.base,
                    };
                }
                if !manifest.all_segments().copied().all(|segment| {
                    residence_covers(
                        &self.residence,
                        SegmentDemand::new(segment.id, segment.full_range()),
                    )
                }) {
                    return SyncTerminal::LocalSelectionIncomplete { base };
                }
                *state = PinnedState::LocalReady;
                SyncTerminal::LocalReady { base }
            }
        }
    }

    fn coalesced_residence(
        &self,
        resident: ResidentRange,
    ) -> Result<ArrayVec<ResidentRange, MAX_CLIENT_RESIDENT_RANGES>, ClientSyncError> {
        let mut next = ArrayVec::new();
        let mut merged = resident;
        let mut inserted = false;
        for present in self.residence.iter().copied() {
            if inserted {
                push_resident(&mut next, present)?;
                continue;
            }
            match present.segment.cmp(&merged.segment) {
                core::cmp::Ordering::Less => push_resident(&mut next, present)?,
                core::cmp::Ordering::Greater => {
                    push_resident(&mut next, merged)?;
                    push_resident(&mut next, present)?;
                    inserted = true;
                }
                core::cmp::Ordering::Equal => {
                    if present.range.end() < merged.range.start() {
                        push_resident(&mut next, present)?;
                    } else if merged.range.end() < present.range.start() {
                        push_resident(&mut next, merged)?;
                        push_resident(&mut next, present)?;
                        inserted = true;
                    } else {
                        merged.range = merged.range.merge(present.range);
                    }
                }
            }
        }
        if !inserted {
            push_resident(&mut next, merged)?;
        }
        Ok(next)
    }

    fn residence_covers(&self, demand: SegmentDemand) -> bool {
        residence_covers(&self.residence, demand)
    }

    /// Records a locally retained exact range after checking it against the currently pinned manifest.
    ///
    /// Receipts are kept sorted by `(segment, start)` and coalesced when adjacent or overlapping,
    /// so coverage checks require a single partition lookup rather than scanning all receipts.
    ///
    /// # Errors
    ///
    /// Returns a typed manifest-bound or residence-capacity error before changing retained
    /// receipts.
    pub fn record_resident_range(
        &mut self,
        resident: ResidentRange,
    ) -> Result<(), ClientSyncError> {
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::ResidentRangeBeforeManifest);
        };
        let Some(segment) = manifest.find(resident.segment) else {
            return Err(ClientSyncError::ResidentRangeOutsideManifest);
        };
        if !segment.full_range().contains(resident.range) {
            return Err(ClientSyncError::ResidentRangeOutsideManifest);
        }
        let next = self.coalesced_residence(resident)?;
        self.residence = next;
        Ok(())
    }

    /// Records a disposable projection that may later be evicted without changing canonical facts.
    ///
    /// # Errors
    ///
    /// Returns [`ClientSyncError::ProjectionCapacity`] if no bounded projection slot remains.
    pub fn record_disposable_projection(
        &mut self,
        projection: DisposableProjection,
    ) -> Result<(), ClientSyncError> {
        if self.projections.contains(&projection) {
            return Ok(());
        }
        if self.projections.len() == MAX_CLIENT_PROJECTIONS {
            return Err(ClientSyncError::ProjectionCapacity {
                maximum: MAX_CLIENT_PROJECTIONS,
            });
        }
        if self.projections.try_push(projection).is_err() {
            return Err(ClientSyncError::ProjectionCapacity {
                maximum: MAX_CLIENT_PROJECTIONS,
            });
        }
        Ok(())
    }

    /// Drops only disposable local projections and returns the exact number evicted.
    pub fn evict_disposable_projections(&mut self) -> usize {
        let evicted = self.projections.len();
        self.projections.clear();
        evicted
    }

    /// Enters a remote-outage phase while retaining the pinned manifest, all range receipts, and overlay.
    pub const fn disconnect(&mut self) {
        if let ClientState::Pinned { state, .. } = &mut self.state {
            *state = PinnedState::Disconnected;
        }
    }

    /// Resumes background synchronization without discarding retained canonical local capability.
    pub const fn reconnect(&mut self) {
        if let ClientState::Pinned { state, .. } = &mut self.state {
            *state = PinnedState::Synchronizing;
        }
    }

    /// Selects locally retained immutable ranges through caller-owned scratch and transactional output.
    ///
    /// # Errors
    ///
    /// Every capacity or manifest-bound failure occurs before either scratch or output changes.
    pub fn select_local<'index, 'output>(
        &'index self,
        selection: DemandSelection<'_>,
        cancellation: SyncCancellation,
        scratch: SelectionScratch<'_, 'index>,
        output: SelectionOutput<'output, 'index>,
    ) -> Result<LocalQueryTerminal<'output, 'index>, ClientSyncError> {
        if cancellation == SyncCancellation::Cancelled {
            return Ok(LocalQueryTerminal::Cancelled);
        }
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::LocalSelectionBeforeManifest);
        };
        let demand_count = selection.demands.len();
        let scratch_slots = scratch.into_slots();
        let scratch_available = scratch_slots.len();
        let Some(scratch_slots) = scratch_slots.get_mut(..demand_count) else {
            return Err(ClientSyncError::InsufficientScratch {
                required: demand_count,
                available: scratch_available,
            });
        };
        let output_slots = output.into_slots();
        let output_available = output_slots.len();
        let Some(output_slots) = output_slots.get_mut(..demand_count) else {
            return Err(ClientSyncError::InsufficientOutput {
                required: demand_count,
                available: output_available,
            });
        };
        let mut selected_segments = ArrayVec::<&ManifestSegment, MAX_CLIENT_DEMANDS>::new();
        for demand in selection.demands {
            let Some(segment) = manifest.find(demand.segment) else {
                return Err(ClientSyncError::DemandRangeOutsideManifest);
            };
            if !segment.full_range().contains(demand.range) {
                return Err(ClientSyncError::DemandRangeOutsideManifest);
            }
            if selected_segments.try_push(segment).is_err() {
                return Err(ClientSyncError::TooManyDemands {
                    supplied: demand_count,
                    maximum: MAX_CLIENT_DEMANDS,
                });
            }
        }

        let mut missing = 0;
        for ((slot, demand), segment) in scratch_slots
            .iter_mut()
            .zip(selection.demands)
            .zip(selected_segments)
        {
            let resident = self.residence_covers(*demand);
            *slot = Some(if resident {
                LocalSelection::Present(LocalSegmentSelection {
                    segment,
                    range: demand.range,
                })
            } else {
                missing += 1;
                LocalSelection::Missing(ResidentRange::new(demand.segment, demand.range))
            });
        }
        output_slots.copy_from_slice(scratch_slots);
        let selections = &*output_slots;
        if missing == 0 {
            Ok(LocalQueryTerminal::Complete {
                selections,
                proof: CompleteLocalSelection {
                    base: manifest.generation.accepted(),
                },
            })
        } else {
            Ok(LocalQueryTerminal::Partial {
                selections,
                missing,
            })
        }
    }

    /// Copies immediate remote candidate evidence only when it matches the accepted immutable base.
    ///
    /// Candidate responses contain no source span and therefore cannot manufacture trusted source
    /// provenance while local synchronization is still running.
    ///
    /// # Errors
    ///
    /// Returns a typed base-authority or caller-output capacity error without retaining reply
    /// storage or changing the controller.
    pub fn accept_remote_search(
        &self,
        search: &RemoteSearch<'_>,
        output: &mut [Option<EffectiveSearchResult>],
    ) -> Result<RemoteSearchTerminal, ClientSyncError> {
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::RemoteSearchBeforeManifest);
        };
        let base = manifest.generation.accepted();
        if search.generation.generation() != base.generation() {
            return Err(ClientSyncError::RemoteSearchGenerationMismatch {
                expected: base.generation(),
                observed: search.generation.generation(),
            });
        }
        if search.generation.snapshot() != base.snapshot() {
            return Err(ClientSyncError::RemoteSearchSnapshotMismatch {
                expected: base.snapshot(),
                observed: search.generation.snapshot(),
            });
        }
        let Some(response_slots) = output.get_mut(..search.candidates.len()) else {
            return Err(ClientSyncError::InsufficientOutput {
                required: search.candidates.len(),
                available: output.len(),
            });
        };
        let effects = search.candidates.iter().copied().filter_map(|candidate| {
            match self.overlay(candidate.key) {
                OverlayObservation::Absent => Some(EffectiveSearchResult::Remote(candidate)),
                OverlayObservation::Upsert { value } => Some(EffectiveSearchResult::LocalOverlay {
                    key: candidate.key,
                    document: value,
                }),
                OverlayObservation::Tombstone => None,
            }
        });
        let mut returned = 0;
        for (slot, effective) in response_slots.iter_mut().zip(effects) {
            *slot = Some(effective);
            returned += 1;
        }
        for slot in response_slots.iter_mut().skip(returned) {
            *slot = None;
        }
        Ok(RemoteSearchTerminal::Candidates { base, returned })
    }
}
