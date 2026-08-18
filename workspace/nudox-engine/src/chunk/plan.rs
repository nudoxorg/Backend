//! Thin wrapper: walk once, return the section plan.
//!
//! All the logic lives in [`super::walk::walk_doc`].  This module exposes
//! a named public function so callers and tests can import `plan::section_plan`
//! without knowing about the shared walk.

use crate::store::package::PackageView;
use nudox_ir::{change::IntroId, entry::Entry, view::IrView};

use crate::wire::SectionPlan;

/// Return the section plan for one entry.
///
/// Calls `walk::walk_doc` once and returns `output.plan`.
/// The rendered sections are discarded; use [`super::sections::sections`] if
/// you need both, or [`super::chunk`] for the full combined output.
pub fn section_plan(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
) -> Vec<SectionPlan> {
    super::walk::walk_doc(intro, entry, view, package).plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        package::{PackageView, Provenance},
        source::fixtures::build_rich_view,
    };

    #[test]
    fn plan_ids_match_section_ids() {
        use super::super::sections::sections;

        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let plan = section_plan(intro, entry, pkg.view(), &pkg);
            let sects = sections(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                plan.len(),
                sects.len(),
                "plan/section count mismatch for '{}'",
                entry.sym().name
            );
            for (p, s) in plan.iter().zip(sects.iter()) {
                assert_eq!(
                    p.id,
                    s.section_id(),
                    "id mismatch at {:?} for '{}'",
                    p.id,
                    entry.sym().name
                );
            }
        }
    }

    #[test]
    fn plan_size_hints_are_non_zero() {
        use crate::wire::SizeHint;

        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            for p in section_plan(intro, entry, pkg.view(), &pkg) {
                match p.size_hint {
                    SizeHint::Lines(n) => {
                        assert!(n > 0, "zero line count in plan for '{}'", entry.sym().name);
                    }
                    SizeHint::Rows(n) => {
                        assert!(n > 0, "zero row count in plan for '{}'", entry.sym().name);
                    }
                    SizeHint::Unknown => {}
                }
            }
        }
    }
}
