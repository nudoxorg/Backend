//! Canonical terminal-only pooled lists.
//!
//! Entity membership and documentation are not type dependency vertices. They
//! have their own framed pools so an empty member list, an empty doc list, and
//! an empty type-owned list can never alias merely because each has no items.

use alloc::vec::Vec;

use crate::{ArenaRange, AtomId, DocFragment, DocId, EntityListId, Ir, LinkTarget, TextId};

use super::super::{canonical::CanonicalFullPlan, fault::CoreSemanticImageFault};
use super::model::{TerminalPoolDomain, TerminalPoolFault, TerminalPoolPlan};

pub(super) struct TerminalPools {
    pub(super) members: TerminalPoolPlan,
    pub(super) docs: TerminalPoolPlan,
}

impl TerminalPools {
    pub(super) fn build(
        ir: &Ir,
        canonical: &CanonicalFullPlan<'_>,
    ) -> Result<Self, CoreTerminalError> {
        Ok(Self {
            members: build_members(ir, canonical)?,
            docs: build_docs(ir, canonical)?,
        })
    }

    pub(super) fn member(&self, id: EntityListId) -> Result<u32, TerminalPoolFault> {
        self.members.canonical(id.raw, TerminalPoolDomain::EntityList)
    }

    pub(super) fn docs(&self, id: DocId) -> Result<u32, TerminalPoolFault> {
        self.docs.canonical(id.raw, TerminalPoolDomain::Documentation)
    }
}

/// List construction can retain either an exact terminal-pool or common-plan
/// reference cause without pretending they share the same invariant.
#[derive(Debug)]
pub(super) enum CoreTerminalError {
    Core(CoreSemanticImageFault),
    Terminal(TerminalPoolFault),
}

impl From<CoreSemanticImageFault> for CoreTerminalError {
    fn from(value: CoreSemanticImageFault) -> Self { Self::Core(value) }
}
impl From<TerminalPoolFault> for CoreTerminalError {
    fn from(value: TerminalPoolFault) -> Self { Self::Terminal(value) }
}

fn build_members(
    ir: &Ir,
    canonical: &CanonicalFullPlan<'_>,
) -> Result<TerminalPoolPlan, CoreTerminalError> {
    let count = ir.storage_columns().entity_lists.ranges.len();
    let mut bytes = Vec::new();
    let mut ranges = Vec::with_capacity(count);
    for raw in 0..count {
        let row = u32::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
            domain: TerminalPoolDomain::EntityList,
            rows: count,
        })?;
        let members = ir.entity_list(EntityListId::new(row)).ok_or(TerminalPoolFault::MissingRow {
            domain: TerminalPoolDomain::EntityList,
            row,
            count: u32::try_from(count).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain: TerminalPoolDomain::EntityList,
                rows: count,
            })?,
        })?;
        let start = bytes.len();
        bytes.push(member_domain_tag());
        write_len(&mut bytes, members.len(), TerminalPoolDomain::EntityList, row)?;
        for member in members.iter().copied() {
            bytes.extend_from_slice(&canonical.entity(member)?.to_le_bytes());
        }
        push_range(&mut ranges, start, bytes.len(), TerminalPoolDomain::EntityList, row)?;
    }
    TerminalPoolPlan::from_key_arena(TerminalPoolDomain::EntityList, bytes, ranges).map_err(Into::into)
}

fn build_docs(
    ir: &Ir,
    canonical: &CanonicalFullPlan<'_>,
) -> Result<TerminalPoolPlan, CoreTerminalError> {
    let count = ir.storage_columns().docs.ranges.len();
    let mut bytes = Vec::new();
    let mut ranges = Vec::with_capacity(count);
    for raw in 0..count {
        let row = u32::try_from(raw).map_err(|_| TerminalPoolFault::GeometryOverflow {
            domain: TerminalPoolDomain::Documentation,
            rows: count,
        })?;
        let docs = ir.documentation(DocId::new(row)).ok_or(TerminalPoolFault::MissingRow {
            domain: TerminalPoolDomain::Documentation,
            row,
            count: u32::try_from(count).map_err(|_| TerminalPoolFault::GeometryOverflow {
                domain: TerminalPoolDomain::Documentation,
                rows: count,
            })?,
        })?;
        let start = bytes.len();
        bytes.push(documentation_domain_tag());
        write_len(&mut bytes, docs.len(), TerminalPoolDomain::Documentation, row)?;
        for fragment in docs.iter().copied() {
            append_doc_fragment(&mut bytes, fragment, canonical)?;
        }
        push_range(&mut ranges, start, bytes.len(), TerminalPoolDomain::Documentation, row)?;
    }
    TerminalPoolPlan::from_key_arena(TerminalPoolDomain::Documentation, bytes, ranges).map_err(Into::into)
}

fn append_doc_fragment(
    out: &mut Vec<u8>,
    fragment: DocFragment,
    canonical: &CanonicalFullPlan<'_>,
) -> Result<(), CoreTerminalError> {
    match fragment {
        DocFragment::Text(text) => {
            out.push(doc_tag_text());
            append_text(out, text, canonical)?;
        }
        DocFragment::Code(text) => {
            out.push(doc_tag_code());
            append_text(out, text, canonical)?;
        }
        DocFragment::Link { label, target } => {
            out.push(doc_tag_link());
            append_text(out, label, canonical)?;
            match target {
                LinkTarget::Local(entity) => {
                    out.push(link_target_local_tag());
                    out.extend_from_slice(&canonical.entity(entity)?.to_le_bytes());
                }
                LinkTarget::External(external) => {
                    out.push(link_target_external_tag());
                    out.extend_from_slice(&canonical.external(external)?.to_le_bytes());
                }
            }
        }
        DocFragment::SoftBreak => out.push(doc_tag_soft_break()),
        DocFragment::HardBreak => out.push(doc_tag_hard_break()),
    }
    Ok(())
}

fn append_text(
    out: &mut Vec<u8>,
    text: TextId,
    canonical: &CanonicalFullPlan<'_>,
) -> Result<(), CoreTerminalError> {
    // `TextId` is a UTF-8 proof over the same universal atom arena. Its raw
    // coordinate is deliberately converted to the atom remap before it is
    // framed into a docs key.
    out.extend_from_slice(&canonical.atom(AtomId::new(text.raw))?.to_le_bytes());
    Ok(())
}

fn push_range(
    ranges: &mut Vec<ArenaRange>,
    start: usize,
    end: usize,
    domain: TerminalPoolDomain,
    row: u32,
) -> Result<(), TerminalPoolFault> {
    let length = end.checked_sub(start).ok_or(TerminalPoolFault::KeyLengthOverflow { domain, row })?;
    ranges.push(ArenaRange {
        start: u32::try_from(start).map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row })?,
        len: u32::try_from(length).map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row })?,
    });
    Ok(())
}

fn write_len(
    out: &mut Vec<u8>,
    length: usize,
    domain: TerminalPoolDomain,
    row: u32,
) -> Result<(), TerminalPoolFault> {
    out.extend_from_slice(&u32::try_from(length)
        .map_err(|_| TerminalPoolFault::KeyLengthOverflow { domain, row })?
        .to_le_bytes());
    Ok(())
}

const fn member_domain_tag() -> u8 { 0 }
const fn documentation_domain_tag() -> u8 { 1 }
const fn doc_tag_text() -> u8 { 0 }
const fn doc_tag_code() -> u8 { 1 }
const fn doc_tag_link() -> u8 { 2 }
const fn doc_tag_soft_break() -> u8 { 3 }
const fn doc_tag_hard_break() -> u8 { 4 }
const fn link_target_local_tag() -> u8 { 0 }
const fn link_target_external_tag() -> u8 { 1 }
