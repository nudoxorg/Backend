//! Pure graph navigation. One exploration owns the reading surface; typed
//! events describe the next state and return effects for the native view.
use super::{discovery::Chain, interaction::Reach, model::NodeId};
use crate::semantics::tour::Tour;
use super::prism::SlotKey;
use std::sync::Arc;
#[derive(Clone, Default)]
pub(crate) enum Exploration {
    #[default] Free,
    Reach(Arc<Reach>),
    Tour { data: Tour, at: usize },
    Chain(Chain),
}
impl Exploration {
    pub(crate) fn reach(&self) -> Option<&Arc<Reach>> { if let Self::Reach(data) = self { Some(data) } else { None } }
    pub(crate) fn tour(&self) -> Option<(&Tour, usize)> { if let Self::Tour { data, at } = self { Some((data, *at)) } else { None } }
    pub(crate) fn chain(&self) -> Option<&Chain> { if let Self::Chain(data) = self { Some(data) } else { None } }
}
#[derive(Clone, Default)]
pub(crate) struct Navigation {
    pub(crate) focus: Option<NodeId>,
    pub(crate) prism_sel: Option<usize>,
    pub(crate) selected: Option<SlotKey>,
    pub(crate) find_open: bool,
    pub(crate) result_sel: usize,
    pub(crate) exploration: Exploration,
    pub(crate) generation: u64,
}
pub(crate) enum Event {
    Focus(Option<NodeId>), Pan, OpenFind, CloseFind, QueryChanged, Walk(usize, SlotKey),
    Reach(Arc<Reach>), Tour(Tour, usize), TourStep(usize), HoldChain(Chain), Escape,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect { None, Focus(Option<NodeId>), CloseFind, BackOut(NodeId), World, FlyTour(NodeId), FrameReach, FrameChain }
impl Navigation {
    pub(crate) fn apply(&mut self, event: Event) -> Effect {
        match event {
            Event::Focus(node) => { self.find_open = false; self.invalidate(); self.focus = node; self.prism_sel = None; self.selected = None; self.exploration = Exploration::Free; Effect::Focus(node) }
            Event::Pan => { self.focus = None; self.prism_sel = None; self.selected = None; self.exploration = Exploration::Free; Effect::None }
            Event::OpenFind => { self.find_open = true; self.exploration = Exploration::Free; self.prism_sel = None; self.selected = None; self.result_sel = 0; self.invalidate(); Effect::None }
            Event::CloseFind => { self.find_open = false; self.result_sel = 0; self.invalidate(); Effect::CloseFind }
            Event::QueryChanged => { self.result_sel = 0; self.invalidate(); Effect::None }
            Event::Walk(row, key) => { if self.focus.is_some() && !self.find_open && matches!(self.exploration, Exploration::Free) { self.prism_sel = Some(row); self.selected = Some(key); } Effect::None }
            Event::Reach(data) => { if self.focus != Some(data.source) || self.find_open { return Effect::None; } self.prism_sel = None; self.selected = None; self.exploration = Exploration::Reach(data); Effect::FrameReach }
            Event::Tour(data, at) => {
                if !data.shown() { return Effect::None; }
                let at = at.min(data.stops.len() - 1); let node = data.stops[at].node;
                self.find_open = false; self.invalidate(); self.focus = None; self.prism_sel = None; self.selected = None;
                self.exploration = Exploration::Tour { data, at }; Effect::FlyTour(node)
            }
            Event::TourStep(next) => { let Exploration::Tour { data, at } = &mut self.exploration else { return Effect::None; }; let next = next.min(data.stops.len() - 1); if *at == next { return Effect::None; } *at = next; Effect::FlyTour(data.stops[next].node) }
            Event::HoldChain(data) => { self.find_open = false; self.invalidate(); self.focus = None; self.prism_sel = None; self.selected = None; self.exploration = Exploration::Chain(data); Effect::FrameChain }
            Event::Escape => {
                if self.find_open { return self.apply(Event::CloseFind); }
                match self.exploration {
                    Exploration::Reach(_) => { self.exploration = Exploration::Free; return Effect::Focus(self.focus); }
                    Exploration::Tour { .. } | Exploration::Chain(_) => { self.exploration = Exploration::Free; return Effect::None; }
                    Exploration::Free => {}
                }
                if self.prism_sel.take().is_some() { self.selected = None; return Effect::None; }
                self.focus.take().map_or(Effect::World, Effect::BackOut)
            }
        }
    }
    fn invalidate(&mut self) { self.generation = self.generation.wrapping_add(1); }
    pub(crate) fn accepts(&self, generation: u64) -> bool { self.find_open && self.generation == generation }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{graph::model::tests::tiny, semantics::tour::{Role, Stop, Why}};
    fn tour() -> Tour { Tour { package: 1, items: 3, stops: vec![Stop { node: 3, role: Role::StartHere, why: Why::CallFirst }, Stop { node: 4, role: Role::WhatItPromises, why: Why::OthersImplement }, Stop { node: 5, role: Role::WhenItFails, why: Why::YouHandle }] } }
    fn chain() -> Chain { Chain { cost: 2.0, from: "#0".into(), output: "text".into(), via: None, steps: vec![], path: vec![0, 3], stops: vec![], brief: "road".into(), rail: "road".into(), code: "page.read()".into() } }
    #[test]
    fn escape_priority_and_latest_query_are_explicit() {
        let mut state = Navigation::default(); state.apply(Event::Focus(Some(5))); state.apply(Event::Walk(2, SlotKey { node: 3, side: 1, word: crate::semantics::Word::Calls }));
        assert_eq!(state.apply(Event::Escape), Effect::None); assert_eq!(state.focus, Some(5)); assert_eq!(state.prism_sel, None);
        state.apply(Event::Reach(Arc::new(Reach::of(&tiny(), 5)))); assert_eq!(state.apply(Event::Escape), Effect::Focus(Some(5)));
        state.apply(Event::OpenFind); let old = state.generation; state.apply(Event::QueryChanged); assert!(!state.accepts(old));
        let current = state.generation; assert!(state.accepts(current)); assert_eq!(state.apply(Event::Escape), Effect::CloseFind); assert!(!state.accepts(current));
        assert_eq!(state.apply(Event::Escape), Effect::BackOut(5)); assert_eq!(state.apply(Event::Escape), Effect::World);
    }
    #[test]
    fn seeded_command_storms_preserve_one_owner_and_valid_stops() {
        let reach = Arc::new(Reach::of(&tiny(), 5));
        for seed in 0..32_u64 {
            let mut rng = seed + 1; let mut state = Navigation::default();
            for _ in 0..512 {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                let event = match (rng >> 32) % 11 { 0 => Event::Focus(Some(5)), 1 => Event::Focus(None), 2 => Event::OpenFind, 3 => Event::CloseFind, 4 => Event::QueryChanged, 5 => Event::Reach(reach.clone()), 6 => Event::Tour(tour(), (rng as usize) % 12), 7 => Event::TourStep((rng as usize) % 12), 8 => Event::HoldChain(chain()), 9 => Event::Walk((rng as usize) % 8, SlotKey { node: 3, side: 1, word: crate::semantics::Word::Calls }), _ => Event::Escape };
                state.apply(event);
                if state.find_open { assert!(matches!(state.exploration, Exploration::Free)); }
                match &state.exploration { Exploration::Reach(r) => assert_eq!(state.focus, Some(r.source)), Exploration::Tour { data, at } => { assert!(*at < data.stops.len()); assert_eq!(state.focus, None); }, Exploration::Chain(_) => assert_eq!(state.focus, None), Exploration::Free => {} }
            }
        }
    }
}
