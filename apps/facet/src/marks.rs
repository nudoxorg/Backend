//! Identity marks: each fact about a package is its own component
//! (`gui-plan.md` §6.2, wave 4's D-Marks target in `v4/marks/`).
//!
//! At rest each mark is one quiet glyph, with at most one word in the hero.
//! The richness lives behind it: resting on a mark unfurls its card from
//! the mark itself, the keyboard opens it at once, and ⌥ raises what waits.
//!
//! - [`eco`]: the registry's stone; the card holds the install line you
//!   press to copy.
//! - [`license`]: a ring per license family; a GitHub-style card of
//!   permissions, conditions and limitations, whether it fits your project,
//!   what it adds to your tree, and one hedge line at its foot.
//! - [`version`]: the version *is* the comb (`controls::comb`): the Rider in
//!   the hero, the Baseline in rows, a band below 240 px.
//! - [`deps`]: dependencies as real links with cards that say what you use
//!   each one for, folded to one line at rest.
//!
//! Plain data in, and nothing here knows the engine: a fact that is not
//! known renders as unknown, never invented.

mod card;
pub mod deps;
pub mod eco;
mod glyph;
pub mod license;
pub mod semver;
pub mod spdx;
pub mod version;

#[cfg(any(test, feature = "gallery"))]
pub mod fixture;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
#[cfg(all(test, feature = "gallery"))]
mod tests;


pub use deps::{DepFacts, DepKind, DepLine, DepLink, InTree, dep_line, dep_link};
pub use eco::{Eco, EcoFacts, EcosystemMark, ecosystem_mark};
pub use license::{LicenseFacts, LicenseMark, license_mark};
pub use version::{Also, Diff, Measured, VersionFacts, VersionMark, version_mark};
