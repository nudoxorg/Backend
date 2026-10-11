//! The graph's own keys, in the words a host lists them (Settings › Keys).
//! The handler is [`GraphView`](super::GraphView)'s `key` / `find_key`
//! (`view.rs`); while the graph has the keyboard these keys are its own,
//! and the host's bindings for the same keys step aside.

use gpui::Keystroke;

/// The same secondary modifier as the host's Find shortcut.
pub const FIND_CHORD: &str = "secondary-k";

/// Local query editing is available without declaration activation authority.
pub(super) fn opens_find(stroke: &Keystroke) -> bool {
    let mods = stroke.modifiers;
    (stroke.key == "/" && !mods.platform && !mods.control && !mods.alt)
        || (stroke.key == "k" && mods.secondary() && !mods.alt)
}

/// One key the graph answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    /// Its cap(s), as shown.
    pub cap: &'static str,
    /// A native chord shown beside its plain cap, when present.
    pub shortcut: Option<&'static str>,
    /// What it does.
    pub says: &'static str,
}

impl Key {
    /// Its complete caption, using the component library's native formatter.
    #[must_use]
    pub fn cap(&self) -> String {
        self.shortcut.map_or_else(|| self.cap.to_owned(), |chord| {
            let chord = Keystroke::parse(chord).expect("static graph key chord");
            format!("{}  {}", self.cap, gpui_component::kbd::Kbd::format(&chord))
        })
    }
}

/// Every key the graph answers, in the order a newcomer needs them.
pub const KEYS: &[Key] = &[
    Key { cap: "/", shortcut: Some(FIND_CHORD), says: "find a symbol, a shape or a road" },
    Key { cap: "↵", shortcut: None, says: "open the focus's page (a walked row: focus it)" },
    Key { cap: "←  →  ↑  ↓", shortcut: None, says: "walk the rows beside the focus; with none, pan" },
    Key { cap: "T", shortcut: None, says: "tour the package here; again to stop" },
    Key { cap: "Space  →", shortcut: None, says: "the tour's next stop (← the one before)" },
    Key { cap: "R", shortcut: None, says: "what a change here reaches; again to return" },
    Key { cap: "+  −", shortcut: None, says: "zoom" },
    Key { cap: "0", shortcut: None, says: "the whole world" },
    Key { cap: "Esc", shortcut: None, says: "back out one step" },
];
