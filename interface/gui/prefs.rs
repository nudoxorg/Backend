//! Defines the persisted reader preferences for `interface-gui`.
//! This module owns the typed file the window reads before it draws its first frame.
//! Its narrow surface keeps presentation policy out of the durable library.
//!
//! The file is a hand-written line codec, not a serialization format. It has to be readable and
//! repairable in a terminal, it has to survive an unknown key written by a newer build, and it must
//! never be the reason a window fails to open — an unreadable file yields defaults and a fault the
//! reader can see in Settings, never a refusal to start.

use std::{fs, io, path::Path, path::PathBuf};

use interface_library::WorkspaceRoot;

use crate::{
    motion::MotionPreference,
    theme::{Appearance, InterfaceSize},
};

/// How wide a panel is, in pixels, clamped to what the layout can actually honour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelWidth(f32);

impl PanelWidth {
    /// The narrowest a panel may be before its rows stop being readable.
    pub const MINIMUM: f32 = 180.0;

    /// The widest a panel may be before it starves the reader.
    pub const MAXIMUM: f32 = 520.0;

    /// Clamps a requested width into range.
    #[must_use]
    pub fn clamped(pixels: f32) -> Self {
        if pixels.is_finite() {
            Self(pixels.clamp(Self::MINIMUM, Self::MAXIMUM))
        } else {
            Self(Self::MINIMUM)
        }
    }

    /// The width in pixels.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Whether a panel is open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelState {
    /// Shown.
    Open,
    /// Collapsed to nothing.
    Closed,
}

impl PanelState {
    /// Lifts a boolean.
    #[must_use]
    pub const fn from_open(open: bool) -> Self {
        if open { Self::Open } else { Self::Closed }
    }

    /// Whether the panel is shown.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// Everything the reader has chosen that outlives one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preferences {
    /// Which of the two themes is in force.
    pub appearance: Appearance,
    /// How large the interface draws itself.
    pub size: InterfaceSize,
    /// Whether springs run.
    pub motion: MotionPreference,
    /// Whether the library panel is shown.
    pub library_panel: PanelState,
    /// Whether the context panel is shown.
    pub outline_panel: PanelState,
    /// How wide the library panel is when shown.
    pub library_width: PanelWidth,
    /// How wide the context panel is when shown.
    pub outline_width: PanelWidth,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            appearance: Appearance::Ink,
            size: InterfaceSize::Regular,
            motion: MotionPreference::Full,
            library_panel: PanelState::Open,
            outline_panel: PanelState::Open,
            library_width: PanelWidth::clamped(crate::motion::LIBRARY_PANEL_WIDTH),
            outline_width: PanelWidth::clamped(crate::motion::OUTLINE_PANEL_WIDTH),
        }
    }
}

/// The file name beneath the workspace's library directory.
pub const PREFERENCES_FILE: &str = "gui.prefs";

impl Preferences {
    /// Where preferences live for one workspace root.
    #[must_use]
    pub fn path(root: &WorkspaceRoot) -> PathBuf {
        root.library_dir().join(PREFERENCES_FILE)
    }

    /// Renders the file, one `key value` line per preference, in a stable order.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut text = String::with_capacity(192);
        text.push_str("# nudox reader preferences\n");
        push_line(&mut text, "appearance", appearance_word(self.appearance));
        push_line(&mut text, "size", size_word(self.size));
        push_line(&mut text, "motion", motion_word(self.motion));
        push_line(&mut text, "library-panel", panel_word(self.library_panel));
        push_line(&mut text, "outline-panel", panel_word(self.outline_panel));
        push_line(&mut text, "library-width", &width_word(self.library_width));
        push_line(&mut text, "outline-width", &width_word(self.outline_width));
        text
    }

    /// Reads the file, keeping the default for any line it cannot understand.
    ///
    /// A malformed line is retained in [`Decoded::unreadable`] with its exact one-based line
    /// number, so Settings can say *which* line it ignored rather than reporting a vague failure.
    #[must_use]
    pub fn decode(text: &str) -> Decoded {
        let mut preferences = Self::default();
        let mut unreadable = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(' ') else {
                unreadable.push(LineNumber(index.saturating_add(1)));
                continue;
            };
            if !preferences.apply(key.trim(), value.trim()) {
                unreadable.push(LineNumber(index.saturating_add(1)));
            }
        }
        Decoded {
            preferences,
            unreadable: unreadable.into_boxed_slice(),
        }
    }

    fn apply(&mut self, key: &str, value: &str) -> bool {
        match key {
            "appearance" => assign(&mut self.appearance, parse_appearance(value)),
            "size" => assign(&mut self.size, parse_size(value)),
            "motion" => assign(&mut self.motion, parse_motion(value)),
            "library-panel" => assign(&mut self.library_panel, parse_panel(value)),
            "outline-panel" => assign(&mut self.outline_panel, parse_panel(value)),
            "library-width" => assign(&mut self.library_width, parse_width(value)),
            "outline-width" => assign(&mut self.outline_width, parse_width(value)),
            _ => false,
        }
    }

    /// Loads preferences, returning defaults when no file has been written yet.
    pub fn load(root: &WorkspaceRoot) -> Result<Decoded, PreferencesError> {
        let path = Self::path(root);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Self::decode(&text)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Decoded {
                preferences: Self::default(),
                unreadable: Box::new([]),
            }),
            Err(source) => Err(PreferencesError::Io {
                phase: PreferencesPhase::Read,
                path: path.into_boxed_path(),
                source,
            }),
        }
    }

    /// Writes preferences through a staged file and one rename, so a torn write is impossible.
    pub fn store(&self, root: &WorkspaceRoot) -> Result<(), PreferencesError> {
        let path = Self::path(root);
        let staged = path.with_extension("staged");
        write_staged(&staged, &self.encode())?;
        fs::rename(&staged, &path).map_err(|source| PreferencesError::Io {
            phase: PreferencesPhase::Rename,
            path: path.into_boxed_path(),
            source,
        })
    }
}

fn write_staged(staged: &Path, body: &str) -> Result<(), PreferencesError> {
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent).map_err(|source| PreferencesError::Io {
            phase: PreferencesPhase::Write,
            path: parent.into(),
            source,
        })?;
    }
    fs::write(staged, body).map_err(|source| PreferencesError::Io {
        phase: PreferencesPhase::Write,
        path: staged.into(),
        source,
    })
}

/// One line of the preferences file, counted from one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineNumber(usize);

impl LineNumber {
    /// The one-based line number.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// What a decode produced: the preferences in force and the lines that were ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// Every readable preference, with defaults for the rest.
    pub preferences: Preferences,
    /// Lines the codec could not understand, in file order.
    pub unreadable: Box<[LineNumber]>,
}

/// Which filesystem step refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferencesPhase {
    /// Reading the existing file.
    Read,
    /// Writing the staged file.
    Write,
    /// Renaming the staged file into place.
    Rename,
}

/// A refusal from the preference file, with the exact path retained.
#[derive(Debug)]
pub enum PreferencesError {
    /// The filesystem refused.
    Io {
        /// Which step refused.
        phase: PreferencesPhase,
        /// The exact path involved.
        path: Box<Path>,
        /// The refusal itself.
        source: io::Error,
    },
}

fn assign<T>(slot: &mut T, parsed: Option<T>) -> bool {
    match parsed {
        Some(value) => {
            *slot = value;
            true
        }
        None => false,
    }
}

fn push_line(text: &mut String, key: &str, value: &str) {
    text.push_str(key);
    text.push(' ');
    text.push_str(value);
    text.push('\n');
}

const fn appearance_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Ink => "ink",
        Appearance::Vellum => "vellum",
    }
}

fn parse_appearance(value: &str) -> Option<Appearance> {
    Appearance::ALL
        .into_iter()
        .find(|candidate| appearance_word(*candidate) == value)
}

const fn size_word(size: InterfaceSize) -> &'static str {
    match size {
        InterfaceSize::Compact => "compact",
        InterfaceSize::Regular => "regular",
        InterfaceSize::Large => "large",
        InterfaceSize::Larger => "larger",
    }
}

fn parse_size(value: &str) -> Option<InterfaceSize> {
    InterfaceSize::ALL
        .into_iter()
        .find(|candidate| size_word(*candidate) == value)
}

const fn motion_word(motion: MotionPreference) -> &'static str {
    match motion {
        MotionPreference::Full => "full",
        MotionPreference::Reduced => "reduced",
    }
}

fn parse_motion(value: &str) -> Option<MotionPreference> {
    match value {
        "full" => Some(MotionPreference::Full),
        "reduced" => Some(MotionPreference::Reduced),
        _ => None,
    }
}

const fn panel_word(panel: PanelState) -> &'static str {
    match panel {
        PanelState::Open => "open",
        PanelState::Closed => "closed",
    }
}

fn parse_panel(value: &str) -> Option<PanelState> {
    match value {
        "open" => Some(PanelState::Open),
        "closed" => Some(PanelState::Closed),
        _ => None,
    }
}

fn width_word(width: PanelWidth) -> String {
    format!("{:.0}", width.get())
}

fn parse_width(value: &str) -> Option<PanelWidth> {
    value.parse::<f32>().ok().map(PanelWidth::clamped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Preferences {
        Preferences {
            appearance: Appearance::Vellum,
            size: InterfaceSize::Large,
            motion: MotionPreference::Reduced,
            library_panel: PanelState::Closed,
            outline_panel: PanelState::Open,
            library_width: PanelWidth::clamped(341.0),
            outline_width: PanelWidth::clamped(214.0),
        }
    }

    #[test]
    fn every_preference_survives_the_round_trip() {
        let decoded = Preferences::decode(&sample().encode());
        assert_eq!(decoded.preferences, sample());
        assert!(decoded.unreadable.is_empty());
        assert_eq!(decoded.preferences.appearance, Appearance::Vellum);
        assert_eq!(decoded.preferences.size, InterfaceSize::Large);
        assert_eq!(decoded.preferences.motion, MotionPreference::Reduced);
        assert!(!decoded.preferences.library_panel.is_open());
        assert!((decoded.preferences.library_width.get() - 341.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_unreadable_line_names_itself_and_costs_only_its_own_key() {
        let decoded = Preferences::decode(
            "# comment\nappearance vellum\nnonsense\nsize enormous\nmotion reduced\n",
        );
        assert_eq!(decoded.preferences.appearance, Appearance::Vellum);
        assert_eq!(decoded.preferences.size, InterfaceSize::Regular);
        assert_eq!(decoded.preferences.motion, MotionPreference::Reduced);
        let lines: Vec<usize> = decoded.unreadable.iter().map(|line| line.get()).collect();
        assert_eq!(lines, vec![3, 4]);
    }

    #[test]
    fn an_absurd_width_is_clamped_rather_than_refused() {
        let decoded = Preferences::decode("library-width 99999\noutline-width -4\n");
        assert!(decoded.unreadable.is_empty());
        assert!((decoded.preferences.library_width.get() - PanelWidth::MAXIMUM).abs() < f32::EPSILON);
        assert!((decoded.preferences.outline_width.get() - PanelWidth::MINIMUM).abs() < f32::EPSILON);
    }

    #[test]
    fn an_empty_file_is_exactly_the_defaults() {
        assert_eq!(Preferences::decode("").preferences, Preferences::default());
    }
}
