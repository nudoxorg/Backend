//! Deterministic font inputs for the desktop and screenshot harness.
//!
//! The application still gets platform fallback fonts for scripts the bundled
//! faces do not cover, but the four Nudox roles always resolve to these
//! embedded files first. This keeps line breaks and glyph metrics stable
//! across machines and makes a font change an explicit baseline event.

use gpui::{App, Result};
use sha2::{Digest, Sha256};
use std::borrow::Cow;

/// The UI family used by controls, navigation, and metadata.
pub(crate) const UI_FAMILY: &str = "Instrument Sans";
/// The display family used by page titles and headings.
pub(crate) const DISPLAY_FAMILY: &str = "Archivo";
/// The reading family used by prose and long descriptions.
pub(crate) const SERIF_FAMILY: &str = "Newsreader";
/// The specimen family used by signatures, paths, and source.
pub(crate) const SPECIMEN_FAMILY: &str = "Geist Mono";

static DISPLAY_FONT: &[u8] = include_bytes!("../../resources/fonts/Archivo[wdth,wght].ttf");
static UI_FONT: &[u8] = include_bytes!("../../resources/fonts/InstrumentSans[wdth,wght].ttf");
static SERIF_FONT: &[u8] = include_bytes!("../../resources/fonts/Newsreader[opsz,wght].ttf");
static SPECIMEN_FONT: &[u8] = include_bytes!("../../resources/fonts/GeistMono[wght].ttf");

/// The exact bytes that define the screenshot typography contract.
///
/// Keeping the hashes in source makes a changed font an explicit baseline
/// change. `install` verifies them before registering anything, so a capture
/// cannot silently fall back to a host font after an incomplete asset update.
pub(crate) const MANIFEST: [(&str, &str); 4] = [
    (
        "Archivo[wdth,wght].ttf",
        "0e094a7d3c7c4c25cf1310c4b30014f1dae9332220b1c2c88f4fa996f0b05053",
    ),
    (
        "InstrumentSans[wdth,wght].ttf",
        "b24f1812584816958afcf22e22d08e44318c5e51651e25d2438efdde389b33b1",
    ),
    (
        "Newsreader[opsz,wght].ttf",
        "8a08d13f8a6c0d51be379a60af84f945f65369a67e509ee3c3bdcc421254d7c1",
    ),
    (
        "GeistMono[wght].ttf",
        "87c2aff9723544a9adaea19d92e42a33705c9723624801b6e0224c2206a6af0d",
    ),
];

fn bundled() -> [(&'static str, &'static [u8]); 4] {
    [
        (MANIFEST[0].0, DISPLAY_FONT),
        (MANIFEST[1].0, UI_FONT),
        (MANIFEST[2].0, SERIF_FONT),
        (MANIFEST[3].0, SPECIMEN_FONT),
    ]
}

/// Checks the capture-critical font contract without requiring a GPUI app.
pub(crate) fn verify_manifest() -> Result<()> {
    for ((name, expected), (_, bytes)) in MANIFEST.iter().zip(bundled()) {
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != *expected {
            anyhow::bail!("bundled font {name} has SHA-256 {actual}; expected {expected}");
        }
    }
    Ok(())
}

/// Adds the exact font bytes to GPUI's text system before the first window.
pub(crate) fn install(cx: &App) -> Result<()> {
    verify_manifest()?;
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(DISPLAY_FONT),
        Cow::Borrowed(UI_FONT),
        Cow::Borrowed(SERIF_FONT),
        Cow::Borrowed(SPECIMEN_FONT),
    ])
}

#[cfg(test)]
mod tests {
    use super::{DISPLAY_FAMILY, SERIF_FAMILY, SPECIMEN_FAMILY, UI_FAMILY, verify_manifest};

    #[test]
    fn roles_are_distinct_and_use_bundled_families() {
        assert_ne!(UI_FAMILY, DISPLAY_FAMILY);
        assert_ne!(UI_FAMILY, SERIF_FAMILY);
        assert_ne!(UI_FAMILY, SPECIMEN_FAMILY);
        assert_eq!(UI_FAMILY, "Instrument Sans");
        assert_eq!(DISPLAY_FAMILY, "Archivo");
        assert_eq!(SERIF_FAMILY, "Newsreader");
        assert_eq!(SPECIMEN_FAMILY, "Geist Mono");
    }

    #[test]
    fn manifest_matches_embedded_bytes() {
        verify_manifest().expect("font bytes must match the screenshot manifest");
    }
}
