//! Its history: what the symbol was at each release of its package the
//! release data has read, against the release you pin.
//!
//! The release data is the fixture's today (toml and smallvec, see
//! `runtime::fixture_releases`); a package outside it has no history to draw,
//! and the page draws none rather than guess. What a release says is the
//! upgrade lens's own reading of this symbol's changes between the pin and
//! that release: no change is the same, a respelled one is "only its
//! lifetimes differ" (the plain words are equal), the rest differ; a symbol
//! that is removed going back is not here yet, going forward it is gone.

use crate::model::pages::{PackageRef, SymbolPage};
use facet::anatomy::history::{History, Release, Was, Weight};
use facet::data::release::{What, lens, semver, short};

/// The symbol's history, or nothing when no release data covers its package.
pub(super) fn of(package: &PackageRef, page: &SymbolPage, path: &str, cx: &mut gpui::App) -> History {
    let Some(krate) = crate::runtime::fixture_releases::release_data(package, cx) else { return History::default() };
    let _ = page;
    let mut previous: Option<(u64, u64, u64)> = None;
    let mut releases = Vec::new();
    for version in &krate.versions {
        let now = semver(&version.v);
        let weight = match previous {
            Some((major, minor, _)) if major != now.0 || (major == 0 && minor != now.1) => Weight::Major,
            Some((_, minor, _)) if minor != now.1 => Weight::Minor,
            _ => Weight::Patch,
        };
        if !version.yanked {
            previous = Some(now);
        }
        let was = if version.v == krate.pinned {
            Was::Pinned
        } else {
            let seen = lens(krate, path, &version.v, &facet::semantics::types::Nowhere);
            if !seen.local {
                Was::Unread
            } else if seen.rows.is_empty() {
                Was::Same
            } else if seen.rows.iter().all(|row| row.respelled) {
                Was::Differs("only its lifetimes differ".to_owned())
            } else if seen.rows.iter().any(|row| row.what == What::Removed) {
                if seen.forward { Was::Gone } else { Was::NotYet }
            } else if seen.rows.iter().any(|row| row.newly_fails) {
                Was::Differs("it can fail now".to_owned())
            } else if seen.rows.iter().any(|row| row.what == What::Added) && !seen.forward {
                Was::NotYet
            } else {
                Was::Differs("its signature changed".to_owned())
            }
        };
        releases.push(Release {
            version: short(&version.v).to_owned(),
            at: version.at.get(..10).unwrap_or(&version.at).to_owned(),
            yanked: version.yanked,
            weight,
            was,
        });
    }
    History { releases }
}
