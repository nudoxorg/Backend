//! Shared one-walk core for `sections` and `plan`.
//!
//! # Why a shared walk?
//!
//! `plan::section_plan` and `sections::sections` must agree on section count,
//! ids, and order by construction — not by convention.  The `WalkOutput` type
//! and `walk_doc` function are the mechanism: both callers call `walk_doc` and
//! destructure whichever field they need.  The `SectionId` counter is
//! incremented once, inside this function, so the two outputs are
//! structurally identical.

use std::sync::Arc;

use nudox_ir::{change::IntroId, entry::Entry, kind::Kind, view::IrView};
use nudox_store::package::PackageView;

use crate::wire::{
    FieldRow, KindTag, MemberRow, RenderSection, SectionId, SectionKind, SectionPlan, SharedStr,
    SizeHint,
};

// ── Submodules ────────────────────────────────────────────────────────────

pub(crate) mod doc_link_table;
pub(crate) mod parse;
pub(crate) mod prose;

// Re-export items needed by sibling modules.
pub(crate) use doc_link_table::DocLinkTable;
pub(crate) use parse::parse_markdown;

// ── Output type ───────────────────────────────────────────────────────────

/// The combined output of one documentation walk.
pub(crate) struct WalkOutput {
    pub sections: Vec<RenderSection>,
    pub plan: Vec<SectionPlan>,
}

// ── walk_doc ──────────────────────────────────────────────────────────────

/// Parse an entry's documentation and enumerate its children in one pass,
/// producing both rendered sections and their plan simultaneously.
pub(crate) fn walk_doc(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
) -> WalkOutput {
    let mut id_counter: u32 = 0;

    let mut sections: Vec<RenderSection> = Vec::new();
    let mut plan: Vec<SectionPlan> = Vec::new();

    // ── 1. Parse markdown documentation ────────────────────────────────────

    let doc = entry.sym().documentation.as_str();
    if !doc.is_empty() {
        let doc_link_table = DocLinkTable::build(entry, package);
        parse_markdown(
            doc,
            &doc_link_table,
            &mut id_counter,
            &mut sections,
            &mut plan,
        );
    }

    // ── 2. Members section ─────────────────────────────────────────────────

    let children = view.children_of(intro);

    let member_children: Vec<(IntroId, MemberRow)> = children
        .iter()
        .filter_map(|&child_intro| {
            let child = view.entry(child_intro)?;
            let disc = child.kind().discriminant()?;
            let is_member = matches!(
                child.kind().as_owned_kind(),
                Some(Kind::Module(_) | Kind::Function(_) | Kind::Trait(_) | Kind::Impl(_))
            );
            if !is_member {
                return None;
            }
            let key = nudox_ir::change::StableRef::new(package.lineage().clone(), child_intro);
            let sig = crate::chunk::signature::tokens(child, package);
            Some((
                child_intro,
                MemberRow {
                    key,
                    name: SharedStr::from(child.sym().name.as_str()),
                    sig,
                    kind: KindTag::Known(disc),
                    visibility: child.sym().visibility,
                },
            ))
        })
        .collect();

    if !member_children.is_empty() {
        id_counter += 1;
        let id = SectionId(id_counter);
        let row_count = member_children.len() as u32;
        let entries: Arc<[MemberRow]> = member_children
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .into();
        sections.push(RenderSection::Members { id, entries });
        plan.push(SectionPlan {
            id,
            kind: SectionKind::Members,
            size_hint: SizeHint::Rows(row_count),
        });
    }

    // ── 3. Fields section ──────────────────────────────────────────────────

    let field_children: Vec<FieldRow> = children
        .iter()
        .filter_map(|&child_intro| {
            let child = view.entry(child_intro)?;
            let disc = child.kind().discriminant()?;
            let is_field = matches!(
                child.kind().as_owned_kind(),
                Some(Kind::Field(_) | Kind::Variant(_))
            );
            if !is_field {
                return None;
            }
            let key = nudox_ir::change::StableRef::new(package.lineage().clone(), child_intro);
            let ty_tokens = crate::chunk::signature::tokens(child, package);
            Some(FieldRow {
                key,
                name: SharedStr::from(child.sym().name.as_str()),
                ty_tokens,
                kind: KindTag::Known(disc),
            })
        })
        .collect();

    if !field_children.is_empty() {
        id_counter += 1;
        let id = SectionId(id_counter);
        let row_count = field_children.len() as u32;
        let entries: Arc<[FieldRow]> = field_children.into();
        sections.push(RenderSection::Fields { id, entries });
        plan.push(SectionPlan {
            id,
            kind: SectionKind::Fields,
            size_hint: SizeHint::Rows(row_count),
        });
    }

    WalkOutput { sections, plan }
}

// ── Unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
