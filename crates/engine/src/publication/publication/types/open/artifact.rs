//! Paired semantic artifact cursor for one reopened publication.

use backend_semantic::ir::{SemanticImageIdentity, SemanticImageView};

use super::{
    OpenedSemanticArtifact, OpenedSemanticArtifactCursor, OpenedSemanticArtifactError,
    opened_fragment,
};
use crate::publication::manifest::StoredFragmentFacts;

impl<'fragment, 'semantic> Iterator for OpenedSemanticArtifactCursor<'_, 'fragment, 'semantic> {
    type Item = Result<OpenedSemanticArtifact<'fragment, 'semantic>, OpenedSemanticArtifactError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let facts = self.facts.next()?;
        let ordinal = self.ordinal;
        let Some(next_ordinal) = ordinal.checked_add(1) else {
            self.failed = true;
            return Some(Err(OpenedSemanticArtifactError::OrdinalOverflow { facts }));
        };
        self.ordinal = next_ordinal;
        match opened_semantic_artifact(
            self.fragment_bytes,
            self.semantic_bytes,
            self.fragment_offset,
            self.semantic_offset,
            ordinal,
            facts,
        ) {
            Ok((artifact, fragment_offset, semantic_offset)) => {
                self.fragment_offset = fragment_offset;
                self.semantic_offset = semantic_offset;
                Some(Ok(artifact))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl core::iter::FusedIterator for OpenedSemanticArtifactCursor<'_, '_, '_> {}

#[allow(
    clippy::result_large_err,
    reason = "cold paired-artifact failures retain exact manifest facts and typed grammar causes"
)]
fn opened_semantic_artifact<'fragment, 'semantic>(
    fragment_bytes: &'fragment [u8],
    semantic_bytes: &'semantic [u8],
    fragment_offset: usize,
    semantic_offset: usize,
    ordinal: usize,
    facts: StoredFragmentFacts,
) -> Result<(OpenedSemanticArtifact<'fragment, 'semantic>, usize, usize), OpenedSemanticArtifactError>
{
    let semantic_facts = facts
        .semantic_image
        .ok_or(OpenedSemanticArtifactError::MissingSemanticImage { ordinal, facts })?;
    let semantic_length = usize::try_from(semantic_facts.byte_length).map_err(|source| {
        OpenedSemanticArtifactError::LengthAddressSpace {
            ordinal,
            facts: semantic_facts,
            source,
        }
    })?;
    let semantic_end = semantic_offset.checked_add(semantic_length).ok_or(
        OpenedSemanticArtifactError::RangeOverflow {
            ordinal,
            facts: semantic_facts,
            offset: semantic_offset,
            length: semantic_length,
        },
    )?;
    let region = semantic_bytes.get(semantic_offset..semantic_end).ok_or(
        OpenedSemanticArtifactError::RegionTruncated {
            ordinal,
            facts: semantic_facts,
            available: semantic_bytes.len().saturating_sub(semantic_offset),
            required: semantic_length,
        },
    )?;
    let semantic_image = SemanticImageView::reopen(region).map_err(|source| {
        OpenedSemanticArtifactError::Grammar {
            ordinal,
            facts: semantic_facts,
            source,
        }
    })?;
    let observed = SemanticImageIdentity::from_encoded_bytes(region);
    if observed != semantic_facts.identity {
        return Err(OpenedSemanticArtifactError::Identity {
            ordinal,
            expected: semantic_facts.identity,
            observed,
        });
    }
    let (fragment, fragment_end) = opened_fragment(fragment_bytes, fragment_offset, ordinal, facts)
        .map_err(|source| OpenedSemanticArtifactError::Fragment { ordinal, source })?;
    Ok((
        OpenedSemanticArtifact {
            fragment,
            semantic_image,
        },
        fragment_end,
        semantic_end,
    ))
}
