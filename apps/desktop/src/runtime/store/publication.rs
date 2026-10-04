//! Completed package publications supersede observations independently of
//! the shell's global root. Cache invalidation never invents producer facts
//! or changes the person's route, focus, scroll position, or history.

use super::{DataStore, PageKey, PackageRef};
use crate::model::browse::BrowseKey;
use gpui::Context;
use std::collections::BTreeSet;

impl DataStore {
    pub(crate) fn packages_published(
        &mut self,
        packages: &BTreeSet<PackageRef>,
        cx: &mut Context<Self>,
    ) {
        if packages.is_empty() { return; }
        let changed = self.pages.keys().into_iter()
            .filter(|key| affected(key, packages))
            .collect::<Vec<_>>();
        for key in &changed {
            // Withdraw all pre-publication jobs before fencing cache inserts.
            self.cancel_key(key, cx);
            self.prefetching.remove(key);
        }
        if let Some(pool) = &self.pool {
            pool.invalidate_package_outlines(packages);
        }
        for key in &changed {
            let before = self.pages.stamp(key);
            self.pages.invalidate_publication(key);
            self.emit_moved(key.clone(), before, cx);
        }
        // Back/Forward entries stay invalidated until actually visited; only
        // the open dependencies reread now. A failed outline is no exception.
        for key in changed {
            if self.focused.contains(&key) {
                self.ensure(key, cx);
            }
        }
    }
}

fn affected(key: &PageKey, packages: &BTreeSet<PackageRef>) -> bool {
    let contains = |package: &PackageRef| {
        packages.iter().any(|changed| changed.reference() == package.reference())
    };
    match key {
        PageKey::Package(package) | PageKey::CargoSource(crate::model::pages::CargoSourceKey { package, .. }) => contains(package),
        PageKey::Symbol(symbol) | PageKey::Source(symbol) => packages.iter().any(|package| {
            symbol.as_str().strip_prefix(package.as_str()).is_some_and(|tail| tail.starts_with("::"))
        }),
        PageKey::Browse(BrowseKey::CargoSourceInventory(key)) => contains(&key.package),
        PageKey::Browse(BrowseKey::CargoReadme(key)) => contains(&key.package),
        PageKey::Browse(BrowseKey::Compare(selection)) => selection.packages().iter().any(contains),
        // Discovery, dependency-tree availability, search and aggregate counts
        // read the index frontier rather than one package's immutable content.
        PageKey::Search(_) | PageKey::Orbit | PageKey::Health
        | PageKey::Browse(BrowseKey::Tree(_) | BrowseKey::FindHome | BrowseKey::Find(_)) => true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::pages::{SearchQuery, SymbolRef};

    #[test]
    fn publication_scope_is_exact_across_content_and_shared_discovery() {
        let changed = PackageRef::parse("pkg:cargo/fail@0.5.1").expect("release");
        let packages = BTreeSet::from([changed.clone()]);
        assert!(affected(&PageKey::Package(changed), &packages));
        assert!(affected(&PageKey::Symbol(SymbolRef::new("pkg:cargo/fail@0.5.1::src/lib.rs:1::FailPoint").expect("symbol")), &packages));
        for address in ["pkg:cargo/fail@0.5.10::src/lib.rs:1::FailPoint", "pkg:cargo/failure@0.5.1::src/lib.rs:1::FailPoint"] {
            assert!(!affected(&PageKey::Symbol(SymbolRef::new(address).expect("other symbol")), &packages));
        }
        assert!(!affected(&PageKey::Package(PackageRef::parse("pkg:cargo/fail@0.5.0").expect("other version")), &packages));
        assert!(affected(&PageKey::Search(SearchQuery::new("fail", 20).expect("query")), &packages));
        assert!(affected(&PageKey::Browse(BrowseKey::FindHome), &packages));
        assert!(affected(&PageKey::Orbit, &packages));
    }
}
