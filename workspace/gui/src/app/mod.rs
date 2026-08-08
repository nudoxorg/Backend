//! Application-level wiring: actions, the keymap registry (GUI-PLAN §13.6), and
//! corpus selection (GUI-LOCAL-PLAN §L10).
//!
//! # Why these live together
//!
//! An action nobody can invoke and a binding pointing at nothing are the same
//! bug seen from two sides. Keeping the declarations ([`actions`]) beside the
//! bindings ([`keymaps`]) is what makes the tests "every binding names a real
//! action" and "every action is reachable" possible to write at all.
//!
//! # The registry is the single source of discoverability (LD-13)
//!
//! [`keymaps`] exposes a queryable registry of
//! `(binding, action, context, description)`. The command palette (§23.1) and
//! the `?` cheat sheet (§23.3) both render *from that registry* rather than from
//! hand-maintained lists — so a shortcut cannot exist while being
//! undiscoverable, which is the failure mode every app with a growing keymap
//! eventually reaches.

pub mod actions;
pub mod corpus;
pub mod keymaps;
pub mod mcp;
