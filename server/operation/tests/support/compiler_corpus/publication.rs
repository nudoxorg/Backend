//! Durable publication, reopen, and fragment-envelope observation.
//!
//! Publication is kept as a separate invariant from native authority setup
//! and semantic comparison so a missing reader plane remains typed.

use super::*;

pub(super) struct PassPublisher {
    _root: FixtureDir,
    publisher: DurablePublisher,
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
            publisher,
            artifacts,
        })
    }

    pub(super) fn publish(
        &mut self,
        compiled: &compiler_driver::CompiledFragment<'_>,
    ) -> Result<ReopenedObservation, CorpusAuditError> {
        let mut manifest = vec![0_u8; MANIFEST_BYTES];
        let mut manifest_facts = [None; 1];
        let mut ordinals = [0_usize; 1];
        let mut locality = vec![0_u8; LOCALITY_BYTES];
        let mut binding = vec![0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
        publish_compiled(
            &self.publisher,
            &self.artifacts,
            core::slice::from_ref(compiled),
            PublishControl::Continue,
            PublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut manifest_facts,
                ordinals: &mut ordinals,
                locality_output: &mut locality,
                binding_output: &mut binding,
            },
        )?;

        let mut reopened_manifest = vec![0_u8; MANIFEST_BYTES];
        let mut reopened_facts = [None; 1];
        let mut reopened_fragments = vec![0_u8; FRAGMENT_BYTES];
        let mut reopened_locality = vec![0_u8; LOCALITY_BYTES];
        let opened = open_published(
            &self.publisher,
            &self.artifacts,
            OpenPublicationScratch {
                manifest_output: &mut reopened_manifest,
                manifest_facts: &mut reopened_facts,
                fragment_output: &mut reopened_fragments,
                locality_output: &mut reopened_locality,
            },
        )?
        .ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingPublishedFragment,
        })?;
        let mut fragments = opened.fragments();
        let fragment = fragments
            .next()
            .ok_or(CorpusAuditError::Invariant {
                key: None,
                cause: CorpusInvariant::MissingPublishedFragment,
            })??;
        if fragments.next().is_some() {
            return Err(CorpusAuditError::Invariant {
                key: None,
                cause: CorpusInvariant::ExtraPublishedFragment,
            });
        }
        let ranges = FragmentRangeManifest::from_view(&fragment.view)?;
        let semantic_planes = observe_reopened_semantic_image(&fragment.view);
        let reopened = ReopenedObservation {
            fragment: digest_bytes(fragment.view.as_ref()),
            source: fragment.facts.source,
            recipe: fragment.facts.recipe,
            ranges: range_manifest_digest_from_manifest(ranges),
            census: fragment.view.discover().census(),
            semantic_data: semantic_planes.semantic_data,
            occurrences: semantic_planes.occurrences,
            type_facts: semantic_planes.type_facts,
            documentation: semantic_planes.documentation,
            extensions: semantic_planes.extensions,
        };
        Ok(reopened)
    }

    pub(super) fn finish(self) -> Result<(), CorpusAuditError> {
        self.publisher.shutdown()?;
        Ok(())
    }
}
