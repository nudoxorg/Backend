//! Thin wrapper: walk once, return sections.
//!
//! All the logic lives in [`super::walk::walk_doc`].  This module exposes
//! a named public function so callers and tests can import `sections::sections`
//! without knowing about the shared walk.

use nudox_ir::{change::IntroId, entry::Entry, view::IrView};
use nudox_store::package::PackageView;

use crate::wire::RenderSection;

/// Return the rendered sections for one entry.
///
/// Calls `walk::walk_doc` once and returns `output.sections`.
/// The plan is discarded; use [`super::plan::section_plan`] if you need both.
pub fn sections(
    intro: IntroId,
    entry: &Entry,
    view: &IrView,
    package: &PackageView,
) -> Vec<RenderSection> {
    super::walk::walk_doc(intro, entry, view, package).sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_store::{
        package::{PackageView, Provenance},
        source::fixtures::build_rich_view,
    };

    #[test]
    fn sections_non_empty_for_module_with_children() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        // At least one entry should produce at least one section.
        let any_sections = pkg
            .view()
            .entries()
            .any(|(intro, entry)| !sections(intro, entry, pkg.view(), &pkg).is_empty());
        assert!(any_sections, "expected at least one entry with sections");
    }

    #[test]
    fn sections_ids_are_monotone() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let sects = sections(intro, entry, pkg.view(), &pkg);
            for window in sects.windows(2) {
                assert!(
                    window[0].section_id() < window[1].section_id(),
                    "sections ids must be strictly monotone for '{}'",
                    entry.sym().name
                );
            }
        }
    }
}
