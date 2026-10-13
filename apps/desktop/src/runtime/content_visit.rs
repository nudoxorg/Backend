//! The shared identity of transient publication and input on one content
//! activation. It carries no resource receipt or serving-owner capability.

use crate::core::VersionedRoot;
use crate::model::AppSnapshot;
use crate::navigation::{Route, presentation::ReadingActivation};

#[derive(Clone, Debug)]
pub(crate) struct ContentVisit {
    route: Route,
    root: VersionedRoot,
    activation: ReadingActivation,
}

impl ContentVisit {
    pub(crate) fn capture(snapshot: &AppSnapshot) -> Self {
        Self {
            route: snapshot.route().clone(),
            root: snapshot.key(),
            activation: snapshot.session().reading.activation().clone(),
        }
    }

    /// Covers preserve the underlying visit; visibility is checked by the
    /// consumer. Root observation metadata does not change producer authority.
    pub(crate) fn current(&self, snapshot: &AppSnapshot) -> bool {
        self.matches_destination(snapshot) && self.root.same_authority(snapshot.key())
    }

    /// Local reading/editing does not require a serving producer. It still
    /// belongs to this activation, even when history restores its reading ID.
    pub(crate) fn matches_destination(&self, snapshot: &AppSnapshot) -> bool {
        self.route == *snapshot.route()
            && &self.activation == snapshot.session().reading.activation()
    }
}

impl PartialEq for ContentVisit {
    fn eq(&self, other: &Self) -> bool {
        self.route == other.route
            && self.root.same_authority(other.root)
            && self.activation == other.activation
    }
}
impl Eq for ContentVisit {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{Intent, View, presentation::{ReadingChange, ReadingOffset}};

    fn base() -> AppSnapshot {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("visit".to_owned(), "tests".to_owned())]), 4,
        );
        crate::navigation::reduce(&AppSnapshot::empty(root), Intent::Navigate(
            crate::shell::tests::view_route("RelationLabel", View::Graph))).snapshot
    }

    #[test]
    fn history_restores_reading_context_with_fresh_action_and_publication_identity() {
        let base = base();
        let base = crate::navigation::reduce(&base, Intent::SetReading {
            visit: base.session().reading.current.id,
            change: ReadingChange::ReaderOffset(ReadingOffset::new(0.0, -240.0).expect("scroll")),
        }).snapshot;
        let visit = ContentVisit::capture(&base);
        let other = crate::navigation::reduce(&base, Intent::Navigate(
            crate::shell::tests::page_route("relation_label"))).snapshot;
        let returned = crate::navigation::reduce(&other, Intent::Back).snapshot;
        assert_eq!(returned.route(), base.route());
        assert_eq!(returned.session().reading.current, base.session().reading.current,
            "Back keeps deliberate graph reading context");
        assert!(!visit.current(&returned), "the old activation cannot return with its reading choices");
        let renewed = ContentVisit::capture(&returned);
        assert!(renewed.current(&returned));
        let forwarded = crate::navigation::reduce(&returned, Intent::Forward).snapshot;
        assert_eq!(forwarded.session().reading.current, other.session().reading.current);
        assert!(!ContentVisit::capture(&other).current(&forwarded));
    }

    #[test]
    fn page_code_graph_replacements_keep_context_and_retire_every_prior_activation() {
        let mut current = base();
        let reading = current.session().reading.current.clone();
        let original = ContentVisit::capture(&current);
        for view in [View::Page, View::Code, View::Graph] {
            let captured = ContentVisit::capture(&current);
            current = crate::navigation::reduce(&current, Intent::SetView(view)).snapshot;
            assert_eq!(current.session().reading.current, reading);
            assert!(!captured.current(&current));
        }
        assert_eq!(current.route(), base().route());
        assert!(!original.current(&current), "returning to the same top navigation view is fresh input ownership");
    }

    #[test]
    fn covers_and_local_edits_preserve_identity_but_cancelled_preview_renews_it() {
        let base = base();
        let visit = ContentVisit::capture(&base);
        let covered = crate::navigation::reduce(&base, Intent::OpenCommandPalette).snapshot;
        assert!(visit.current(&covered));
        let uncovered = crate::navigation::reduce(&covered, Intent::DismissOverlay).snapshot;
        assert!(visit.current(&uncovered));
        let edited = crate::navigation::reduce(&uncovered, Intent::SetReading {
            visit: uncovered.session().reading.current.id,
            change: ReadingChange::ReaderOffset(ReadingOffset::new(0.0, -80.0).expect("scroll")),
        }).snapshot;
        assert!(visit.current(&edited));
        let covered = crate::navigation::reduce(&edited, Intent::OpenCommandPalette).snapshot;
        let preview = crate::navigation::reduce(&covered, Intent::Preview(Route::World)).snapshot;
        let returned = crate::navigation::reduce(&preview, Intent::EndPreview).snapshot;
        assert_eq!(returned.session().reading.current, edited.session().reading.current);
        assert!(!visit.current(&returned));
    }

    #[test]
    fn observation_metadata_preserves_identity_and_every_producer_authority_change_retires_it() {
        let base = base();
        let root = base.key();
        let visit = ContentVisit::capture(&base);
        let observed = base.with_key(VersionedRoot::from_revision(
            root.producer_epoch(), root.revision(), root.observation().saturating_add(1)), None);
        assert!(visit.current(&observed));
        assert_eq!(visit, ContentVisit::capture(&observed));
        for changed in [
            root.with_generation(root.generation().saturating_add(1)),
            VersionedRoot::from_revision(root.producer_epoch().saturating_add(1), root.revision(), root.observation()),
            VersionedRoot::synthetic(backend_library::view_state_root(&[("visit".to_owned(), "other".to_owned())]), root.generation()),
        ] {
            assert!(!visit.current(&base.with_key(changed, None)));
        }
    }
}
