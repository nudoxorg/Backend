//! Defines the exploration state for `interface-gui`.
//! This module owns the index-search page, per-package version rows, and per-package profiles.
//! Its narrow surface keeps every catalogue refusal a typed value the views can render.

use std::collections::HashMap;

use interface_library::{
    ExploreError, ExplorePackageName, IndexSearchPage, PackageProfile, PackageVersionRows,
};

/// The state one exploration command owes its slot: loading, ready, or refused as typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExploreSlot<T> {
    /// The command is on its way.
    Loading,
    /// The catalogue answered.
    Ready(T),
    /// The catalogue refused, with its own cause retained.
    Failed(ExploreError),
}

/// Everything the exploration views read, keyed by what asked for it.
///
/// A fault is stored as the engine's own [`ExploreError`], never flattened into a string, so a
/// view can name the state exactly as every other surface does. An absent catalogue is a state:
/// it lands here as [`ExploreSlot::Failed`] with [`ExploreError::CatalogAbsent`], not a crash.
#[derive(Debug, Default)]
pub struct ExploreStore {
    index_page: Option<ExploreSlot<IndexSearchPage>>,
    versions: HashMap<ExplorePackageName, ExploreSlot<PackageVersionRows>>,
    profiles: HashMap<ExplorePackageName, ExploreSlot<PackageProfile>>,
}

impl ExploreStore {
    /// The index page, when one has been asked for.
    #[must_use]
    pub const fn index_page(&self) -> Option<&ExploreSlot<IndexSearchPage>> {
        self.index_page.as_ref()
    }

    /// One package's version rows, when they have been asked for.
    #[must_use]
    pub fn versions(&self, name: &ExplorePackageName) -> Option<&ExploreSlot<PackageVersionRows>> {
        self.versions.get(name)
    }

    /// One package's profile, when it has been asked for.
    #[must_use]
    pub fn profile(&self, name: &ExplorePackageName) -> Option<&ExploreSlot<PackageProfile>> {
        self.profiles.get(name)
    }

    /// Records that an index search is on its way, replacing any page already shown.
    pub fn begin_index_search(&mut self) {
        self.index_page = Some(ExploreSlot::Loading);
    }

    /// Records that one package's versions are on their way.
    pub fn begin_versions(&mut self, name: &ExplorePackageName) {
        self.versions.insert(name.clone(), ExploreSlot::Loading);
    }

    /// Records that one package's profile is on their way.
    pub fn begin_profile(&mut self, name: &ExplorePackageName) {
        self.profiles.insert(name.clone(), ExploreSlot::Loading);
    }

    /// Folds one index-search terminal into its slot.
    pub fn apply_index_search(&mut self, page: Result<IndexSearchPage, ExploreError>) {
        self.index_page = Some(match page {
            Ok(page) => ExploreSlot::Ready(page),
            Err(error) => ExploreSlot::Failed(error),
        });
    }

    /// Folds one version terminal into the named package's slot.
    pub fn apply_versions(
        &mut self,
        name: &ExplorePackageName,
        rows: Result<PackageVersionRows, ExploreError>,
    ) {
        self.versions.insert(name.clone(), match rows {
            Ok(rows) => ExploreSlot::Ready(rows),
            Err(error) => ExploreSlot::Failed(error),
        });
    }

    /// Folds one profile terminal into the named package's slot.
    pub fn apply_profile(
        &mut self,
        name: &ExplorePackageName,
        profile: Result<PackageProfile, ExploreError>,
    ) {
        self.profiles.insert(name.clone(), match profile {
            Ok(profile) => ExploreSlot::Ready(profile),
            Err(error) => ExploreSlot::Failed(error),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{ExploreSlot, ExploreStore};
    use interface_library::{
        ExploreCoverage, ExploreError, ExplorePackageName, ExploreUnavailable, IndexSearchPage,
        PackageProfile, PackageVersionRows,
    };

    /// One admitted package name, so a fixture failure names its input.
    fn name(text: &str) -> Option<ExplorePackageName> {
        ExplorePackageName::new(text).ok()
    }

    /// One ready page the index answered without any retained row.
    fn empty_page() -> IndexSearchPage {
        IndexSearchPage {
            hits: Box::new([]),
            coverage: ExploreCoverage::Unavailable {
                reason: ExploreUnavailable::CatalogAbsent,
            },
        }
    }

    #[test]
    fn an_absent_catalogue_is_a_typed_state_never_a_dropped_result() {
        let mut store = ExploreStore::default();
        store.begin_index_search();
        assert_eq!(store.index_page(), Some(&ExploreSlot::Loading));
        store.apply_index_search(Err(ExploreError::CatalogAbsent));
        assert_eq!(
            store.index_page(),
            Some(&ExploreSlot::Failed(ExploreError::CatalogAbsent)),
            "the catalogue's own refusal is retained as typed"
        );
    }

    #[test]
    fn a_ready_page_replaces_its_loading_slot_with_the_answer_retained() {
        let mut store = ExploreStore::default();
        store.begin_index_search();
        store.apply_index_search(Ok(empty_page()));
        let Some(ExploreSlot::Ready(landed)) = store.index_page() else {
            return;
        };
        assert!(landed.hits.is_empty());
        assert_eq!(
            landed.coverage,
            ExploreCoverage::Unavailable {
                reason: ExploreUnavailable::CatalogAbsent,
            }
        );
    }

    #[test]
    fn version_and_profile_slots_are_independent_per_package_name() {
        let mut store = ExploreStore::default();
        let Some(serde) = name("serde") else {
            return;
        };
        let Some(tokio) = name("tokio") else {
            return;
        };
        store.begin_versions(&serde);
        store.begin_profile(&serde);
        assert_eq!(store.versions(&serde), Some(&ExploreSlot::Loading));
        assert_eq!(
            store.versions(&tokio),
            None,
            "names are keyed separately"
        );
        store.apply_versions(&serde, Ok(PackageVersionRows { rows: Box::new([]) }));
        store.apply_profile(
            &serde,
            Ok(PackageProfile {
                latest: None,
                versions: PackageVersionRows { rows: Box::new([]) },
            }),
        );
        assert_eq!(
            store.versions(&serde),
            Some(&ExploreSlot::Ready(PackageVersionRows { rows: Box::new([]) }))
        );
        assert_eq!(
            store.profile(&serde),
            Some(&ExploreSlot::Ready(PackageProfile {
                latest: None,
                versions: PackageVersionRows { rows: Box::new([]) },
            }))
        );
        assert_eq!(store.profile(&tokio), None, "a profile only lands on the name that asked");
    }

    #[test]
    fn a_catalogue_refusal_folds_into_the_package_that_asked() {
        let mut store = ExploreStore::default();
        let Some(serde) = name("serde") else {
            return;
        };
        store.begin_versions(&serde);
        store.apply_versions(&serde, Err(ExploreError::CatalogAbsent));
        assert_eq!(
            store.versions(&serde),
            Some(&ExploreSlot::Failed(ExploreError::CatalogAbsent))
        );
    }
}
