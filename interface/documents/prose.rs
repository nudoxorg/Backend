//! Defines prose behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the prose invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Reassembling documentation blocks from the image's flat run of inline fragments.

use compiler_ir::{DocFragment, DocId, EntityId, SemanticReader};

use crate::{
    Block, ByteBudget, Inline, ProjectionError, Projector, Prose, Target, Text,
};

/// Rebuilds one entity's documentation into blocks.
///
/// The image keeps documentation as a flat run of inline fragments with no block structure at all,
/// so blocks are reconstructed here: a hard break closes a paragraph, a soft break is retained as
/// an inline break, and a code fragment that spans lines becomes its own fenced block while a
/// single-line one stays inline. Nothing is invented; a fragment the image did not retain does not
/// appear.
///
/// # Errors
///
/// Returns [`ProjectionError::ProseBudget`] with both operands when the retained documentation
/// exceeds the caller's byte budget. That is a corruption-class event rather than a large page, so
/// it fails instead of silently truncating.
pub(crate) fn project_prose<Reader: SemanticReader + ?Sized>(
    projector: &Projector<'_, Reader>,
    entity: EntityId,
    docs: DocId,
    budget: ByteBudget,
) -> Result<Prose, ProjectionError> {
    let Some(fragments) = projector.reader.docs(docs) else {
        return Ok(Prose::default());
    };
    let mut blocks = Vec::new();
    let mut paragraph: Vec<Inline> = Vec::new();
    let mut retained = 0_usize;
    for fragment in fragments {
        match fragment {
            DocFragment::Text(text) => {
                let run = projector.reader.text(text).unwrap_or_default();
                retained = retained.saturating_add(run.len());
                paragraph.push(Inline::Text(Text::new(run)));
            }
            DocFragment::Code(text) => {
                let run = projector.reader.text(text).unwrap_or_default();
                retained = retained.saturating_add(run.len());
                if run.contains('\n') {
                    flush(&mut blocks, &mut paragraph);
                    blocks.push(Block::Code(Text::new(run)));
                } else {
                    paragraph.push(Inline::Code(Text::new(run)));
                }
            }
            DocFragment::Link { label, target } => {
                let run = projector.reader.text(label).unwrap_or_default();
                retained = retained.saturating_add(run.len());
                let resolved = projector
                    .target(target)
                    .unwrap_or_else(|| Target::Unresolved(Text::new(run)));
                paragraph.push(Inline::Link {
                    label: Text::new(run),
                    target: resolved,
                });
            }
            DocFragment::SoftBreak => paragraph.push(Inline::Break),
            DocFragment::HardBreak => flush(&mut blocks, &mut paragraph),
        }
        if retained > budget.get() {
            return Err(ProjectionError::ProseBudget {
                entity,
                required: ByteBudget(u32::try_from(retained).unwrap_or(u32::MAX)),
                maximum: budget,
            });
        }
    }
    flush(&mut blocks, &mut paragraph);
    Ok(Prose::new(blocks))
}

fn flush(blocks: &mut Vec<Block>, paragraph: &mut Vec<Inline>) {
    if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(core::mem::take(paragraph).into_boxed_slice()));
    }
}
