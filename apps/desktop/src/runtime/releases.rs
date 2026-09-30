//! Real release data for a registry package the library holds, behind the
//! same API the fixture served (`fixture_releases::release_data`): every
//! release the cargo index cache lists (dated, yanked, on this machine or
//! not), and, for each other release the library also holds, the owner's own
//! diff from the pinned release to it (`SurfaceCommand::Diff`).
//!
//! Built off the UI thread, once per package and per landing: a release added
//! later (the comb's "Add toml 0.5.11") is compared once it lands. Nothing is
//! invented: a release the library does not hold has no diff, so the page
//! says its names are not read rather than showing a comparison.
//!
//! The value is leaked (`&'static`, the fixture's lifetime), once per package
//! and landing, a few kilobytes each.

use crate::model::pages::PackageRef;
use crate::model::release::Availability;
use backend_client::Session;
use backend_library::{
    CommandReply, DeclarationChange, PackageReference, SurfaceCommand, SurfaceReply,
};
use facet::data::release::{Change, Crate, ReleaseDiff, Severity, Version, What};
use std::collections::{HashMap, HashSet};

/// The release data of the registry package whose tree the library holds at
/// `root`: `None` for a person's own project, a release the source does not
/// know, or an owner that cannot be reached.
pub(crate) fn release_data(root: &str) -> Option<&'static Crate> {
    let package = PackageRef::parse(root).ok()?;
    let pinned = package.release()?;
    let composition = crate::host::registry::composed()?;
    let published = composition.source.releases(&pinned.name);
    let mut session = Session::connect(&composition.endpoint).ok()?;
    let listed = match session.packages().ok()?.reply {
        CommandReply::Packages(snapshot) => snapshot
            .root
            .rows()
            .iter()
            .map(|row| row.label.clone())
            .collect::<HashSet<_>>(),
        _ => return None,
    };
    let pinned_version = pinned.version.as_str().to_owned();
    let from = PackageReference::parse(package.as_str().to_owned()).ok()?;
    let mut diffs = Vec::new();
    for release in published.iter().filter(|release| release.release != pinned) {
        let Some(tree) = composition.source.tree_of(&release.release) else {
            continue;
        };
        let Some(tree) = tree.to_str().filter(|tree| listed.contains(*tree)) else {
            continue;
        };
        let Ok(to) = PackageReference::parse(tree.to_owned()) else {
            continue;
        };
        let Ok(SurfaceReply::Diff(records)) = session.surface(SurfaceCommand::Diff {
            from: from.clone(),
            to,
        }) else {
            continue;
        };
        let changes = records
            .iter()
            .filter_map(|record| {
                let (what, severity) = match record.change {
                    DeclarationChange::Added => (What::Added, Severity::Additive),
                    DeclarationChange::Removed => (What::Removed, Severity::Breaking),
                    DeclarationChange::Changed => (What::Changed, Severity::Breaking),
                    // No one-to-one pairing: not a change the page can name.
                    DeclarationChange::Indeterminate => return None,
                };
                Some(Change {
                    path: item_path(record.label.as_str(), package.as_str(), tree).into(),
                    what,
                    severity,
                    before: None,
                    after: None,
                })
            })
            .collect();
        diffs.push(ReleaseDiff {
            from: pinned_version.clone().into(),
            to: release.release.version.as_str().to_owned().into(),
            changes,
            semver_slip: false,
        });
    }
    let versions = published
        .iter()
        .map(|release| Version {
            v: release.release.version.as_str().to_owned().into(),
            at: release
                .date
                .as_deref()
                .unwrap_or_default()
                .to_owned()
                .into(),
            yanked: release.yanked,
            local: !matches!(release.availability, Availability::Download),
        })
        .collect();
    let krate = Crate {
        name: pinned.name.as_str().to_owned().into(),
        pinned: pinned_version.into(),
        versions,
        aliases: HashMap::new(),
        diffs,
        uses: Vec::new(),
        impact: Vec::new(),
    };
    Some(Box::leak(Box::new(krate)))
}

/// A declaration's path within its release: its label less the tree it is
/// in (`src/value.rs:25::Value`), whichever of the two trees that is.
fn item_path(label: &str, from: &str, to: &str) -> String {
    [from, to]
        .iter()
        .find_map(|root| {
            label
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix("::"))
        })
        .unwrap_or(label)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::item_path;

    #[test]
    fn an_item_is_named_by_its_path_in_either_release() {
        assert_eq!(
            item_path(
                "/c/toml-0.8.23::src/value.rs:25::Value",
                "/c/toml-0.8.23",
                "/c/toml-0.5.11"
            ),
            "src/value.rs:25::Value"
        );
        assert_eq!(
            item_path(
                "/c/toml-0.5.11::src/tokens.rs:67::Tokenizer",
                "/c/toml-0.8.23",
                "/c/toml-0.5.11"
            ),
            "src/tokens.rs:67::Tokenizer"
        );
    }
}
