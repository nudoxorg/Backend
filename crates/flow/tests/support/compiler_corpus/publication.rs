//! Durable publication, reopen, and fragment-envelope observation.
//!
//! Publication is kept as a separate invariant from native authority setup
//! and semantic comparison so a missing reader plane remains typed.

use super::observation::{observe_reopened_semantic, range_manifest_digest_from_manifest};
use super::*;

pub(super) struct PassPublisher {
    _root: FixtureDir,
    publisher: Option<DurablePublisher>,
    paths: PublicationPaths,
    limits: PublicationLimits,
    artifacts: PathBuf,
}
impl PassPublisher {
    pub(super) fn new(pass: Pass) -> Result<Self, CorpusAuditError> {
        let label = match pass {
            Pass::Original => "original",
            Pass::Reverse => "reverse",
            Pass::FixedShuffle => "fixed-shuffle",
        };
        let root = FixtureDir::new(label).map_err(|source| CorpusAuditError::Io {
            phase: AuditIoPhase::Fixture,
            source,
        })?;
        let journal = root.child("journal");
        let artifacts = root.child("artifacts");
        let paths = PublicationPaths::in_directory(&journal);
        let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?;
        let publisher = DurablePublisher::create(&paths, limits)?;
        Ok(Self {
            _root: root,
            publisher: Some(publisher),
            paths,
            limits,
            artifacts,
        })
    }

    /// Publishes one compiled batch, then lends the validated reopened
    /// fragment and complete semantic reader to a caller-owned observer while
    /// their scratch leases are alive.  Returning only observations from the
    /// callback keeps this helper free of self-referential reader values.
    pub(super) fn publish_with_reader<T>(
        &mut self,
        compiled: &backend_engine::driver::CompiledSemantic<'_>,
        inspect: impl FnOnce(
            &backend_engine::publication::OpenedFragment<'_>,
            &backend_semantic::ir::SemanticImageView<'_>,
        ) -> Result<T, CorpusAuditError>,
    ) -> Result<T, CorpusAuditError> {
        let mut manifest = vec![0_u8; MANIFEST_BYTES];
        let mut manifest_facts = [None; 1];
        let mut ordinals = [0_usize; 1];
        let mut semantic_image_plan = [SemanticImageRegion::EMPTY; 1];
        let mut semantic_image_output = vec![0_u8; SEMANTIC_IMAGE_BYTES];
        let mut locality = vec![0_u8; LOCALITY_BYTES];
        let mut binding = vec![0_u8; backend_engine::publication::binding::COMPILATION_BINDING_BYTES];
        let publisher = self.publisher.as_ref().ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::PublisherClosed,
        })?;
        publish_semantic(
            publisher,
            &self.artifacts,
            core::slice::from_ref(compiled),
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut manifest_facts,
                ordinals: &mut ordinals,
                semantic_image_plan: &mut semantic_image_plan,
                semantic_image_output: &mut semantic_image_output,
                locality_output: &mut locality,
                binding_output: &mut binding,
            },
        )?;

        // `DurablePublisher::shutdown` consumes its owner.  Reopen a fresh
        // journal owner before reading so this audit proves cold durability,
        // not merely an in-process snapshot of the publishing owner.
        let publisher = self.publisher.take().ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::PublisherClosed,
        })?;
        publisher.shutdown()?;
        self.publisher = Some(DurablePublisher::reopen(&self.paths, self.limits)?);

        let mut reopened_manifest = vec![0_u8; MANIFEST_BYTES];
        let mut reopened_facts = [None; 1];
        let mut reopened_fragments = vec![0_u8; FRAGMENT_BYTES];
        let mut reopened_semantic_images = vec![0_u8; SEMANTIC_IMAGE_BYTES];
        let mut reopened_locality = vec![0_u8; LOCALITY_BYTES];
        let opened = open_published_semantic(
            self.publisher.as_ref().ok_or(CorpusAuditError::Invariant {
                key: None,
                cause: CorpusInvariant::PublisherClosed,
            })?,
            &self.artifacts,
            OpenSemanticPublicationScratch {
                manifest_output: &mut reopened_manifest,
                manifest_facts: &mut reopened_facts,
                fragment_output: &mut reopened_fragments,
                semantic_image_output: &mut reopened_semantic_images,
                locality_output: &mut reopened_locality,
            },
        )?
        .ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingPublishedFragment,
        })?;
        let mut artifacts = opened.artifacts();
        let artifact = artifacts.next().ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingPublishedFragment,
        })??;
        if artifacts.next().is_some() {
            return Err(CorpusAuditError::Invariant {
                key: None,
                cause: CorpusInvariant::ExtraPublishedFragment,
            });
        }
        inspect(&artifact.fragment, &artifact.semantic_image)
    }

    pub(super) fn publish(
        &mut self,
        compiled: &backend_engine::driver::CompiledSemantic<'_>,
        primary: Option<EntityId>,
    ) -> Result<ReopenedObservation, CorpusAuditError> {
        self.publish_with_reader(compiled, |fragment, semantic_image| {
            let ranges = FragmentRangeManifest::from_view(&fragment.view)?;
            let semantic = observe_reopened_semantic(semantic_image, primary);
            Ok(ReopenedObservation {
                fragment: digest_bytes(fragment.view.as_ref()),
                source: fragment.facts.source,
                recipe: fragment.facts.recipe,
                ranges: range_manifest_digest_from_manifest(ranges),
                census: fragment.view.discover().census(),
                semantic_data: PlaneObservation::Captured,
                occurrences: PlaneObservation::Captured,
                type_facts: PlaneObservation::Captured,
                documentation: PlaneObservation::Captured,
                extensions: PlaneObservation::Captured,
                semantic,
            })
        })
    }

    pub(super) fn finish(mut self) -> Result<(), CorpusAuditError> {
        if let Some(publisher) = self.publisher.take() {
            publisher.shutdown()?;
        }
        Ok(())
    }
}
