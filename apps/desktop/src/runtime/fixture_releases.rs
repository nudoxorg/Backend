//! The **fixture** releases: the upgrade lens's data, from facet's release
//! fixture (a slice of the prototype's `releases.json`: **toml and smallvec
//! only**, with this workspace's uses of each).
//!
//! It is a stand-in until the index serves release diffs (W-Browse plans
//! them). Any other package's comb shows no lens: scrubbing still views the
//! release, the lens simply has nothing to compare. The fixture is parsed
//! once, off the UI thread, the first time a lens asks; every window redraws
//! once when it lands.
//!
//! Callers ask for a package's [`Crate`] through [`release_data`]; none of
//! them names the fixture, so index-served diffs replace this module and
//! nothing else.

use crate::model::pages::PackageRef;
use facet::data::release::Crate;
use gpui::{App, Global};

/// The service's state in the app.
#[cfg_attr(test, allow(dead_code, reason = "tests install the parsed fixture"))]
enum Service {
    Loading,
    Ready(&'static [Crate]),
}

impl Global for Service {}

/// The release data for `package` (matched by its registry name), when the
/// fixture has it and has landed. `None` while it loads (the first ask
/// starts it) and for every package outside the fixture.
pub(crate) fn release_data(package: &PackageRef, cx: &mut App) -> Option<&'static Crate> {
    let crates = match cx.try_global::<Service>() {
        Some(Service::Ready(crates)) => *crates,
        Some(Service::Loading) => return None,
        None => {
            start(cx);
            return None;
        }
    };
    if package.is_local() {
        return None;
    }
    let name = package.display_name();
    crates.iter().find(|release| release.name.as_ref() == name)
}

/// `version` as `release` spells it: the registry, the comb and the route
/// say `1.1.6`, the release data may carry build metadata
/// (`1.1.6+spec-1.1.0`). `None` when the data has no such release.
pub(crate) fn spelled(release: &Crate, version: &str) -> Option<gpui::SharedString> {
    release
        .versions
        .iter()
        .find(|known| known.v.as_ref() == version || facet::data::release::short(&known.v) == version)
        .map(|known| known.v.clone())
}

/// Starts the parse off the UI thread (tests [`install`] it instead).
fn start(cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
    }
    #[cfg(not(test))]
    {
        cx.set_global(Service::Loading);
        let parse = cx.background_executor().spawn(async { facet::data::release::fixture::crates() });
        cx.spawn(async move |cx| {
            let crates = parse.await;
            let _ = cx.update(|cx| {
                cx.set_global(Service::Ready(crates));
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

/// Installs the parsed fixture (tests; parsing is synchronous there).
#[cfg(test)]
pub(crate) fn install(cx: &mut App) {
    cx.set_global(Service::Ready(facet::data::release::fixture::crates()));
}
