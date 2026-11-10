/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Make |cargo bench| work.
#![cfg_attr(feature = "bench", feature(test))]

pub mod attr;
pub mod bloom;
mod builder;
pub mod context;
pub mod kleene_value;
pub mod matching;
mod nth_index_cache;
pub mod parser;
pub mod relative_selector;
pub mod sink;
pub mod subtree_filter;
mod tree;
pub mod visitor;

pub use crate::nth_index_cache::NthIndexCache;
pub use crate::parser::{Parser, SelectorImpl, SelectorList};
pub use crate::tree::{Element, OpaqueElement};

/// A hash map using the Fx hasher.
pub type FxHashMap<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;
/// Bounded class-token observations scoped to one native selector parse.
pub mod nudox_observer {
    use std::cell::RefCell;
    /// A token consumed by the native class-selector parser.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Event {
        /// Original parser byte start.
        pub start: usize,
        /// Original parser byte end.
        pub end: usize,
        /// Decoded identifier.
        pub name: String,
    }
    struct State { events: Vec<Event>, limit: usize, bytes: usize, used: usize, overflow: bool }
    thread_local! { static EVENTS: RefCell<Option<State>> = const { RefCell::new(None) }; }
    /// Owns a per-prelude observer and cleans it up on error or unwind.
    pub struct Guard;
    impl Drop for Guard { fn drop(&mut self) { EVENTS.with(|v| { v.borrow_mut().take(); }); } }
    impl Guard {
        /// Take the buffered component candidates.
        pub fn finish(self) -> (Vec<Event>, bool) { EVENTS.with(|v| v.borrow_mut().take().map(|s| (s.events, s.overflow)).unwrap_or_default()) }
    }
    /// Start if no selector observer is active.
    pub fn begin(limit: usize, bytes: usize) -> Option<Guard> { EVENTS.with(|v| { let mut v = v.borrow_mut(); if v.is_some() { None } else { *v = Some(State { events: Vec::new(), limit, bytes, used: 0, overflow: false }); Some(Guard) } }) }
    pub(crate) fn class(start: usize, end: usize, name: &str) { EVENTS.with(|v| { if let Some(v) = v.borrow_mut().as_mut() { if v.events.len() >= v.limit || name.len() > v.bytes.saturating_sub(v.used) { v.overflow = true; return; } v.used += name.len(); v.events.push(Event { start, end, name: name.to_owned() }); } }); }
}
