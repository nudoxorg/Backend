//! The keys the sidebar reads while it has the keyboard (the shelf zone is
//! the active one).
//!
//! The shell binds plain letters (`j`, `k`, `s`, `g`, `f`, `h`, `t`) to its
//! commands, and GPUI runs a bound action before any key listener. So the
//! shelf does not wait for a listener: while its zone is the active one it
//! intercepts the keystroke itself (`App::intercept_keystrokes` runs before
//! the bindings are matched, and stopping propagation there keeps the
//! shell's letter commands from firing). It steps aside exactly where the
//! shell's plain bindings do: in `Input`, `Menu`, `Graph`, `BrowseCompare`
//! and hint contexts.
//!
//! - Every letter and symbol is typed and narrows the scope.
//! - `G` then `C` / `V` / `R` / `U` picks the lens (Linear's chords). Any
//!   other key after `G` makes the `G` an ordinary letter of the query, and
//!   so does waiting: the chord is only pending for [`CHORD_WINDOW`].
//! - `←` and `→` step out and hoist (or close and open a group); ⌫ takes a
//!   character back; Esc clears the narrowing.
//! - Arrows, ↵, Space and Tab stay the shell's.

use super::lens::Lens;
use gpui::Keystroke;
use std::time::Duration;

/// How long `G` waits for the letter of a lens before it is just a `g`.
pub(super) const CHORD_WINDOW: Duration = Duration::from_millis(700);

/// One key the sidebar reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SideKey {
    /// A character to type (shifted letters arrive shifted).
    Char(char),
    /// ⌫.
    Backspace,
    /// ←.
    Left,
    /// →.
    Right,
    /// Esc.
    Escape,
    /// ↑.
    Up,
    /// ↓.
    Down,
    /// Space.
    Space,
}

/// Reads a keystroke as a sidebar key, or `None` when it is not one (a
/// chord with ⌘, ⌃, ⌥ or fn, or a key the shell owns).
pub(super) fn decode(keystroke: &Keystroke) -> Option<SideKey> {
    let mods = keystroke.modifiers;
    if mods.platform || mods.control || mods.alt || mods.function {
        return None;
    }
    match keystroke.key.as_str() {
        "backspace" => return Some(SideKey::Backspace),
        "left" => return Some(SideKey::Left),
        "right" => return Some(SideKey::Right),
        "escape" => return Some(SideKey::Escape),
        "up" => return Some(SideKey::Up),
        "down" => return Some(SideKey::Down),
        "space" => return Some(SideKey::Space),
        _ => {}
    }
    // A real key event carries the character it types; a synthesised one
    // carries only the key's name.
    let typed = keystroke.key_char.as_deref().unwrap_or(keystroke.key.as_str());
    let mut chars = typed.chars();
    let (Some(character), None) = (chars.next(), chars.next()) else { return None };
    if character.is_control() || character.is_whitespace() {
        return None;
    }
    let shifted = mods.shift && keystroke.key_char.is_none();
    Some(SideKey::Char(if shifted { character.to_ascii_uppercase() } else { character }))
}

/// Whether words are already being typed: a chord only starts on an empty
/// query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Query {
    /// Nothing is typed.
    Empty,
    /// Words are being typed.
    Typing,
}

impl Query {
    /// `Empty` when nothing is typed.
    pub(super) const fn of(is_empty: bool) -> Self {
        if is_empty { Self::Empty } else { Self::Typing }
    }
}

/// Whether a peek is on a row (`G G` is then the graph of that row).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Peek {
    /// No peek is showing.
    Closed,
    /// A peek is on the row the keyboard is on.
    On,
}

impl Peek {
    /// `On` when a peek is open.
    pub(super) const fn of(is_open: bool) -> Self {
        if is_open { Self::On } else { Self::Closed }
    }
}

/// What a typed character came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Typed {
    /// Goes on the end of the query.
    Text(String),
    /// A lens chord completed.
    Lens(Lens),
    /// `G G`: the graph of the row the peek is on.
    Graph,
    /// `G` is waiting for its letter.
    Armed,
}

/// The `G` chord's state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Chord {
    /// Nothing pending.
    #[default]
    Idle,
    /// `G` was typed on an empty query.
    Armed,
}

impl Chord {
    /// Feeds one typed character. A chord only starts on an empty query:
    /// once you are typing, `g` is a letter. `G G` is the graph only while a
    /// peek is on a row; otherwise it is the word `gg`.
    pub(super) fn feed(&mut self, character: char, query: Query, peek: Peek) -> Typed {
        match *self {
            Self::Armed => {
                *self = Self::Idle;
                match Lens::from_chord(character) {
                    Some(lens) => Typed::Lens(lens),
                    None if peek == Peek::On && character.eq_ignore_ascii_case(&'g') => Typed::Graph,
                    None => Typed::Text(format!("g{character}")),
                }
            }
            Self::Idle if query == Query::Empty && character.eq_ignore_ascii_case(&'g') => {
                *self = Self::Armed;
                Typed::Armed
            }
            Self::Idle => Typed::Text(character.to_string()),
        }
    }

    /// The pending `G` turns out to be a letter (the window passed, or
    /// another key came): the text it stands for.
    pub(super) fn flush(&mut self) -> Option<&'static str> {
        match std::mem::take(self) {
            Self::Armed => Some("g"),
            Self::Idle => None,
        }
    }

    /// Whether `G` is waiting.
    pub(super) const fn is_armed(self) -> bool {
        matches!(self, Self::Armed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(source: &str) -> Option<SideKey> {
        decode(&Keystroke::parse(source).expect("keystroke"))
    }

    #[test]
    fn letters_and_symbols_are_typed_and_the_shells_own_keys_and_modified_chords_are_not_read() {
        assert_eq!(press("a"), Some(SideKey::Char('a')));
        assert_eq!(press("_"), Some(SideKey::Char('_')));
        assert_eq!(press("shift-a"), Some(SideKey::Char('A')));
        assert_eq!(press("backspace"), Some(SideKey::Backspace));
        assert_eq!(press("left"), Some(SideKey::Left));
        assert_eq!(press("right"), Some(SideKey::Right));
        assert_eq!(press("escape"), Some(SideKey::Escape));
        assert_eq!(press("up"), Some(SideKey::Up));
        assert_eq!(press("down"), Some(SideKey::Down));
        assert_eq!(press("space"), Some(SideKey::Space));
        for shell_key in ["enter", "tab", "cmd-d", "ctrl-1", "alt-x", "cmd-k", "cmd-up", "shift-tab"] {
            assert_eq!(press(shell_key), None, "{shell_key} is not the sidebar's to read");
        }
        let typed = Keystroke { key: "a".into(), key_char: Some("é".into()), modifiers: gpui::Modifiers::none() };
        assert_eq!(decode(&typed), Some(SideKey::Char('é')), "the character a key types wins over its name");
    }

    #[test]
    fn g_then_a_lens_letter_is_the_chord() {
        for lens in Lens::ALL {
            let mut chord = Chord::default();
            assert_eq!(chord.feed('g', Query::Empty, Peek::Closed), Typed::Armed);
            assert!(chord.is_armed());
            assert_eq!(chord.feed(lens.chord(), Query::Empty, Peek::Closed), Typed::Lens(lens));
            assert!(!chord.is_armed());
        }
    }

    #[test]
    fn g_then_any_other_key_is_the_two_letters_of_a_word() {
        let mut chord = Chord::default();
        assert_eq!(chord.feed('g', Query::Empty, Peek::Closed), Typed::Armed);
        assert_eq!(chord.feed('e', Query::Empty, Peek::Closed), Typed::Text("ge".into()), "typing get is not a chord");
        assert_eq!(chord.feed('t', Query::Typing, Peek::Closed), Typed::Text("t".into()));
    }

    #[test]
    fn g_g_is_the_graph_only_while_a_peek_is_on_a_row() {
        let mut chord = Chord::default();
        assert_eq!(chord.feed('g', Query::Empty, Peek::On), Typed::Armed);
        assert_eq!(chord.feed('g', Query::Empty, Peek::On), Typed::Graph);
        assert_eq!(chord.feed('g', Query::Empty, Peek::Closed), Typed::Armed);
        assert_eq!(chord.feed('g', Query::Empty, Peek::Closed), Typed::Text("gg".into()), "with no peek it is the word");
    }

    #[test]
    fn a_g_in_the_middle_of_a_query_is_a_letter_and_a_g_left_waiting_is_a_letter_too() {
        let mut chord = Chord::default();
        assert_eq!(chord.feed('g', Query::Typing, Peek::Closed), Typed::Text("g".into()), "the chord only starts on an empty query");
        assert_eq!(chord.feed('G', Query::Empty, Peek::Closed), Typed::Armed, "a shifted G chords too");
        assert_eq!(chord.flush(), Some("g"), "the window passed: it was a g");
        assert_eq!(chord.flush(), None, "and it is not pending any more");
    }
}
