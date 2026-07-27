//! The async/streaming bridge kernel — Part III of GUI-PLAN.
//!
//! This module is the *only* place where events cross from the engine (or any
//! async runtime) into GPUI's foreground thread. Everything in here is generic
//! over the event type; no wire types (`DocEvent`, `SearchEvent`, …) appear.
//!
//! ## The Never-Block Contract (GUI-PLAN §2)
//!
//! The GPUI foreground thread runs input dispatch, state mutation, layout, and
//! paint. It must **never** block on I/O, a lock, a socket, or a large
//! computation. This module makes that contract true by construction:
//!
//! - `drain` is the **only** path by which events reach GPUI state. It awaits
//!   channel events asynchronously (yielding to the executor) and mutates state
//!   in a batched `store.update()` call.
//! - All channels are bounded `flume` channels (see `channels`). No
//!   `std::sync::mpsc`, no unbounded channels, no `block_on`.
//! - No function in this module is `async` except through the `drain` task.
//!
//! ## Module map
//!
//! | Module         | What it provides                                              |
//! |----------------|---------------------------------------------------------------|
//! | [`generation`]        | `Gen(u64)` + `GenSource` — per-slot monotonic counters        |
//! | [`handle`]     | `StreamHandle` — single-owner cancellation token             |
//! | [`slot`]       | `Phase`, `StreamSlot<T>`, `SlotError`, `Display<'_, T>`       |
//! | [`progressive`]| `Progressive<T>` — append-only streamed document accumulator |
//! | [`drain`]      | `drain()` — the batched event-to-GPUI bridge function         |
//! | [`channels`]   | `ChannelSpec` constants from GUI-PLAN Appendix C              |

pub mod channels;
pub mod drain;
pub mod generation;
pub mod handle;
pub mod progressive;
pub mod slot;
