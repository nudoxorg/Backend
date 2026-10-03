//! Owner-selected README content and its bounded, worker-prepared addresses.

use crate::model::document_identity::{DocumentIdentity, DocumentPaintIdentity};
use crate::model::local_package::ReadmeHeading;
use crate::model::pages::PackageRef;
use crate::model::pages::Stamp;
use crate::navigation::{CargoBrowseContext, CargoReadmeLinkAddress};
use backend_library::{CargoPackageReadmeAbsenceV1, CargoPackageReadmeOriginV1};
use std::sync::Arc;

mod navigation;
mod page;
use navigation::NavigationIndex;
pub(crate) use navigation::PreparationError;
pub(crate) use page::NavigationPage;

/// One exact package README selector, independent of its semantic dossier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CargoReadmeKey {
    /// Exact requested project and owner-returned root commitments.
    pub context: CargoBrowseContext,
    /// Full source-qualified package observed by Cargo.
    pub package: PackageRef,
}

/// A prepared navigation hint still requires the README's current lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoReadmeDestination {
    /// A bounded supported external URI, admitted only at user activation.
    External(Arc<str>),
    /// One exact heading in this Markdown body.
    Anchor(ReadmeHeading),
    /// Owner-relative file address retaining the original root scope.
    Source(CargoReadmeLinkAddress),
    /// The authored link has no supported address in the complete index.
    Unavailable(&'static str),
}

/// One bounded link row; its label does not establish target content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeLink {
    /// Bounded exact-origin endpoint identity; occurrence only separates
    /// identical authored destinations within the same immutable document.
    pub id: Arc<str>,
    /// Authored label from the exact Markdown AST.
    pub label: Arc<str>,
    /// Exact href supplied back to the owner for a file read.
    pub href: Arc<str>,
    /// Prepared address projection, never target-file evidence.
    pub destination: CargoReadmeDestination,
}

/// Current Markdown bytes and an exact owner origin, prepared once on a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeDocument {
    /// Exact package/project/selection/scope/content receipt.
    pub origin: CargoPackageReadmeOriginV1,
    /// Complete bounded UTF-8 Markdown returned by the owner.
    pub source: Arc<str>,
    identity: Arc<str>,
    document_identity: DocumentIdentity,
    navigation: Arc<NavigationIndex>,
}

/// A remembered focus ID resolved only against this exact immutable origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CargoReadmeFocus {
    Link(usize),
    Heading(usize),
}

impl CargoReadmeDocument {
    pub(crate) fn prepare(
        origin: CargoPackageReadmeOriginV1,
        source: Arc<str>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, PreparationError> {
        let document_identity = DocumentIdentity::readme(&origin);
        let navigation = NavigationIndex::prepare(&origin, document_identity, &source, cancelled)?;
        let identity = endpoint_identity(&document_identity, "document", "");
        Ok(Self {
            origin,
            source,
            identity,
            document_identity,
            navigation: Arc::new(navigation),
        })
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }
    pub(crate) fn link_count(&self) -> usize {
        self.navigation.link_count()
    }
    pub(crate) fn heading_count(&self) -> usize {
        self.navigation.heading_count()
    }
    pub(crate) fn link(&self, index: usize) -> Option<CargoReadmeLink> {
        self.navigation.link(index, &self.origin, None)
    }
    pub(crate) fn heading(&self, index: usize) -> Option<ReadmeHeading> {
        self.navigation.heading(index, None)
    }
    pub(crate) fn heading_id(&self, index: usize) -> Option<Arc<str>> {
        self.navigation.heading_id(index)
    }

    /// Transient pixel/address projections for an actual Reader visit. The
    /// caller must separately hold the current selected resource/action lease.
    pub(crate) fn paint(&self, place: u64, stamp: Stamp) -> CargoReadmePaint<'_> {
        CargoReadmePaint {
            document: self,
            identity: self.document_identity.for_visit(place, stamp),
        }
    }

    /// Actual prepared index storage for bounded result payload accounting.
    pub(crate) fn navigation_storage_bytes(&self) -> usize {
        self.navigation.storage_bytes()
    }

    /// An inline callback names an exact authored href. Its first occurrence
    /// is the native focus tie; all authored hrefs have a prepared lookup.
    pub(crate) fn inline_focus_id(&self, href: &str) -> Option<Arc<str>> {
        self.navigation.inline_focus_id(href)
    }

    /// Cheap native/pointer disclosure from the immutable prepared index.
    /// This never admits content or an action against a live owner.
    pub(crate) fn link_actionable(&self, href: &str) -> bool {
        self.navigation.link_actionable(href)
    }

    /// No fallback to an ordinal: a different origin or target has no match.
    pub(crate) fn restore_focus(&self, id: &str) -> Option<CargoReadmeFocus> {
        self.navigation.restore_focus(id)
    }

    pub(crate) fn destination(&self, href: &str) -> CargoReadmeDestination {
        self.navigation.destination(href, &self.origin, None)
    }
}

/// Shared scoped projections keep native heading actions and Markdown's
/// painted bounds on the same document/visit identity.
pub(crate) struct CargoReadmePaint<'a> {
    document: &'a CargoReadmeDocument,
    identity: DocumentPaintIdentity,
}

impl CargoReadmePaint<'_> {
    pub(crate) fn identity(&self) -> DocumentPaintIdentity {
        self.identity
    }
    pub(crate) fn heading(&self, index: usize) -> Option<ReadmeHeading> {
        self.document.navigation.heading(index, Some(self.identity))
    }
    pub(crate) fn link(&self, index: usize) -> Option<CargoReadmeLink> {
        self.document
            .navigation
            .link(index, &self.document.origin, Some(self.identity))
    }
    pub(crate) fn destination(&self, href: &str) -> CargoReadmeDestination {
        self.document
            .navigation
            .destination(href, &self.document.origin, Some(self.identity))
    }
}

fn endpoint_digest(origin: &DocumentIdentity, kind: &str, endpoint: &str) -> blake3::Hash {
    origin.endpoint(kind, endpoint)
}

fn endpoint_identity(origin: &DocumentIdentity, kind: &str, endpoint: &str) -> Arc<str> {
    Arc::from(format!(
        "cargo-readme-{kind}-{}",
        endpoint_digest(origin, kind, endpoint).to_hex()
    ))
}

/// README content and exact absence are separate completed owner observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CargoReadmeState {
    /// Complete Markdown and owner-held scope.
    Read(Arc<CargoReadmeDocument>),
    /// Exact manifest/default absence, never inferred from a file inventory.
    Absent(CargoPackageReadmeAbsenceV1),
}

/// One exact owner read, independent of semantic indexing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoReadmeModel {
    /// Exact source-qualified package returned by the owner.
    pub package: PackageRef,
    /// Full requested/effective root binding returned by this read.
    pub request_binding: backend_library::browse::ProjectTreeRequestBindingV1,
    /// Revalidated Cargo observation revision.
    pub source_revision: [u8; 32],
    /// Complete README or exact Cargo-selected absence.
    pub state: CargoReadmeState,
}

#[cfg(test)]
mod focus_tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;

    fn document(contents: &str) -> CargoReadmeDocument {
        let (_, _, mut result) = crate::runtime::cargo_readme_reads::tests::fixture();
        let backend_library::CargoPackageReadmeResultV1::Read { readme, .. } = &mut result else {
            panic!("fixture read");
        };
        readme.contents = contents.into();
        readme.content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
        let origin = CargoPackageReadmeOriginV1::from_result(&result).expect("exact origin");
        CargoReadmeDocument::prepare(origin, Arc::from(contents), &|| false)
            .expect("complete navigation index")
    }

    #[test]
    fn native_link_disclosure_uses_exact_prepared_authored_destinations() {
        let readme = document(
            "# Current\n\n[Docs](https://docs.rs/example) [File](src/lib.rs) [Here](#current) [Missing](#not-present) [Unsafe](javascript:alert)\n\n`[Code](src/fiction.rs)`",
        );
        for href in ["https://docs.rs/example", "src/lib.rs", "#current"] {
            assert!(readme.link_actionable(href));
        }
        for href in [
            "#not-present",
            "javascript:alert",
            "src/fiction.rs",
            "https://forged",
        ] {
            assert!(!readme.link_actionable(href));
        }
    }

    #[test]
    fn back_focus_matches_exact_endpoint_and_never_the_new_ordinal() {
        let old =
            document("# Guide\n\n[Old target](../src/old.rs#L7)\n\n[Other](../src/other.rs)\n");
        let remembered = old.link(0).expect("old endpoint").id;
        assert_eq!(
            old.restore_focus(&remembered),
            Some(CargoReadmeFocus::Link(0))
        );
        let repeated =
            CargoReadmeDocument::prepare(old.origin.clone(), Arc::clone(&old.source), &|| false)
                .expect("same document");
        assert_eq!(
            repeated.restore_focus(&remembered),
            Some(CargoReadmeFocus::Link(0)),
            "repaint of the same immutable read keeps the endpoint"
        );

        let new = document(
            "# Guide\n\n[New target](../src/new.rs#L7)\n\n[Old target](../src/old.rs#L7)\n",
        );
        assert_eq!(
            new.restore_focus(&remembered),
            None,
            "a remembered row cannot transfer to another origin or the replacement at ordinal zero"
        );
        assert_eq!(
            new.restore_focus(&old.heading_id(0).expect("old heading")),
            None,
            "even equal heading words require the same Markdown origin"
        );
        assert_eq!(
            new.restore_focus("cargo-readme-link-0"),
            None,
            "legacy ordinal IDs have no semantic fallback"
        );

        let presented = [
            old.link(1).expect("other endpoint"),
            old.link(0).expect("remembered endpoint"),
        ];
        assert_eq!(
            presented[1].id, remembered,
            "presentation order never defines endpoint identity"
        );
    }

    #[test]
    fn duplicate_destinations_only_use_occurrence_as_a_tie_break_and_ids_are_bounded() {
        let document = document(
            "# Guide\n\n[First](../src/lib.rs#L7)\n\n[Different](../src/other.rs)\n\n[Again](../src/lib.rs#L7)\n",
        );
        assert_eq!(
            document.link(0).expect("first link").href,
            document.link(2).expect("repeated link").href
        );
        assert_ne!(
            document.link(0).expect("first link").id,
            document.link(2).expect("repeated link").id
        );
        assert_eq!(
            document.restore_focus(&document.link(2).expect("repeated link").id),
            Some(CargoReadmeFocus::Link(2))
        );
        assert_eq!(
            document.inline_focus_id("../src/lib.rs#L7"),
            Some(document.link(0).expect("first link").id),
            "inline navigation remembers an exact native endpoint rather than the Markdown block ordinal"
        );
        for index in 0..document.link_count() {
            assert!(document.link(index).expect("link").id.len() <= 96);
        }
        let mut changed = document.origin.clone();
        changed.request_binding.requested_root_digest = [3; 32];
        let other_request =
            CargoReadmeDocument::prepare(changed, Arc::clone(&document.source), &|| false)
                .expect("different address only");
        assert_eq!(
            other_request.restore_focus(&document.link(0).expect("first link").id),
            None,
            "an equal package/path in another requested browse context cannot borrow focus"
        );
        let mut changed = document.origin.clone();
        changed.path = backend_library::CargoPackageSourcePathV1::new("other/README.md")
            .expect("different origin path");
        let other_origin =
            CargoReadmeDocument::prepare(changed, Arc::clone(&document.source), &|| false)
                .expect("different address only");
        assert_eq!(
            other_origin.restore_focus(&document.link(0).expect("first link").id),
            None,
            "same href under another README directory has another endpoint"
        );
    }

    #[test]
    fn late_authored_links_and_headings_remain_reachable_in_the_complete_document() {
        use std::fmt::Write as _;
        let mut source = String::new();
        for index in 0..700 {
            writeln!(
                &mut source,
                "# Section {index}\n\n[Code {index}](../src/file-{index}.rs#L7)\n"
            )
            .expect("source");
        }
        source.push_str("[Last heading](#section-699)\n\n[External](https://example.com/late)\n\n[Escaping](../../escape.rs)\n\n```md\n[Not authored](../src/not-a-link.rs)\n```\n");
        let document = document(&source);
        assert_eq!(document.heading_count(), 700);
        assert_eq!(document.link_count(), 703);
        let CargoReadmeDestination::Source(address) = document.destination("../src/file-699.rs#L7")
        else {
            panic!("the final authored file address");
        };
        assert_eq!(address.origin(), &document.origin);
        assert_eq!(address.path().as_str(), "src/file-699.rs");
        assert_eq!(address.fragment(), Some("L7"));
        assert!(
            matches!(document.destination("#section-699"), CargoReadmeDestination::Anchor(heading) if heading.slug.as_ref() == "section-699")
        );
        assert!(matches!(
            document.destination("https://example.com/late"),
            CargoReadmeDestination::External(_)
        ));
        assert!(
            matches!(document.destination("../../escape.rs"), CargoReadmeDestination::Unavailable(reason) if reason.contains("scope"))
        );
        assert!(
            matches!(document.destination("../src/not-a-link.rs"), CargoReadmeDestination::Unavailable(reason) if reason.contains("not an authored"))
        );
        let late = document.link(699).expect("late native row");
        assert_eq!(
            document.inline_focus_id("../src/file-699.rs#L7"),
            Some(Arc::clone(&late.id))
        );
        assert_eq!(
            document.restore_focus(&late.id),
            Some(CargoReadmeFocus::Link(699))
        );
        assert!(
            NavigationPage::containing(699)
                .range(document.link_count())
                .contains(&699)
        );
        assert_eq!(
            document.restore_focus(&document.heading_id(699).expect("last heading")),
            Some(CargoReadmeFocus::Heading(699))
        );
    }

    #[test]
    fn reference_links_share_long_destinations_and_late_duplicate_focus_is_exact() {
        let href = format!("https://example.com/{}", "x".repeat(3_000));
        let source = format!("{}\n\n[shared]: {href}\n", "[shared] ".repeat(2_049));
        let document = document(&source);
        assert_eq!(document.link_count(), 2_049);
        assert!(
            matches!(document.destination(&href), CargoReadmeDestination::External(url) if url.as_ref() == href)
        );
        let late = document.link(2_048).expect("last reference");
        assert_eq!(
            document.restore_focus(&late.id),
            Some(CargoReadmeFocus::Link(2_048))
        );
        assert_ne!(late.id, document.link(0).expect("first reference").id);
        assert!(late.id.len() <= 96);
        assert!(
            document.navigation_storage_bytes() < source.len() * 8,
            "the reference URL is retained once rather than expanded into every row or owner receipt"
        );
        let cloned = document.clone();
        assert!(
            Arc::ptr_eq(&document.navigation, &cloned.navigation),
            "render snapshots share the immutable index"
        );
    }

    #[test]
    fn duplicate_unicode_and_literal_suffix_headings_use_the_full_authored_index() {
        let document = document(
            "# X\n\n# X\n\n# X-1\n\n# CafÉ guide\n\n# 🚀\n\n[last](#x-1-1)\n\n[unicode](#caf%C3%A9-guide)\n",
        );
        assert_eq!(
            document.heading_count(),
            5,
            "even headings without an alphanumeric fragment remain native scroll targets"
        );
        assert_eq!(document.heading(0).expect("first").slug.as_ref(), "x");
        assert_eq!(document.heading(1).expect("duplicate").slug.as_ref(), "x-1");
        assert_eq!(
            document.heading(2).expect("literal suffix").slug.as_ref(),
            "x-1-1"
        );
        assert!(
            matches!(document.destination("#x-1-1"), CargoReadmeDestination::Anchor(heading) if heading.title.as_ref() == "X-1")
        );
        assert!(
            matches!(document.destination("#caf%C3%A9-guide"), CargoReadmeDestination::Anchor(heading) if heading.slug.as_ref() == "café-guide")
        );
    }

    #[test]
    fn withdrawn_preparation_never_publishes_a_shortened_index() {
        use std::cell::Cell;
        let ready = document("# Guide\n\n[Code](../src/lib.rs#L7)\n");
        assert!(matches!(
            CargoReadmeDocument::prepare(ready.origin.clone(), Arc::clone(&ready.source), &|| true),
            Err(PreparationError::Cancelled)
        ));
        for limit in [2, 5, 12] {
            let calls = Cell::new(0);
            let cancelled = || {
                calls.set(calls.get() + 1);
                calls.get() >= limit
            };
            assert!(matches!(
                CargoReadmeDocument::prepare(
                    ready.origin.clone(),
                    Arc::clone(&ready.source),
                    &cancelled
                ),
                Err(PreparationError::Cancelled)
            ));
        }
    }
}
