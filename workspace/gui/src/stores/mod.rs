//! Store catalog — GUI-PLAN §12 / GUI-LOCAL-PLAN §L3.
//!
//! Every store is a GPUI `Entity` that:
//!
//! 1. Holds **precomputed render-ready state** — sorted, filtered,
//!    `SharedString`-ified at *update* time, never in `render` (§1.1.4).
//! 2. Drives streams through the §7.4 shape (gen → slot → handle → drain).
//! 3. Emits cross-store events declared in `events.rs`, which form a DAG
//!    (§12.10). A cycle is a review-blocking error.
//! 4. Never blocks, never spawns detached tasks (LD-18 / §2.2).
//!
//! ## Module map
//!
//! | Module     | GUI-PLAN § | Summary                                         |
//! |------------|------------|-------------------------------------------------|
//! | `events`   | §12.10     | Inter-store event types (acyclic DAG)           |
//! | `search`   | §12.3      | Search input → debounced stream → sectioned hits|
//! | `symbol`   | §12.4      | Open symbol tabs → streamed doc pages           |
//! | `nav`      | §12.8      | Back/forward navigation history                 |
//! | `shell`    | §12.9      | Dock sizes, overlays, banners, toasts           |
//! | `project`  | §12.2      | Open projects and active-project tracking       |
//! | `registry` | §12.5      | Corpus-generation sync status                   |
//! | `job`      | §12.6      | Background job lifecycle and pipeline stages    |

pub mod events;
pub mod job;
pub mod nav;
pub mod project;
pub mod registry;
pub mod search;
pub mod shell;
pub mod symbol;
