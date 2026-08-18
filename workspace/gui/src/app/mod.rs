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

//! # The windowless half (`lifecycle` + `menus`)
//!
//! lindsey hosts an MCP endpoint (`mcp`), which means the process has to outlive
//! its window and still be reachable and still be quittable. [`lifecycle`] owns
//! that state machine and [`menus`] owns the only input surface that survives a
//! dismissal — they live here rather than under `workspace::` because neither
//! may depend on `Shell`: the window is rebuilt *by* them.

pub mod account;
pub mod actions;
pub mod corpus;
pub mod desktop;
pub mod keymaps;
pub mod lifecycle;
pub mod mcp;
pub mod menus;
