/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Calculate [specified][specified] and [computed values][computed] from a
//! tree of DOM nodes and a set of stylesheets.
//!
//! [computed]: https://drafts.csswg.org/css-cascade/#computed
//! [specified]: https://drafts.csswg.org/css-cascade/#specified
//!
//! In particular, this crate contains the definitions of supported properties,
//! the code to parse them into specified values and calculate the computed
//! values based on the specified values, as well as the code to serialize both
//! specified and computed values.
//!
//! The main entry point is [`recalc_style_at`][recalc_style_at].
//!
//! [recalc_style_at]: traversal/fn.recalc_style_at.html
//!
//! A list of supported style properties can be found as [docs::supported_properties]
//!
//! Major dependencies are the [cssparser][cssparser] and [selectors][selectors]
//! crates.
//!
//! [cssparser]: ../cssparser/index.html
//! [selectors]: ../selectors/index.html

#![deny(missing_docs)]

// Needed by macros that refer to `$crate::cssparser`.
#[allow(clippy::single_component_path_imports)]
pub(crate) use cssparser;

#[macro_use]
extern crate bitflags;
#[macro_use]
#[cfg(feature = "gecko")]
extern crate gecko_profiler;
#[cfg(feature = "gecko")]
#[macro_use]
pub mod gecko_string_cache;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde;
pub use servo_arc;
#[cfg(feature = "servo")]
#[macro_use]
extern crate stylo_atoms;
#[macro_use]
extern crate static_assertions;

#[macro_use]
mod macros;

mod derives {
    pub(crate) use derive_more::{Add, AddAssign, Deref, DerefMut, From};
    pub(crate) use malloc_size_of_derive::MallocSizeOf;
    pub(crate) use num_derive::FromPrimitive;
    pub(crate) use style_derive::{
        Animate, ComputeSquaredDistance, Parse, SpecifiedValueInfo, ToAnimatedValue,
        ToAnimatedZero, ToComputedValue, ToCss, ToResolvedValue, ToTyped,
    };
    pub(crate) use to_shmem_derive::ToShmem;
}

pub mod applicable_declarations;
pub mod author_styles;
pub mod bezier;
pub mod bloom;
pub mod color;
#[path = "properties/computed_value_flags.rs"]
pub mod computed_value_flags;
pub mod context;
pub mod counter_style;
pub mod custom_properties;
pub mod custom_properties_map;
pub mod data;
pub mod device;
pub mod dom;
pub mod dom_apis;
pub mod driver;
pub mod error_reporting;
pub mod font_face;
pub mod font_metrics;
#[cfg(feature = "gecko")]
#[allow(unsafe_code)]
pub mod gecko_bindings;
pub mod global_style_data;
pub mod invalidation;
#[allow(missing_docs)] // TODO.
pub mod logical_geometry;
pub mod matching;
pub mod media_queries;
pub mod parallel;
pub mod parser;
pub mod piecewise_linear;
pub mod prefs;
pub mod properties_and_values;
#[macro_use]
pub mod queries;
pub mod rule_cache;
pub mod rule_collector;
pub mod rule_tree;
pub mod scoped_tls;
pub mod selector_map;
pub mod selector_parser;
pub mod shared_lock;
pub mod sharing;
mod simple_buckets_map;
pub mod str;
pub mod style_adjuster;
pub mod style_resolver;
pub mod stylesheet_set;
pub mod stylesheets;
pub mod stylist;
pub mod thread_state;
pub mod traversal;
pub mod traversal_flags;
pub mod typed_om;
pub mod url;
pub mod use_counters;

#[macro_use]
#[allow(non_camel_case_types)]
pub mod values;

#[cfg(all(doc, feature = "servo"))]
/// Documentation
pub mod docs {
    /// The CSS properties supported by the style system.
    /// Generated from the `properties.mako.rs` template by `build.rs`
    pub mod supported_properties {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/css-properties.html"))]
    }
}

#[cfg(feature = "gecko")]
pub use crate::gecko_string_cache as string_cache;
#[cfg(feature = "gecko")]
pub use crate::gecko_string_cache::Atom;
/// The namespace prefix type for Gecko, which is just an atom.
#[cfg(feature = "gecko")]
pub type Prefix = crate::values::AtomIdent;
/// The local name of an element for Gecko, which is just an atom.
#[cfg(feature = "gecko")]
pub type LocalName = crate::values::AtomIdent;
#[cfg(feature = "gecko")]
pub use crate::gecko_string_cache::Namespace;

#[cfg(feature = "servo")]
pub use stylo_atoms::Atom;

#[cfg(feature = "servo")]
#[allow(missing_docs)]
pub type LocalName = crate::values::GenericAtomIdent<web_atoms::LocalNameStaticSet>;
#[cfg(feature = "servo")]
#[allow(missing_docs)]
pub type Namespace = crate::values::GenericAtomIdent<web_atoms::NamespaceStaticSet>;
#[cfg(feature = "servo")]
#[allow(missing_docs)]
pub type Prefix = crate::values::GenericAtomIdent<web_atoms::PrefixStaticSet>;

pub use style_traits::arc_slice::ArcSlice;
pub use style_traits::owned_array::OwnedArray;
pub use style_traits::owned_slice::OwnedSlice;
pub use style_traits::owned_str::OwnedStr;

use std::hash::{BuildHasher, Hash};

#[cfg_attr(feature = "servo", macro_use)]
pub mod properties;

#[cfg(feature = "gecko")]
#[allow(unsafe_code)]
pub mod gecko;

// uses a macro from properties
#[cfg(feature = "servo")]
#[allow(unsafe_code)]
pub mod servo;
#[cfg(feature = "servo")]
pub use servo::{animation, attr};

macro_rules! reexport_computed_values {
    ( $( { $name: ident } )+ ) => {
        /// Types for [computed values][computed].
        ///
        /// [computed]: https://drafts.csswg.org/css-cascade/#computed
        pub mod computed_values {
            $(
                pub use crate::properties::longhands::$name::computed_value as $name;
            )+
            // Don't use a side-specific name needlessly:
            pub use crate::properties::longhands::border_top_style::computed_value as border_style;
        }
    }
}
longhand_properties_idents!(reexport_computed_values);
#[cfg(feature = "gecko")]
use crate::gecko_string_cache::WeakAtom;
#[cfg(feature = "servo")]
use stylo_atoms::Atom as WeakAtom;

/// Extension methods for selectors::attr::CaseSensitivity
pub trait CaseSensitivityExt {
    /// Return whether two atoms compare equal according to this case sensitivity.
    fn eq_atom(self, a: &WeakAtom, b: &WeakAtom) -> bool;
}

impl CaseSensitivityExt for selectors::attr::CaseSensitivity {
    #[inline]
    fn eq_atom(self, a: &WeakAtom, b: &WeakAtom) -> bool {
        match self {
            selectors::attr::CaseSensitivity::CaseSensitive => a == b,
            selectors::attr::CaseSensitivity::AsciiCaseInsensitive => a.eq_ignore_ascii_case(b),
        }
    }
}

/// A trait pretty much similar to num_traits::Zero, but without the need of
/// implementing `Add`.
pub trait Zero {
    /// Returns the zero value.
    fn zero() -> Self;

    /// Returns whether this value is zero.
    fn is_zero(&self) -> bool;
}

impl<T> Zero for T
where
    T: num_traits::Zero,
{
    fn zero() -> Self {
        <Self as num_traits::Zero>::zero()
    }

    fn is_zero(&self) -> bool {
        <Self as num_traits::Zero>::is_zero(self)
    }
}

/// A trait implementing a function to tell if the number is zero without a percent
pub trait ZeroNoPercent {
    /// So, `0px` should return `true`, but `0%` or `1px` should return `false`
    fn is_zero_no_percent(&self) -> bool;
}

/// A trait pretty much similar to num_traits::One, but without the need of
/// implementing `Mul`.
pub trait One {
    /// Reutrns the one value.
    fn one() -> Self;

    /// Returns whether this value is one.
    fn is_one(&self) -> bool;
}

impl<T> One for T
where
    T: num_traits::One + PartialEq,
{
    fn one() -> Self {
        <Self as num_traits::One>::one()
    }

    fn is_one(&self) -> bool {
        *self == One::one()
    }
}

/// An allocation error.
///
/// TODO(emilio): Would be nice to have more information here, or for SmallVec
/// to return the standard error type (and then we can just return that).
///
/// But given we use these mostly to bail out and ignore them, it's not a big
/// deal.
#[derive(Debug)]
pub struct AllocErr;

impl From<smallvec::CollectionAllocErr> for AllocErr {
    #[inline]
    fn from(_: smallvec::CollectionAllocErr) -> Self {
        Self
    }
}

impl From<std::collections::TryReserveError> for AllocErr {
    #[inline]
    fn from(_: std::collections::TryReserveError) -> Self {
        Self
    }
}

impl From<hashbrown::TryReserveError> for AllocErr {
    #[inline]
    fn from(_: hashbrown::TryReserveError) -> Self {
        Self
    }
}

/// A hash map using the Fx hasher.
pub type FxHashMap<K, V> = hashbrown::HashMap<K, V, rustc_hash::FxBuildHasher>;

/// A hash set using the Fx hasher.
pub type FxHashSet<T> = hashbrown::HashSet<T, rustc_hash::FxBuildHasher>;

/// Shrink the capacity of the collection if needed.
pub(crate) trait ShrinkIfNeeded {
    fn shrink_if_needed(&mut self);
}

/// We shrink the capacity of a collection if we're wasting more than a 25% of
/// its capacity, and if the collection is arbitrarily big enough
/// (>= CAPACITY_THRESHOLD entries).
#[inline]
fn should_shrink(len: usize, capacity: usize) -> bool {
    const CAPACITY_THRESHOLD: usize = 64;
    capacity >= CAPACITY_THRESHOLD && len + capacity / 4 < capacity
}

impl<K, V, H> ShrinkIfNeeded for hashbrown::HashMap<K, V, H>
where
    K: Eq + Hash,
    H: BuildHasher,
{
    fn shrink_if_needed(&mut self) {
        if should_shrink(self.len(), self.capacity()) {
            self.shrink_to_fit();
        }
    }
}

impl<T, H> ShrinkIfNeeded for hashbrown::HashSet<T, H>
where
    T: Eq + Hash,
    H: BuildHasher,
{
    fn shrink_if_needed(&mut self) {
        if should_shrink(self.len(), self.capacity()) {
            self.shrink_to_fit();
        }
    }
}

// TODO(emilio): Measure and see if we're wasting a lot of memory on Vec /
// SmallVec, and if so consider shrinking those as well.

/// Bounded, thread-local observation for accepted top-level CSS rules.
/// The native frontend validates host bytes and exposes only its declared partial scope.
pub mod nudox_observer {
    use std::{cell::{Cell, RefCell}, marker::PhantomData, rc::Rc};
    /// Native parser accepted a declaration before block deduplication.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Event {
        /// Parser byte start before declaration tokenization.
        pub start: usize,
        /// Parser byte end after accepted declaration value.
        pub end: usize,
        /// Decoded property name supplied to native parser.
        pub name: String,
        /// Native generated property identifiers before deduplication.
        pub property_ids: Vec<String>,
        /// Top-level native style rule ordinal, committed only on insertion.
        pub owner_rule: Option<usize>,
    }
    /// Accepted simple class candidate from the native selector parser.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ClassEvent {
        /// Top-level native style rule ordinal, committed only on insertion.
        pub owner_rule: Option<usize>,
        /// Start of selector-list prelude.
        pub prelude_start: usize,
        /// End of selector-list prelude.
        pub prelude_end: usize,
        /// Index of the selector in its native list.
        pub selector_ordinal: usize,
        /// Byte start of the original name token.
        pub start: usize,
        /// Byte end of the original name token.
        pub end: usize,
        /// Native decoded class name.
        pub name: String,
    }
    struct State { id: u64, host: [u8; 32], budget: usize, byte_budget: usize, used: usize, overflow: bool, parse_depth: usize, nest_depth: usize, frame: Option<(usize, usize)>, skipped: bool, events: Vec<Event>, classes: Vec<ClassEvent> }
    thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; static NEXT_ID: Cell<u64> = const { Cell::new(0) }; }
    /// Active observation scoped to one thread and host token.
    pub struct Guard { id: u64, _not_send: PhantomData<Rc<()>> }
    impl Drop for Guard {
        fn drop(&mut self) { STATE.with(|state| { let mut s = state.borrow_mut(); if s.as_ref().is_some_and(|v| v.id == self.id) { *s = None; } }); }
    }
    impl Guard {
        /// Retrieve events, host token, overflow and skipped-syntax status.
        /// Rule ownership is limited to committed top-level style rules.
        pub fn finish(self) -> Option<([u8; 32], Vec<Event>, Vec<ClassEvent>, bool, bool)> {
            STATE.with(|state| { let mut s = state.borrow_mut(); if s.as_ref().is_some_and(|v| v.id == self.id && v.parse_depth == 0) { s.take().map(|mut s| { if let Some((events, classes)) = s.frame.take() { s.events.truncate(events); s.classes.truncate(classes); s.skipped = true; } (s.host, s.events, s.classes, s.overflow, s.skipped) }) } else { None } })
        }
    }
    /// Begin observation, rejecting reentrant independent invocations on the same thread.
    pub fn begin(host: [u8; 32], budget: usize, byte_budget: usize) -> Option<Guard> {
        STATE.with(|state| { let mut s = state.borrow_mut(); if s.is_some() { return None; } let id = NEXT_ID.with(|v| { let n = v.get().wrapping_add(1); v.set(n); n }); *s = Some(State { id, host, budget, byte_budget, used: 0, overflow: false, parse_depth: 0, nest_depth: 0, frame: None, skipped: false, events: Vec::new(), classes: Vec::new() }); Some(Guard { id, _not_send: PhantomData }) })
    }
    pub(crate) struct ParseScope { id: u64 }
    impl Drop for ParseScope {
        fn drop(&mut self) { STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() { if s.id == self.id { s.parse_depth = s.parse_depth.saturating_sub(1); } } }); }
    }
    pub(crate) fn enter_parse() -> Option<ParseScope> {
        STATE.with(|state| { let mut s = state.borrow_mut(); let s = s.as_mut()?; s.parse_depth += 1; Some(ParseScope { id: s.id }) })
    }
    pub(crate) fn begin_rule() { STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() { if s.parse_depth == 1 { if s.frame.is_some() { s.skipped = true; } else { s.frame = Some((s.events.len(), s.classes.len())); } } } }); }
    pub(crate) fn finish_rule(owner: Option<usize>) { STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() { if s.parse_depth != 1 { return; } if let Some((events, classes)) = s.frame.take() { if let Some(owner) = owner { for e in &mut s.events[events..] { e.owner_rule = Some(owner); } for e in &mut s.classes[classes..] { e.owner_rule = Some(owner); } } else { s.events.truncate(events); s.classes.truncate(classes); s.skipped = true; } } } }); }
    pub(crate) struct NestScope { id: u64 }
    impl Drop for NestScope { fn drop(&mut self) { STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() { if s.id == self.id { s.nest_depth = s.nest_depth.saturating_sub(1); } } }); } }
    pub(crate) fn enter_nest() -> Option<NestScope> { STATE.with(|state| { let mut s = state.borrow_mut(); let s = s.as_mut()?; if s.parse_depth != 1 { return None; } s.nest_depth += 1; Some(NestScope { id: s.id }) }) }
    pub(crate) fn budgets() -> Option<(usize, usize)> { STATE.with(|state| { let mut s = state.borrow_mut(); let s = s.as_mut()?; if s.parse_depth != 1 { return None; } if s.frame.is_none() || s.nest_depth != 0 { s.skipped = true; return None; } Some((s.budget.saturating_sub(s.events.len() + s.classes.len()), s.byte_budget.saturating_sub(s.used))) }) }
    pub(crate) fn skip() { STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() { if s.parse_depth == 1 { s.skipped = true; } } }); }
    pub(crate) fn class_events(prelude_start: usize, prelude_end: usize, events: Vec<(usize, selectors::nudox_observer::Event)>, overflow: bool) {
        STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() {
            if s.parse_depth != 1 { return; }
            if s.frame.is_none() || s.nest_depth != 0 { s.skipped = true; return; }
            if overflow { s.overflow = true; return; }
            for (selector_ordinal, event) in events {
                if s.events.len() + s.classes.len() >= s.budget || event.name.len() > s.byte_budget.saturating_sub(s.used) { s.overflow = true; return; }
                s.used += event.name.len();
                s.classes.push(ClassEvent { owner_rule: None, prelude_start, prelude_end, selector_ordinal, start: event.start, end: event.end, name: event.name });
            }
        }});
    }
    pub(crate) fn accepted(start: usize, end: usize, name: &str, ids: impl FnOnce() -> Vec<String>) {
        STATE.with(|state| { if let Some(s) = state.borrow_mut().as_mut() {
            if s.parse_depth != 1 { return; }
            if s.frame.is_none() || s.nest_depth != 1 { s.skipped = true; return; }
            if s.events.len() + s.classes.len() >= s.budget || name.len() > s.byte_budget.saturating_sub(s.used) { s.overflow = true; return; }
            let property_ids = ids();
            let Some(cost) = property_ids.iter().try_fold(name.len(), |sum, id| sum.checked_add(id.len())) else { s.overflow = true; return; };
            if cost > s.byte_budget.saturating_sub(s.used) { s.overflow = true; return; }
            s.used += cost;
            s.events.push(Event { start, end, name: name.to_owned(), property_ids, owner_rule: None });
        }});
    }
}
