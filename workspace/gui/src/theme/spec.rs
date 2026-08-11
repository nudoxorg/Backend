//! `ThemeSpec` — a theme, as data.
//!
//! # What changed and why
//!
//! `themes/light.rs` and `themes/dark.rs` each carried a `fn light_theme() ->
//! NudoxThemeExt` that wrote out every token by hand, and their doc comments
//! said "this is DATA" and "adding a third theme = adding a third file with a
//! third `fn foo_theme()`". Those two claims cannot both be true: a function
//! containing ninety `hsla(…)` literals is code, and it is code that has to
//! make ninety correct decisions to produce one coherent theme. That is why
//! there were two themes after the whole life of the project.
//!
//! A `ThemeSpec` is the decisions and nothing else — twenty-three numbers, four
//! trust hues, thirteen kind hues, four elevation planes. It deserialises from
//! JSON, the bundled themes live in `assets/themes/*.json`, and the mapping from
//! spec to tokens is [`crate::theme::resolve::resolve`], which is one function
//! shared by every theme.
//!
//! **Adding a theme is: one JSON file, one line in [`BUNDLED`].** No view
//! changes, no role assignment changes, no colour code anywhere. That is the
//! property the restructure was for, and `tests/theme_law.rs` asserts it by
//! parsing every bundled theme and checking the invariants hold for all of
//! them, so a new theme cannot be added *wrongly* either.

use crate::theme::palette::{Appearance, KindSpec, Palette, RampSpec, SurfaceSpec};

// ─────────────────────────────────────────────────────────────────────────────
// ThemeSpec
// ─────────────────────────────────────────────────────────────────────────────

/// Every decision a theme makes.
///
/// `deny_unknown_fields` is deliberate: a typo in a theme file must be a parse
/// error naming the field, not a silently-ignored key that leaves the reader
/// wondering why their colour did nothing. This is the same reasoning as
/// doctrine §8's rule against `map_err(|_|)` — a failure that presents as a
/// success is the expensive kind.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSpec {
    /// Stable identifier. Used by the cycle order, by tests, and by any future
    /// persistence — so it must not change when the display name does.
    pub key: String,
    /// What the reader sees in the theme switcher and the status bar.
    pub name: String,
    /// One line explaining what this theme is *for*. Shown when cycling, so the
    /// reader learns the set rather than guessing at it.
    pub blurb: String,
    /// Which direction the scale runs.
    pub appearance: Appearance,

    /// The greys. `chroma` here is the theme's temperature.
    pub neutral: RampSpec,
    /// Links, focus rings, selection, keywords.
    pub accent: RampSpec,
    /// Success.
    pub ok: RampSpec,
    /// Warning.
    pub warn: RampSpec,
    /// Error.
    pub danger: RampSpec,
    /// Information.
    pub info: RampSpec,

    /// Provenance chrome, in `Provenance` order: local, synced, remote, stale.
    ///
    /// An array rather than four named fields because the resolve step wants to
    /// hand them to the palette positionally, and because a theme author who
    /// gets the order wrong will see it immediately — "local" is the green one.
    pub trust: [RampSpec; 4],

    /// The thirteen kind hues and the plane they share.
    pub kinds: KindSpec,

    /// The four elevation planes.
    pub surfaces: SurfaceSpec,
}

impl ThemeSpec {
    /// Resolve this spec into a palette.
    ///
    /// Separate from `resolve()` so a test can inspect the palette without
    /// building a whole token set, and so the palette can be carried on the
    /// theme for the coverage assertion.
    pub fn palette(&self) -> Palette {
        Palette::generate(
            self.appearance,
            self.neutral,
            self.accent,
            self.ok,
            self.warn,
            self.danger,
            self.info,
            self.trust,
            self.kinds,
            self.surfaces,
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The bundle
// ─────────────────────────────────────────────────────────────────────────────

/// Every theme shipped with the application, in cycle order.
///
/// # Why `include_str!` and not a directory read
///
/// A theme has to exist before the first frame paints, and a missing or
/// unreadable file at that point has no good recovery — a themeless GPUI window
/// renders opaque black text on opaque black, which is how this codebase already
/// lost a day to `TitleStyle` defaulting to `black()`. Compiling the JSON in
/// makes "the theme file is missing" a build error instead of a runtime one,
/// while keeping the *content* data.
///
/// The cost is this one line per theme. That is the entire code change adding a
/// theme requires, and `bundled_themes_all_parse` in `tests/theme_law.rs` fails
/// if a file listed here does not parse or does not satisfy the invariants.
pub const BUNDLED: &[&str] = &[
    include_str!("../../assets/themes/ink-dark.json"),
    include_str!("../../assets/themes/paper-light.json"),
    include_str!("../../assets/themes/slate-contrast.json"),
    include_str!("../../assets/themes/ember-dark.json"),
];

/// Why a bundled theme could not be loaded.
///
/// One variant per distinct recoverable situation, each naming the theme it
/// happened to — because "theme failed to parse" with four candidates is a
/// message that costs the reader the rest of the diagnosis.
#[derive(Debug, thiserror::Error)]
pub enum ThemeLoadError {
    /// The JSON did not deserialise into a [`ThemeSpec`].
    #[error("bundled theme #{index} is not a valid ThemeSpec: {source}")]
    Malformed {
        /// Position in [`BUNDLED`], so the reader can find the file.
        index: usize,
        /// The serde error, which names the offending field and line.
        #[source]
        source: serde_json::Error,
    },
    /// Two themes claimed the same `key`, so `cycle` and any future
    /// persistence would be ambiguous.
    #[error("themes #{first} and #{second} share the key `{key}`")]
    DuplicateKey {
        /// Index of the first theme with this key.
        first: usize,
        /// Index of the second.
        second: usize,
        /// The colliding key.
        key: String,
    },
    /// [`BUNDLED`] was empty, so there is no theme to start in.
    #[error("no themes are bundled; the application cannot start themeless")]
    Empty,
}

/// Parse every bundled theme.
///
/// Returns `Err` rather than panicking so the test suite can assert on the
/// typed variant instead of on a panic message, per doctrine §4.
pub fn parse_bundled() -> Result<Vec<ThemeSpec>, ThemeLoadError> {
    if BUNDLED.is_empty() {
        return Err(ThemeLoadError::Empty);
    }
    let mut out: Vec<ThemeSpec> = Vec::with_capacity(BUNDLED.len());
    for (index, raw) in BUNDLED.iter().enumerate() {
        let spec: ThemeSpec = serde_json::from_str(raw)
            .map_err(|source| ThemeLoadError::Malformed { index, source })?;
        if let Some(first) = out.iter().position(|t| t.key == spec.key) {
            return Err(ThemeLoadError::DuplicateKey {
                first,
                second: index,
                key: spec.key,
            });
        }
        out.push(spec);
    }
    Ok(out)
}
