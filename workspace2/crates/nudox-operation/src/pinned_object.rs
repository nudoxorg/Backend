//! Concrete root-and-locality-witnessed object operation.
#![allow(
    missing_docs,
    reason = "batch facts are intentionally direct public fields"
)]

use core::{convert::Infallible, marker::PhantomData, ops::Deref};

use nudox_id::GenerationId;
use nudox_object::{ObjectRef, ProviderSet};
use nudox_root::{EntryKey, GenerationEntry, GenerationView, Locality, LocalityReadError};
use nudox_schema::OperationId;
use thiserror::Error;

use crate::{BatchSource, Operation, Provider, SourcePoll, TerminalSummary};

/// Provenance uniform to the entire one-object local batch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectProvenance {
    Resident,
    Overlay,
}

#[cfg(test)]
#[allow(
    clippy::result_large_err,
    reason = "tests preserve descriptor-rich typed operation errors without heap allocation"
)]
mod tests {
    extern crate alloc;

    use alloc::{vec, vec::Vec};
    use nudox_id::{ContentId, GenerationId, ObjectDomain};
    use nudox_object::{
        ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderIdError, ProviderSet, RemoteBase,
    };
    use nudox_root::{
        EntryKey, GenerationRoot, GenerationView, LocalityError, LocalityException,
        LocalityReadError, LocalityWriteError, NonResident, PreparedLocality, RootBuildError,
        RootEntry,
    };
    use nudox_schema::SchemaId;
    use thiserror::Error;

    use super::{LocalObjectError, LocalObjectProvider, ObjectProvenance, PinnedObjectRequest};
    use crate::{BatchSource, Provider, SourcePoll, TerminalSummary};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestStep {
        ComposeProvider,
        FirstPoll,
        PromisedTerminal,
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ObservedPoll {
        Batch,
        CancelledTerminal,
        CompleteTerminal,
        PartialTerminal,
        Finished,
    }
    #[derive(Debug, Error)]
    enum TestError {
        #[error("root setup failed")]
        Root(#[from] RootBuildError),
        #[error("locality setup failed")]
        Locality(#[from] LocalityError),
        #[error("locality output failed")]
        LocalityWrite(#[from] LocalityWriteError),
        #[error("locality read failed")]
        LocalityRead(#[from] LocalityReadError),
        #[error("provider identifier setup failed")]
        ProviderId(#[from] ProviderIdError),
        #[error("object operation failed")]
        Operation(#[from] LocalObjectError<ObjectDomain>),
        #[error("{step:?} could not find its root-composed provider")]
        MissingProvider { step: TestStep },
        #[error("{step:?} expected a batch but observed {observed:?}")]
        UnexpectedPoll {
            step: TestStep,
            observed: ObservedPoll,
        },
        #[error("a request expected to fail unexpectedly produced a runnable source")]
        UnexpectedStartSuccess,
    }

    impl From<core::convert::Infallible> for TestError {
        fn from(error: core::convert::Infallible) -> Self {
            match error {}
        }
    }

    fn object(byte: u8) -> ObjectRef<ObjectDomain> {
        ObjectRef {
            content: ContentId::from([byte; 32]),
            schema: SchemaId::Object,
            length: ObjectLength::from(4),
            kind: ObjectKind::from(1),
        }
    }
    fn root(
        object: ObjectRef<ObjectDomain>,
    ) -> Result<GenerationRoot<ObjectDomain>, RootBuildError> {
        GenerationRoot::new(vec![RootEntry {
            key: EntryKey::from(1),
            parent: None,
            object,
        }])
    }
    fn locality_bytes(
        root: &GenerationRoot<ObjectDomain>,
        facts: &[LocalityException<ObjectDomain>],
    ) -> Result<Vec<u8>, TestError> {
        let prepared = PreparedLocality::prepare(root, facts)?;
        Ok(vec![0; usize::from(prepared.required_bytes)])
    }

    #[test]
    fn resident_and_overlay_emit_one_batch_then_terminal_then_fuse() -> Result<(), TestError> {
        let required = object(9);
        let root = root(required)?;
        let mut resident_bytes = locality_bytes(&root, &[])?;
        let resident = PreparedLocality::prepare(&root, &[])?.write(&mut resident_bytes)?;
        let overlay_row =
            root.locality_row(EntryKey::from(1))
                .ok_or(TestError::MissingProvider {
                    step: TestStep::ComposeProvider,
                })?;
        let overlay_facts = [LocalityException::new(
            overlay_row,
            NonResident::Overlaid(RemoteBase::Absent {
                generation: GenerationId::from([3; 32]),
            }),
        )];
        let mut overlay_bytes = locality_bytes(&root, &overlay_facts)?;
        let overlay =
            PreparedLocality::prepare(&root, &overlay_facts)?.write(&mut overlay_bytes)?;
        assert_run(
            &GenerationView::new(&root, &resident)?,
            required,
            ObjectProvenance::Resident,
        )?;
        assert_run(
            &GenerationView::new(&root, &overlay)?,
            required,
            ObjectProvenance::Overlay,
        )?;
        Ok(())
    }

    fn assert_run(
        view: &GenerationView<'_, '_, ObjectDomain>,
        required: ObjectRef<ObjectDomain>,
        provenance: ObjectProvenance,
    ) -> Result<(), TestError> {
        let provider = LocalObjectProvider::from_view(view, EntryKey::from(1))?.ok_or(
            TestError::MissingProvider {
                step: TestStep::ComposeProvider,
            },
        )?;
        let mut run = provider.start(PinnedObjectRequest {
            generation: view.id,
            required,
        })?;
        match run.next_batch()? {
            SourcePoll::Batch(batch) => assert_eq!(
                (batch.generation, batch.provenance, *batch.item),
                (view.id, provenance, required)
            ),
            SourcePoll::Terminal(TerminalSummary::Complete { .. }) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::FirstPoll,
                    observed: ObservedPoll::CompleteTerminal,
                });
            }
            SourcePoll::Terminal(TerminalSummary::Partial { .. }) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::FirstPoll,
                    observed: ObservedPoll::PartialTerminal,
                });
            }
            SourcePoll::Terminal(TerminalSummary::Cancelled { .. }) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::FirstPoll,
                    observed: ObservedPoll::CancelledTerminal,
                });
            }
            SourcePoll::Finished => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::FirstPoll,
                    observed: ObservedPoll::Finished,
                });
            }
        }
        assert_eq!(
            run.next_batch(),
            Ok(SourcePoll::Terminal(TerminalSummary::Complete {
                emitted: 1,
            }))
        );
        assert_eq!(run.next_batch(), Ok(SourcePoll::Finished));
        Ok(())
    }

    #[test]
    fn promised_locality_is_partial_and_request_mismatches_fail() -> Result<(), TestError> {
        let required = object(7);
        let root = root(required)?;
        let providers = ProviderSet::only(ProviderId::try_from(1)?);
        let promised_row =
            root.locality_row(EntryKey::from(1))
                .ok_or(TestError::MissingProvider {
                    step: TestStep::ComposeProvider,
                })?;
        let facts = [LocalityException::new(
            promised_row,
            NonResident::Promised(providers),
        )];
        let mut locality_bytes = locality_bytes(&root, &facts)?;
        let locality = PreparedLocality::prepare(&root, &facts)?.write(&mut locality_bytes)?;
        let view = GenerationView::new(&root, &locality)?;
        let provider = LocalObjectProvider::from_view(&view, EntryKey::from(1))?.ok_or(
            TestError::MissingProvider {
                step: TestStep::ComposeProvider,
            },
        )?;
        assert_request_mismatches(&provider, root.id, required)?;
        assert_promised_terminal(&provider, root.id, required, providers)
    }

    fn assert_request_mismatches(
        provider: &LocalObjectProvider<ObjectDomain>,
        generation: GenerationId,
        required: ObjectRef<ObjectDomain>,
    ) -> Result<(), TestError> {
        assert_eq!(
            start_error(&provider.start(PinnedObjectRequest {
                generation: GenerationId::from([8; 32]),
                required
            }))?,
            LocalObjectError::StaleGeneration {
                expected: generation,
                observed: GenerationId::from([8; 32]),
            }
        );
        assert_eq!(
            start_error(&provider.start(PinnedObjectRequest {
                generation,
                required: object(8)
            }))?,
            LocalObjectError::RequirementMismatch {
                expected: required,
                observed: object(8),
            }
        );
        Ok(())
    }

    fn start_error(
        result: &Result<super::LocalObjectRun<ObjectDomain>, LocalObjectError<ObjectDomain>>,
    ) -> Result<LocalObjectError<ObjectDomain>, TestError> {
        match result {
            Err(error) => Ok(*error),
            Ok(_) => Err(TestError::UnexpectedStartSuccess),
        }
    }

    fn assert_promised_terminal(
        provider: &LocalObjectProvider<ObjectDomain>,
        generation: GenerationId,
        required: ObjectRef<ObjectDomain>,
        providers: ProviderSet,
    ) -> Result<(), TestError> {
        let mut run = provider.start(PinnedObjectRequest {
            generation,
            required,
        })?;
        match run.next_batch()? {
            SourcePoll::Terminal(TerminalSummary::Partial { emitted, missing }) => {
                assert_eq!(emitted, 0);
                assert_eq!(
                    missing,
                    super::MissingObject {
                        required,
                        providers,
                    }
                );
            }
            SourcePoll::Batch(_) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::PromisedTerminal,
                    observed: ObservedPoll::Batch,
                });
            }
            SourcePoll::Terminal(TerminalSummary::Complete { .. }) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::PromisedTerminal,
                    observed: ObservedPoll::CompleteTerminal,
                });
            }
            SourcePoll::Terminal(TerminalSummary::Cancelled { .. }) => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::PromisedTerminal,
                    observed: ObservedPoll::CancelledTerminal,
                });
            }
            SourcePoll::Finished => {
                return Err(TestError::UnexpectedPoll {
                    step: TestStep::PromisedTerminal,
                    observed: ObservedPoll::Finished,
                });
            }
        }
        assert_eq!(run.next_batch(), Ok(SourcePoll::Finished));
        Ok(())
    }
}
/// One typed request pinned to a real immutable generation and descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedObjectRequest<ObjectDomain> {
    pub generation: GenerationId,
    pub required: ObjectRef<ObjectDomain>,
}
/// Exact partial coverage for the one required object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissingObject<ObjectDomain> {
    pub required: ObjectRef<ObjectDomain>,
    pub providers: ProviderSet,
}
/// Static Wave 1 operation over actual generation/object vocabulary.
pub struct PinnedObjectOperation<ObjectDomain>(PhantomData<fn() -> ObjectDomain>);
impl<ObjectDomain> Operation for PinnedObjectOperation<ObjectDomain> {
    type Request = PinnedObjectRequest<ObjectDomain>;
    type Item = ObjectRef<ObjectDomain>;
    type Missing = MissingObject<ObjectDomain>;
    type StartError = LocalObjectError<ObjectDomain>;
    type SourceError = Infallible;
    const ID: OperationId = OperationId::PinnedObject;
}
/// Request/provider incompatibility.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalObjectError<ObjectDomain> {
    #[error("request generation {observed:?} differs from provider generation {expected:?}")]
    StaleGeneration {
        expected: GenerationId,
        observed: GenerationId,
    },
    #[error("request object {observed:?} differs from root-witnessed object {expected:?}")]
    RequirementMismatch {
        expected: ObjectRef<ObjectDomain>,
        observed: ObjectRef<ObjectDomain>,
    },
}
/// One row whose common provenance and generation are hoisted once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectBatch<'source, ObjectDomain> {
    pub generation: GenerationId,
    pub provenance: ObjectProvenance,
    pub item: &'source ObjectRef<ObjectDomain>,
}
impl<ObjectDomain> Deref for ObjectBatch<'_, ObjectDomain> {
    type Target = [ObjectRef<ObjectDomain>];
    fn deref(&self) -> &Self::Target {
        core::slice::from_ref(self.item)
    }
}
impl<ObjectDomain> AsRef<[ObjectRef<ObjectDomain>]> for ObjectBatch<'_, ObjectDomain> {
    fn as_ref(&self) -> &[ObjectRef<ObjectDomain>] {
        self
    }
}

/// Provider created only by a checked composition of semantic root and locality evidence.
pub struct LocalObjectProvider<ObjectDomain> {
    generation: GenerationId,
    entry: GenerationEntry<ObjectDomain>,
}
impl<ObjectDomain> LocalObjectProvider<ObjectDomain> {
    /// Selects an actual entry from a generation-coherent view.
    ///
    /// # Errors
    ///
    /// Returns the exact locality lane read failure before constructing a provider.
    pub fn from_view(
        view: &GenerationView<'_, '_, ObjectDomain>,
        key: EntryKey,
    ) -> Result<Option<Self>, LocalityReadError> {
        Ok(view.get(key)?.map(|entry| Self {
            generation: view.id,
            entry,
        }))
    }
}
impl<ObjectDomain> Provider<PinnedObjectOperation<ObjectDomain>>
    for LocalObjectProvider<ObjectDomain>
{
    type Run = LocalObjectRun<ObjectDomain>;
    fn start(
        &self,
        request: PinnedObjectRequest<ObjectDomain>,
    ) -> Result<Self::Run, LocalObjectError<ObjectDomain>> {
        if request.generation != self.generation {
            return Err(LocalObjectError::StaleGeneration {
                expected: self.generation,
                observed: request.generation,
            });
        }
        if request.required != self.entry.object {
            return Err(LocalObjectError::RequirementMismatch {
                expected: self.entry.object,
                observed: request.required,
            });
        }
        let phase = match self.entry.locality {
            Locality::Resident => RunPhase::ReadyBatch(ObjectProvenance::Resident),
            Locality::Overlaid(_) => RunPhase::ReadyBatch(ObjectProvenance::Overlay),
            Locality::Promised(providers) => RunPhase::PromisedTerminal(providers),
        };
        Ok(LocalObjectRun {
            generation: self.generation,
            object: self.entry.object,
            phase,
        })
    }
}

/// Coherent cursor phase: valid transitions are encoded as closed variants, never booleans/options.
enum RunPhase {
    ReadyBatch(ObjectProvenance),
    ReadyTerminal,
    PromisedTerminal(ProviderSet),
    Finished,
}
/// Uniquely owned stateful cursor. It is intentionally neither `Copy` nor `Clone`.
pub struct LocalObjectRun<ObjectDomain> {
    generation: GenerationId,
    object: ObjectRef<ObjectDomain>,
    phase: RunPhase,
}
impl<ObjectDomain> BatchSource<PinnedObjectOperation<ObjectDomain>>
    for LocalObjectRun<ObjectDomain>
{
    type Batch<'source>
        = ObjectBatch<'source, ObjectDomain>
    where
        Self: 'source;
    fn next_batch(
        &mut self,
    ) -> Result<SourcePoll<Self::Batch<'_>, MissingObject<ObjectDomain>>, Infallible> {
        match self.phase {
            RunPhase::ReadyBatch(provenance) => {
                self.phase = RunPhase::ReadyTerminal;
                Ok(SourcePoll::Batch(ObjectBatch {
                    generation: self.generation,
                    provenance,
                    item: &self.object,
                }))
            }
            RunPhase::ReadyTerminal => {
                self.phase = RunPhase::Finished;
                Ok(SourcePoll::Terminal(TerminalSummary::Complete {
                    emitted: 1,
                }))
            }
            RunPhase::PromisedTerminal(providers) => {
                self.phase = RunPhase::Finished;
                Ok(SourcePoll::Terminal(TerminalSummary::Partial {
                    emitted: 0,
                    missing: MissingObject {
                        required: self.object,
                        providers,
                    },
                }))
            }
            RunPhase::Finished => Ok(SourcePoll::Finished),
        }
    }
}
