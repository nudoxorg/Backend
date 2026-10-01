//! The graph's own keys, in the words a host lists them (Settings › Keys).
//! The handler is [`GraphView`](super::GraphView)'s `key` / `find_key`
//! (`view.rs`); while the graph has the keyboard these keys are its own,
//! and the host's bindings for the same keys step aside.

/// One key the graph answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    /// Its cap(s), as shown.
    pub cap: &'static str,
    /// What it does.
    pub says: &'static str,
}

/// Every key the graph answers, in the order a newcomer needs them.
pub const KEYS: &[Key] = &[
    Key {
        cap: "/  ⌘K",
        says: "find a symbol, a shape or a road",
    },
    Key {
        cap: "↵",
        says: "open the focus's page (a walked row: focus it)",
    },
    Key {
        cap: "←  →  ↑  ↓",
        says: "walk the rows beside the focus; with none, pan",
    },
    Key {
        cap: "T",
        says: "tour the package here; again to stop",
    },
    Key {
        cap: "Space  →",
        says: "the tour's next stop (← the one before)",
    },
    Key {
        cap: "R",
        says: "what a change here reaches; again to return",
    },
    Key {
        cap: "+  −",
        says: "zoom",
    },
    Key {
        cap: "0",
        says: "the whole world",
    },
    Key {
        cap: "Esc",
        says: "back out one step",
    },
];
