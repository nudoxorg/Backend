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
//! | `package`  | §L10       | Which packages are loaded, loading, or failed   |
//! | `index_jobs` | —        | On-demand `pkg:` fetches, and how far each got   |

pub mod events;
pub mod index_jobs;
pub mod job;
pub mod nav;
pub mod package;
pub mod project;
pub mod registry;
pub mod search;
pub mod search_model;
pub mod shell;
pub mod symbol;

// ─────────────────────────────────────────────────────────────────────────────
// The shipping instantiations
// ─────────────────────────────────────────────────────────────────────────────
//
// Stores are generic over a narrow engine capability trait (`SearchEngine`,
// `SymbolEngine`) so each can be driven by a test double. The *app*, though,
// has exactly one engine, and spelling `SearchStore<EngineHandle>` at every
// call site invites two failures: a second instantiation appearing by accident,
// and a `Shell` made generic over an engine it will only ever have one of.
//
// These aliases are the app's answer to "which engine", stated once.

/// The engine the shipping app runs on (LR-9).
pub type Engine = nudox_engine::EngineHandle;

/// [`search::SearchStore`] over the real engine.
pub type SearchStore = search::SearchStore<Engine>;

/// [`symbol::SymbolStore`] over the real engine.
pub type SymbolStore = symbol::SymbolStore<Engine>;

/// [`package::PackageStore`] over the real engine.
pub type PackageStore = package::PackageStore<Engine>;
