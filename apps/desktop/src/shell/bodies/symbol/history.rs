//! Its history: what the symbol was at each exact release the local owner
//! compared with the pin. Unread releases stay unread; a missing comparison
//! never becomes an unchanged symbol.

use crate::model::pages::SymbolPage;
use facet::anatomy::history::{History, Release, Was, Weight};
use facet::data::release::{Crate, What, lens, semver, short};

/// The symbol's history, or nothing when no release data covers its package.
pub(super) fn of(krate: &Crate, page: &SymbolPage, path: &str) -> History {
    let _ = page;
    let mut previous: Option<(u64, u64, u64)> = None;
    let mut releases = Vec::new();
    for version in &krate.versions {
        let now = semver(&version.v);
        let weight = match previous {
            Some((major, minor, _)) if major != now.0 || (major == 0 && minor != now.1) => {
                Weight::Major
            }
            Some((_, minor, _)) if minor != now.1 => Weight::Minor,
            _ => Weight::Patch,
        };
        previous = Some(now);
        let was = if version.v == krate.pinned {
            Was::Pinned
        } else {
            let seen = lens(krate, path, &version.v, &facet::semantics::types::Nowhere);
            if !seen.compared() {
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
            at: version.at.clone(),
            yanked: version.yanked.clone(),
            weight,
            was,
        });
    }
    History { releases }
}
