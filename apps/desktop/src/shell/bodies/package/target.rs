//! What a keyboard door on the package page stands for, and the one place its
//! id is spelled. The shell's `j`/`k`/Enter walk a list of ids; the page
//! decides what each id opens, and Back finds the module a card was in from
//! the id it left by. Nothing else on the page writes or reads one.

use crate::model::pages::SymbolRef;
use gpui::SharedString;

/// A release on the ticker a door stands on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Mark {
    /// The release you pin.
    Pin,
    /// The release being read, when it is not the pin.
    Reading,
    /// The newest release.
    Newest,
    /// A release that broke its API (an index into the ticks).
    Breaking(usize),
}

/// One thing on the page the keyboard can be on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum PageTarget {
    /// A region of the territory: Enter opens the module.
    Module(SharedString),
    /// A card of the open module: Enter opens its page.
    Card(SymbolRef),
    /// The licence stamp: Enter holds it unfolded.
    Licence,
    /// The heads-up hand: Enter opens the sheet of evidence.
    Heads,
    /// The weight glyph: Enter opens the berg.
    Weight,
    /// A release on the ticker: Enter travels to it.
    Release(Mark),
    /// A block of the open berg (an index into its blocks): Enter goes to that package.
    Block(usize),
    /// A dependency the hero names that has a place to go (by its name):
    /// Enter opens it.
    Dependency(SharedString),
}

const MODULE: &str = "pkg-module-";
const CARD: &str = "pkg-card-";
const RELEASE: &str = "pkg-release-";
const BLOCK: &str = "pkg-block-";
const DEPENDENCY: &str = "pkg-dep-";

impl PageTarget {
    /// The id the shell's target list carries.
    pub(super) fn id(&self) -> SharedString {
        match self {
            Self::Module(name) => format!("{MODULE}{name}"),
            Self::Card(symbol) => format!("{CARD}{}", symbol.as_str()),
            Self::Licence => "pkg-licence".to_owned(),
            Self::Heads => "pkg-heads".to_owned(),
            Self::Weight => "pkg-weight".to_owned(),
            Self::Release(Mark::Pin) => format!("{RELEASE}pin"),
            Self::Release(Mark::Reading) => format!("{RELEASE}reading"),
            Self::Release(Mark::Newest) => format!("{RELEASE}newest"),
            Self::Release(Mark::Breaking(tick)) => format!("{RELEASE}{tick}"),
            Self::Block(index) => format!("{BLOCK}{index}"),
            Self::Dependency(name) => format!("{DEPENDENCY}{name}"),
        }
        .into()
    }

    /// The door an id names (`None` for an id the page did not write).
    pub(super) fn parse(id: &str) -> Option<Self> {
        if let Some(name) = id.strip_prefix(MODULE) {
            return Some(Self::Module(name.to_owned().into()));
        }
        if let Some(symbol) = id.strip_prefix(CARD) {
            return SymbolRef::new(symbol).ok().map(Self::Card);
        }
        if let Some(mark) = id.strip_prefix(RELEASE) {
            return match mark {
                "pin" => Some(Self::Release(Mark::Pin)),
                "reading" => Some(Self::Release(Mark::Reading)),
                "newest" => Some(Self::Release(Mark::Newest)),
                tick => tick
                    .parse()
                    .ok()
                    .map(|tick| Self::Release(Mark::Breaking(tick))),
            };
        }
        if let Some(index) = id.strip_prefix(BLOCK) {
            return index.parse().ok().map(Self::Block);
        }
        if let Some(name) = id.strip_prefix(DEPENDENCY) {
            return Some(Self::Dependency(name.to_owned().into()));
        }
        match id {
            "pkg-licence" => Some(Self::Licence),
            "pkg-heads" => Some(Self::Heads),
            "pkg-weight" => Some(Self::Weight),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_door_reads_back_as_itself() {
        let symbol = SymbolRef::new("pkg:cargo/x@1::a.rs:3::Item").expect("symbol");
        for target in [
            PageTarget::Module("sync::mpsc".into()),
            PageTarget::Card(symbol),
            PageTarget::Licence,
            PageTarget::Heads,
            PageTarget::Weight,
            PageTarget::Release(Mark::Pin),
            PageTarget::Release(Mark::Reading),
            PageTarget::Release(Mark::Newest),
            PageTarget::Release(Mark::Breaking(41)),
            PageTarget::Block(7),
            PageTarget::Dependency("serde_spanned".into()),
        ] {
            assert_eq!(
                PageTarget::parse(&target.id()),
                Some(target.clone()),
                "{target:?}"
            );
        }
        assert_eq!(
            PageTarget::parse("tb-shelf"),
            None,
            "an id the page did not write is not a door"
        );
        assert_eq!(
            PageTarget::parse("pkg-feature-rt-multi-thread"),
            None,
            "a read-only feature preview is not a keyboard door"
        );
    }

    #[test]
    fn the_module_and_card_ids_keep_the_spelling_the_shell_tests_walk() {
        assert_eq!(
            PageTarget::Module("glyph".into()).id().as_ref(),
            "pkg-module-glyph"
        );
        assert_eq!(
            PageTarget::Card(SymbolRef::new("a::b").expect("symbol"))
                .id()
                .as_ref(),
            "pkg-card-a::b"
        );
    }
}
