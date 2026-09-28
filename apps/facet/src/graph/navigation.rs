//! Pure graph navigation. One exploration owns the reading surface; typed
//! events describe the next state and return effects for the native view.
use super::{cold_tour::{TourIntent, TourResolution}, discovery::Chain, interaction::Reach, model::NodeId};
use crate::semantics::tour::Tour;
use super::prism::SlotKey;
use std::sync::Arc;
#[derive(Clone, Default)]
pub(crate) enum Exploration {
    #[default] Free,
    Reach(Arc<Reach>),
    PreparingTour(TourIntent),
    TourUnavailable { package: u32 },
    Tour { data: Tour, at: usize },
    Chain(Chain),
}
impl Exploration {
    pub(crate) fn preparing_tour(&self) -> Option<TourIntent> { if let Self::PreparingTour(intent) = self { Some(*intent) } else { None } }
    pub(crate) fn unavailable_tour(&self) -> Option<u32> { if let Self::TourUnavailable { package } = self { Some(*package) } else { None } }
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
    pub(crate) pending_accept: Option<u64>,
}
pub(crate) enum Event {
    Focus(Option<NodeId>), Pan, OpenFind, CloseFind, QueryChanged, Walk(usize, SlotKey),
    Reach(Arc<Reach>), PrepareTour(u32, usize), ResolveTour(TourIntent, TourResolution),
    Tour(Tour, usize), TourStep(usize), HoldChain(Chain), Suspend, Escape,
    AwaitResult, ResultsReady(u64, bool),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect { None, AcceptResult, Focus(Option<NodeId>), CloseFind, BackOut(NodeId), World, FlyTour(NodeId), FrameReach, FrameChain }
impl Navigation {
    pub(crate) fn apply(&mut self, event: Event) -> Effect {
        match event {
            Event::Focus(node) => { self.find_open = false; self.invalidate(); self.focus = node; self.prism_sel = None; self.selected = None; self.exploration = Exploration::Free; Effect::Focus(node) }
            Event::Pan => { self.invalidate(); self.focus = None; self.prism_sel = None; self.selected = None; self.exploration = Exploration::Free; Effect::None }
            Event::OpenFind => { self.find_open = true; self.exploration = Exploration::Free; self.prism_sel = None; self.selected = None; self.result_sel = 0; self.invalidate(); Effect::None }
            Event::CloseFind => { self.find_open = false; self.result_sel = 0; self.invalidate(); Effect::CloseFind }
            Event::QueryChanged => { self.result_sel = 0; self.invalidate(); Effect::None }
            Event::Walk(row, key) => { if self.focus.is_some() && !self.find_open && matches!(self.exploration, Exploration::Free) { self.prism_sel = Some(row); self.selected = Some(key); } Effect::None }
            Event::Reach(data) => { if self.focus != Some(data.source) || self.find_open { return Effect::None; } self.prism_sel = None; self.selected = None; self.exploration = Exploration::Reach(data); Effect::FrameReach }
            Event::PrepareTour(package, at) => {
                if self.find_open { return Effect::None; }
                self.invalidate(); self.prism_sel = None; self.selected = None;
                self.exploration = Exploration::PreparingTour(TourIntent { package, at, epoch: self.generation });
                Effect::None
            }
            Event::ResolveTour(intent, resolution) => {
                if self.find_open || self.generation != intent.epoch || self.exploration.preparing_tour() != Some(intent) { return Effect::None; }
                match resolution {
                    TourResolution::Stale => Effect::None,
                    TourResolution::Unavailable => { self.exploration = Exploration::TourUnavailable { package: intent.package }; Effect::None }
                    TourResolution::Ready { data, at } => {
                        if data.package != intent.package || !data.shown() { return Effect::None; }
                        self.apply(Event::Tour(data, at))
                    }
                }
            }
            Event::AwaitResult => {
                if self.find_open { self.pending_accept = Some(self.generation); }
                Effect::None
            }
            Event::ResultsReady(generation, has_results) => {
                if !self.accepts(generation) || self.pending_accept != Some(generation) { return Effect::None; }
                self.pending_accept = None;
                if has_results { Effect::AcceptResult } else { Effect::None }
            }
            Event::Suspend => { self.invalidate(); Effect::None }
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
                    Exploration::PreparingTour(_) | Exploration::TourUnavailable { .. } => { self.invalidate(); self.exploration = Exploration::Free; return Effect::None; }
                    Exploration::Reach(_) => { self.exploration = Exploration::Free; return Effect::Focus(self.focus); }
                    Exploration::Tour { .. } | Exploration::Chain(_) => { self.exploration = Exploration::Free; return Effect::None; }
                    Exploration::Free => {}
                }
                if self.prism_sel.take().is_some() { self.selected = None; return Effect::None; }
                self.focus.take().map_or(Effect::World, Effect::BackOut)
            }
        }
    }
    fn invalidate(&mut self) { self.pending_accept = None; if self.exploration.preparing_tour().is_some() { self.exploration = Exploration::Free; } self.generation = self.generation.wrapping_add(1); }
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
                match &state.exploration { Exploration::Reach(r) => assert_eq!(state.focus, Some(r.source)), Exploration::Tour { data, at } => { assert!(*at < data.stops.len()); assert_eq!(state.focus, None); }, Exploration::Chain(_) => assert_eq!(state.focus, None), Exploration::PreparingTour(_) | Exploration::TourUnavailable { .. } | Exploration::Free => {} }
            }
        }
    }

    #[test]
    fn cold_tour_is_owned_by_navigation_and_cannot_replay_after_leaving() {
        use super::super::cold_tour::TourResolution;
        for cancel in [Event::Focus(Some(3)), Event::OpenFind, Event::CloseFind, Event::QueryChanged, Event::Pan, Event::Escape, Event::Suspend] {
            let mut state = Navigation::default();
            state.apply(Event::Focus(Some(5))); state.apply(Event::PrepareTour(1, 1));
            let intent = state.exploration.preparing_tour().expect("pending reading intent");
            state.apply(cancel);
            assert_eq!(state.apply(Event::ResolveTour(intent, intent.resolve(state.generation, Some(&tour())))), Effect::None);
            assert!(state.exploration.tour().is_none());
            assert!(state.exploration.preparing_tour().is_none());
        }
        let mut state = Navigation::default(); state.apply(Event::PrepareTour(1, 1));
        let intent = state.exploration.preparing_tour().expect("tour is preparing");
        assert!(matches!(state.apply(Event::ResolveTour(intent, intent.resolve(state.generation, Some(&tour())))), Effect::FlyTour(_)));
        assert_eq!(state.apply(Event::ResolveTour(intent, TourResolution::Ready { data: tour(), at: 1 })), Effect::None, "delivery is one-shot");
    }

    #[test]
    fn latest_cold_tour_wins_and_unavailable_is_truthful() {
        let mut state = Navigation::default(); state.apply(Event::PrepareTour(0, 0));
        let old = state.exploration.preparing_tour().expect("first tour is preparing");
        state.apply(Event::PrepareTour(1, 2)); let latest = state.exploration.preparing_tour().expect("replacement tour is preparing");
        assert_ne!(old.epoch, latest.epoch);
        assert_eq!(state.apply(Event::ResolveTour(old, old.resolve(state.generation, Some(&tour())))), Effect::None);
        assert_eq!(state.exploration.preparing_tour(), Some(latest));
        assert_eq!(state.apply(Event::ResolveTour(latest, latest.resolve(state.generation, None))), Effect::None);
        assert_eq!(state.exploration.unavailable_tour(), Some(1));
        assert!(state.exploration.tour().is_none());
        state.apply(Event::Escape); assert!(state.exploration.unavailable_tour().is_none());
    }
    #[test]
    fn pending_result_acceptance_is_generation_owned_and_one_shot() {
        for cancel in [Event::QueryChanged, Event::CloseFind, Event::Escape, Event::Focus(Some(5)), Event::Pan, Event::Suspend] {
            let mut state = Navigation::default(); state.apply(Event::OpenFind); state.apply(Event::AwaitResult);
            let generation = state.generation; assert_eq!(state.pending_accept, Some(generation));
            state.apply(cancel);
            assert_eq!(state.apply(Event::ResultsReady(generation, true)), Effect::None);
            assert_eq!(state.pending_accept, None);
        }
        let mut state = Navigation::default(); state.apply(Event::OpenFind); state.apply(Event::AwaitResult);
        let generation = state.generation;
        assert_eq!(state.apply(Event::ResultsReady(generation.wrapping_sub(1), true)), Effect::None);
        assert_eq!(state.pending_accept, Some(generation), "stale result cannot consume the current Enter");
        assert_eq!(state.apply(Event::ResultsReady(generation, true)), Effect::AcceptResult);
        assert_eq!(state.apply(Event::ResultsReady(generation, true)), Effect::None);
        state.apply(Event::AwaitResult);
        assert_eq!(state.apply(Event::ResultsReady(generation, false)), Effect::None);
        assert!(state.find_open && state.pending_accept.is_none(), "no matches retain an editable query without pending navigation");
    }
}
