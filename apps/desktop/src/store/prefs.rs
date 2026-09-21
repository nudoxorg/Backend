//! Typed, atomically written preferences with a hand-rolled codec.
//! One `key = value` line per setting, no dynamic document model, no partial
//! writes: the file is written to a sibling temporary and renamed into place.
//!
//! Unknown keys and unparseable values are ignored rather than fatal, and every
//! field has a default, so a file written by a newer build still loads here and
//! a corrupted file degrades to defaults instead of to an empty window.
//! Preferences load before the first frame, which is why the codec is
//! synchronous and allocation-light.

use super::persist;
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
///
/// Three of the eight fields are booleans, and they stay booleans: each one is
/// a single independent switch a reader flips, and folding them into a bitset
/// or a state enum would make the file format less readable without removing a
/// single state from the product.
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent reader switches, one line each in a human-readable file"
)]
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
        let _ = writeln!(text, "schema = 1");
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
        Self::decode_with_diagnostic(text).0
    }

    fn decode_with_diagnostic(text: &str) -> (Self, Option<PreferenceDiagnostic>) {
        // Files written before schema tagging are legacy v1. They remain
        // admissible so an upgrade does not erase a reader's layout; every
        // newly written file is tagged by `encode` above.
        let mut prefs = Self::default();
        let mut diagnostic = None;
        let mut incompatible_schema = false;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                if !line.trim().is_empty() {
                    diagnostic = Some(PreferenceDiagnostic::Malformed);
                }
                continue;
            };
            if !prefs.apply(key.trim(), value.trim()) {
                diagnostic = Some(PreferenceDiagnostic::Malformed);
                if key.trim() == "schema" {
                    incompatible_schema = true;
                }
            }
        }
        if incompatible_schema {
            return (Self::default(), diagnostic);
        }
        (
            prefs.with_widths(prefs.library_width, prefs.context_width),
            diagnostic,
        )
    }

    fn apply(&mut self, key: &str, value: &str) -> bool {
        match key {
            "schema" => {
                if value != "1" {
                    return false;
                }
            }
            "appearance" => {
                let Some(appearance) = Appearance::parse(value) else {
                    return false;
                };
                self.appearance = appearance;
            }
            "interface" => {
                let Ok(value) = value.parse::<u16>() else {
                    return false;
                };
                self.interface = InterfaceSize::percent(value);
            }
            "library-width" => {
                let Ok(value) = value.parse::<f32>() else {
                    return false;
                };
                if !value.is_finite() {
                    return false;
                }
                self.library_width = value;
            }
            "context-width" => {
                let Ok(value) = value.parse::<f32>() else {
                    return false;
                };
                if !value.is_finite() {
                    return false;
                }
                self.context_width = value;
            }
            "library-open" => {
                let Some(value) = parse_bool(value) else {
                    return false;
                };
                self.library_open = value;
            }
            "context-open" => {
                let Some(value) = parse_bool(value) else {
                    return false;
                };
                self.context_open = value;
            }
            "reduced-motion" => {
                let Some(value) = parse_bool(value) else {
                    return false;
                };
                self.reduced_motion = value;
            }
            "editor" => {
                let Some(editor) = EditorScheme::parse(value) else {
                    return false;
                };
                self.editor = editor;
            }
            _ => return true,
        }
        true
    }
}

/// Typed reason a preference file was not admitted exactly as written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreferenceDiagnostic {
    /// No file exists yet; defaults are the expected first-run state.
    Missing,
    /// The file exceeded the bounded reader budget.
    Oversized,
    /// The file could not be read or decoded as UTF-8.
    Unreadable,
    /// At least one known setting was malformed.
    Malformed,
}

/// Preferences plus a bounded, machine-checkable startup observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LoadedPreferences {
    /// Value installed in the shell.
    pub(crate) preferences: Preferences,
    /// Why defaults or field-level fallback was used, when applicable.
    pub(crate) diagnostic: Option<PreferenceDiagnostic>,
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
    load_with_diagnostic(data).preferences
}

/// Reads preferences and retains a typed recovery observation for startup.
pub(crate) fn load_with_diagnostic(data: &Path) -> LoadedPreferences {
    let path = path_in(data);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedPreferences {
                preferences: Preferences::default(),
                diagnostic: Some(PreferenceDiagnostic::Missing),
            };
        }
        Err(_) => {
            return LoadedPreferences {
                preferences: Preferences::default(),
                diagnostic: Some(PreferenceDiagnostic::Unreadable),
            };
        }
    };
    if metadata.len() > MAX_BYTES {
        return LoadedPreferences {
            preferences: Preferences::default(),
            diagnostic: Some(PreferenceDiagnostic::Oversized),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (preferences, diagnostic) = Preferences::decode_with_diagnostic(&text);
            LoadedPreferences {
                preferences,
                diagnostic: diagnostic.or_else(|| {
                    text.trim()
                        .is_empty()
                        .then_some(PreferenceDiagnostic::Malformed)
                }),
            }
        }
        Err(_) => LoadedPreferences {
            preferences: Preferences::default(),
            diagnostic: Some(PreferenceDiagnostic::Unreadable),
        },
    }
}

/// Writes preferences atomically beside the workspace data directory.
///
/// A torn preferences file would cost the reader their layout on the next
/// launch, so the bytes land in a sibling temporary and are renamed into place.
pub(crate) fn save(data: &Path, prefs: Preferences) -> Result<(), std::io::Error> {
    let path = path_in(data);
    persist::atomic_write(&path, prefs.encode().as_bytes())
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn data_directory(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nudox-preferences-{label}-{}-{}",
            std::process::id(),
            FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("preferences fixture");
        root
    }

    #[test]
    fn save_then_cold_load_preserves_every_user_setting() {
        let root = data_directory("restart");
        let expected = Preferences::default()
            .with_appearance(Appearance::Vellum)
            .with_interface(InterfaceSize::percent(125))
            .with_widths(310.0, 240.0)
            .with_open(false, true)
            .with_reduced_motion(true)
            .with_editor(EditorScheme::Zed);
        save(&root, expected).expect("save preferences");
        let encoded = fs::read_to_string(path_in(&root)).expect("read encoded preferences");
        assert!(encoded.starts_with("schema = 1\n"));
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.preferences, expected);
        assert_eq!(loaded.diagnostic, None);
        fs::remove_dir_all(root).expect("remove preferences fixture");
    }

    #[test]
    fn legacy_schema_less_preferences_are_admitted_as_v1() {
        let root = data_directory("legacy-v1");
        fs::write(
            path_in(&root),
            "appearance = vellum\ninterface = 115\nreduced-motion = true\n",
        )
        .expect("write legacy preferences");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.preferences.appearance(), Appearance::Vellum);
        assert_eq!(loaded.preferences.interface().get(), 115);
        assert!(loaded.preferences.reduced_motion());
        assert_eq!(loaded.diagnostic, None);
        fs::remove_dir_all(root).expect("remove legacy preferences fixture");
    }

    #[test]
    fn truncated_or_schema_incompatible_preferences_fail_closed_with_a_typed_reason() {
        let root = data_directory("corrupt");
        fs::write(path_in(&root), "appearance = vellum\ninterface =").expect("truncated file");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.preferences.appearance(), Appearance::Vellum);
        assert_eq!(loaded.diagnostic, Some(PreferenceDiagnostic::Malformed));

        fs::write(path_in(&root), "schema = 99\nappearance = vellum\n").expect("future schema");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.preferences, Preferences::default());
        assert_eq!(loaded.diagnostic, Some(PreferenceDiagnostic::Malformed));
        fs::remove_dir_all(root).expect("remove preferences fixture");
    }

    #[test]
    fn oversized_preferences_are_not_partially_admitted() {
        let root = data_directory("oversized");
        fs::write(
            path_in(&root),
            vec![b'x'; (MAX_BYTES as usize).saturating_add(1)],
        )
        .expect("oversized file");
        let loaded = load_with_diagnostic(&root);
        assert_eq!(loaded.preferences, Preferences::default());
        assert_eq!(loaded.diagnostic, Some(PreferenceDiagnostic::Oversized));
        fs::remove_dir_all(root).expect("remove preferences fixture");
    }
}
