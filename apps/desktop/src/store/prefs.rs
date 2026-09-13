//! Typed, atomically written preferences with a hand-rolled codec.
//! One `key = value` line per setting, no dynamic document model, no partial
//! writes: the file is written to a sibling temporary and renamed into place.
//!
//! Unknown keys and unparseable values are ignored rather than fatal, and every
//! field has a default, so a file written by a newer build still loads here and
//! a corrupted file degrades to defaults instead of to an empty window.
//! Preferences load before the first frame, which is why the codec is
//! synchronous and allocation-light.

use crate::theme::palette::Appearance;
use crate::theme::tokens::{InterfaceSize, PanelWidth};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// File name of the preferences file inside the workspace data directory.
pub(crate) const PREFS_FILE: &str = "desktop.prefs";

/// Largest preferences file this build will read.
const MAX_BYTES: u64 = 64 * 1024;

/// Which external editor an "open in editor" affordance targets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum EditorScheme {
    /// Cursor: `cursor://file/<path>:<line>`.
    #[default]
    Cursor,
    /// Visual Studio Code: `vscode://file/<path>:<line>`.
    VsCode,
    /// Zed: `zed://file/<path>:<line>`.
    Zed,
    /// No external editor; the affordance reveals the file instead.
    Reveal,
}

impl EditorScheme {
    /// Every scheme, in settings order.
    pub(crate) const ALL: [Self; 4] = [Self::Cursor, Self::VsCode, Self::Zed, Self::Reveal];

    /// Returns the stable name written to the preferences file.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::VsCode => "vscode",
            Self::Zed => "zed",
            Self::Reveal => "reveal",
        }
    }

    /// Returns the reader-facing label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::VsCode => "Visual Studio Code",
            Self::Zed => "Zed",
            Self::Reveal => "Reveal in Finder",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scheme| scheme.name() == text)
    }

    /// Builds the URL that opens one source site, when this scheme has one.
    pub(crate) fn url(self, path: &Path, line: u32) -> Option<String> {
        let spelling = path.to_string_lossy();
        match self {
            Self::Cursor => Some(format!("cursor://file/{spelling}:{line}")),
            Self::VsCode => Some(format!("vscode://file/{spelling}:{line}")),
            Self::Zed => Some(format!("zed://file/{spelling}:{line}")),
            Self::Reveal => None,
        }
    }
}

/// Every persisted preference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Preferences {
    appearance: Appearance,
    interface: InterfaceSize,
    library_width: f32,
    context_width: f32,
    library_open: bool,
    context_open: bool,
    reduced_motion: bool,
    editor: EditorScheme,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            appearance: Appearance::Ink,
            interface: InterfaceSize::DEFAULT,
            library_width: PanelWidth::DEFAULT_LIBRARY,
            context_width: PanelWidth::DEFAULT_CONTEXT,
            library_open: true,
            context_open: true,
            reduced_motion: false,
            editor: EditorScheme::Cursor,
        }
    }
}

impl Preferences {
    /// Returns the lit appearance.
    pub(crate) const fn appearance(self) -> Appearance {
        self.appearance
    }

    /// Returns the reading size.
    pub(crate) const fn interface(self) -> InterfaceSize {
        self.interface
    }

    /// Returns the library panel width in pixels.
    pub(crate) const fn library_width(self) -> f32 {
        self.library_width
    }

    /// Returns the context panel width in pixels.
    pub(crate) const fn context_width(self) -> f32 {
        self.context_width
    }

    /// Returns whether the library panel is open.
    pub(crate) const fn library_open(self) -> bool {
        self.library_open
    }

    /// Returns whether the context panel is open.
    pub(crate) const fn context_open(self) -> bool {
        self.context_open
    }

    /// Returns whether motion is suppressed.
    pub(crate) const fn reduced_motion(self) -> bool {
        self.reduced_motion
    }

    /// Returns the external editor scheme.
    pub(crate) const fn editor(self) -> EditorScheme {
        self.editor
    }

    /// Returns these preferences with a different appearance.
    pub(crate) const fn with_appearance(mut self, appearance: Appearance) -> Self {
        self.appearance = appearance;
        self
    }

    /// Returns these preferences with a different reading size.
    pub(crate) const fn with_interface(mut self, interface: InterfaceSize) -> Self {
        self.interface = interface;
        self
    }

    /// Returns these preferences with different panel widths.
    pub(crate) fn with_widths(mut self, library: f32, context: f32) -> Self {
        self.library_width = library.clamp(PanelWidth::MIN_LIBRARY, PanelWidth::MAX_LIBRARY);
        self.context_width = context.clamp(PanelWidth::MIN_CONTEXT, PanelWidth::MAX_CONTEXT);
        self
    }

    /// Returns these preferences with different panel visibility.
    pub(crate) const fn with_open(mut self, library: bool, context: bool) -> Self {
        self.library_open = library;
        self.context_open = context;
        self
    }

    /// Returns these preferences with motion suppressed or restored.
    pub(crate) const fn with_reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }

    /// Returns these preferences with a different external editor.
    pub(crate) const fn with_editor(mut self, editor: EditorScheme) -> Self {
        self.editor = editor;
        self
    }

    /// Encodes the preferences as the exact file text.
    pub(crate) fn encode(self) -> String {
        let mut text = String::with_capacity(224);
        let _ = writeln!(text, "appearance = {}", self.appearance.name());
        let _ = writeln!(text, "interface = {}", self.interface.get());
        let _ = writeln!(text, "library-width = {:.0}", self.library_width);
        let _ = writeln!(text, "context-width = {:.0}", self.context_width);
        let _ = writeln!(text, "library-open = {}", self.library_open);
        let _ = writeln!(text, "context-open = {}", self.context_open);
        let _ = writeln!(text, "reduced-motion = {}", self.reduced_motion);
        let _ = writeln!(text, "editor = {}", self.editor.name());
        text
    }

    /// Decodes preferences, keeping defaults for anything missing or malformed.
    pub(crate) fn decode(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            prefs.apply(key.trim(), value.trim());
        }
        prefs.with_widths(prefs.library_width, prefs.context_width)
    }

    fn apply(&mut self, key: &str, value: &str) {
        match key {
            "appearance" => self.appearance = Appearance::parse(value).unwrap_or(self.appearance),
            "interface" => {
                self.interface = value
                    .parse::<u16>()
                    .map_or(self.interface, InterfaceSize::percent);
            }
            "library-width" => self.library_width = value.parse().unwrap_or(self.library_width),
            "context-width" => self.context_width = value.parse().unwrap_or(self.context_width),
            "library-open" => self.library_open = parse_bool(value).unwrap_or(self.library_open),
            "context-open" => self.context_open = parse_bool(value).unwrap_or(self.context_open),
            "reduced-motion" => {
                self.reduced_motion = parse_bool(value).unwrap_or(self.reduced_motion);
            }
            "editor" => self.editor = EditorScheme::parse(value).unwrap_or(self.editor),
            _ => {}
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Returns the preferences path beside one workspace data directory.
pub(crate) fn path_in(data: &Path) -> PathBuf {
    data.join(PREFS_FILE)
}

/// Reads preferences, falling back to defaults for any read or parse failure.
pub(crate) fn load(data: &Path) -> Preferences {
    let path = path_in(data);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Preferences::default();
    };
    if metadata.len() > MAX_BYTES {
        return Preferences::default();
    }
    std::fs::read_to_string(&path)
        .map_or_else(|_| Preferences::default(), |text| Preferences::decode(&text))
}

/// Writes preferences atomically beside the workspace data directory.
///
/// A torn preferences file would cost the reader their layout on the next
/// launch, so the bytes land in a sibling temporary and are renamed into place.
pub(crate) fn save(data: &Path, prefs: Preferences) -> Result<(), std::io::Error> {
    let path = path_in(data);
    let temporary = data.join(format!("{PREFS_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, prefs.encode())?;
    match std::fs::rename(&temporary, &path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}
