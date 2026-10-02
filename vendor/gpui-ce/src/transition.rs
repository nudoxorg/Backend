use std::cell::{Cell, Ref, RefCell};

use scheduler::Instant;

use crate::{Animated, App, Entity, EntityId, Lerp, Motion, Progress, Window};

/// An animated transition between values of type `T`.
///
/// `Transition` retains its logical target in an entity and uses GPUI's
/// [`Animated`] sampler for time, interpolation, interruption, and completion.
/// The executor clock is used so headless tests and rendered frames share the
/// same timeline.
#[derive(Clone)]
pub struct Transition<T: Lerp + Clone + PartialEq + 'static> {
    motion: Motion,
    state: Entity<TransitionState<T>>,
    cached_value: RefCell<Option<T>>,
    cached_progress: RefCell<Option<Progress>>,
    cached_at: RefCell<Option<Instant>>,
    cached_revision: Cell<Option<u64>>,
    continuous: bool,
}

impl<T: Lerp + Clone + PartialEq + 'static> Transition<T> {
    /// Creates a transition over the supplied motion and state.
    pub fn new(state: Entity<TransitionState<T>>, motion: impl Into<Motion>) -> Self {
        Self {
            motion: motion.into(),
            state,
            cached_value: RefCell::new(None),
            cached_progress: RefCell::new(None),
            cached_at: RefCell::new(None),
            cached_revision: Cell::new(None),
            continuous: true,
        }
    }

    /// Sets the forward pass's easing function.
    pub fn with_easing(mut self, easing: impl Fn(f32) -> f32 + 'static) -> Self {
        self.motion = self.motion.with_easing(easing);
        self.clear_cache();
        self
    }

    /// Chooses whether a changed target continues from the current presentation.
    pub fn continuous(mut self, continuous: bool) -> Self {
        self.continuous = continuous;
        self
    }

    fn clear_cache(&self) {
        self.cached_value.borrow_mut().take();
        self.cached_progress.borrow_mut().take();
        self.cached_at.borrow_mut().take();
        self.cached_revision.set(None);
    }

    /// Evaluates the current value without using the per-render cache.
    fn raw_evaluate_at(&self, cx: &mut App, now: Instant) -> (bool, T, Progress) {
        let mut state = self.state.as_mut(cx);
        let sample = state.animated.sample(now);
        (sample.is_active, sample.value, sample.progress)
    }

    fn raw_evaluate(&self, cx: &mut App) -> (bool, T, Progress) {
        let now = cx.background_executor().now();
        self.raw_evaluate_at(cx, now)
    }

    /// Evaluates the transition and requests a frame while its motion is active.
    ///
    /// The result is cached for the current executor-clock instant and sampled
    /// again when the clock advances.
    pub fn evaluate(&self, window: &mut Window, cx: &mut App) -> Ref<'_, T> {
        let now = cx.background_executor().now();
        let revision = self.state.read(cx).revision;
        let cache_is_current = *self.cached_at.borrow() == Some(now)
            && self.cached_revision.get() == Some(revision)
            && self.cached_value.borrow().is_some();
        if !cache_is_current {
            let (active, value, progress) = self.raw_evaluate_at(cx, now);
            *self.cached_value.borrow_mut() = Some(value);
            *self.cached_progress.borrow_mut() = Some(progress);
            *self.cached_at.borrow_mut() = Some(now);
            self.cached_revision.set(Some(revision));
            if active {
                window.request_animation_frame();
            }
        }

        Ref::map(self.cached_value.borrow(), |value| value.as_ref().unwrap())
    }

    /// Reads the transition's logical target.
    pub fn read_goal<'b>(&'b self, cx: &'b mut App) -> &'b T {
        self.state.read(cx).value()
    }

    /// Reads the value cached by [`Transition::evaluate`], if any.
    pub fn read_cache(&self) -> Ref<'_, Option<T>> {
        self.cached_value.borrow()
    }

    /// Returns the eased presentation progress at the executor's current time.
    /// The easing curve may overshoot zero through one.
    pub fn evaluate_delta(&self, cx: &App) -> f32 {
        let now = cx.background_executor().now();
        let revision = self.state.read(cx).revision;
        if *self.cached_at.borrow() == Some(now) && self.cached_revision.get() == Some(revision) {
            if let Some(progress) = *self.cached_progress.borrow() {
                return progress.get();
            }
        }
        self.state.read(cx).progress_at(now).get()
    }

    /// Updates the logical target.
    ///
    /// Continuous transitions sample their current presentation and retarget
    /// from that value. Non-continuous transitions restart at their initial value.
    /// Returns whether the target changed.
    pub fn update<R>(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut T, &mut crate::Context<TransitionState<T>>) -> R,
    ) -> bool {
        let now = cx.background_executor().now();
        let mut was_updated = false;

        self.state.update(cx, |state, cx| {
            let mut target = state.value().clone();
            update(&mut target, cx);
            was_updated = if self.continuous {
                state.animated.set(target, &self.motion, now)
            } else {
                state.animated.restart(target, &self.motion, now)
            };
            if was_updated {
                state.revision = state.revision.wrapping_add(1);
            }
        });

        if was_updated {
            self.clear_cache();
        }
        was_updated
    }

    /// Sets the target immediately without animation.
    pub fn jump_to(&self, target: T, cx: &mut App) {
        self.state.update(cx, |state, _| {
            state.animated.jump_to(target);
            state.revision = state.revision.wrapping_add(1);
        });
        self.clear_cache();
    }

    /// Scales both the current presentation and target by `ratio`.
    pub fn scale_by(&self, ratio: f32, cx: &mut App)
    where
        T: std::ops::Mul<f32, Output = T>,
    {
        self.state.update(cx, |state, _| {
            state.animated.scale_by(ratio);
            state.revision = state.revision.wrapping_add(1);
        });
        self.clear_cache();
    }

    /// Returns the entity ID associated with this transition.
    pub fn entity_id(&self) -> EntityId {
        self.state.entity_id()
    }

    /// Resets the logical and sampled values to the initial goal.
    pub fn reset(&self, cx: &mut App) {
        self.state.update(cx, |state, _| {
            state.animated.reset();
            state.revision = state.revision.wrapping_add(1);
        });
        self.clear_cache();
    }
}

/// The animated value stored by the legacy transition hooks.
#[derive(Clone)]
pub struct TransitionState<T: Lerp + Clone + PartialEq + 'static> {
    animated: Animated<T, Instant>,
    revision: u64,
}

impl<T: Lerp + Clone + PartialEq + 'static> TransitionState<T> {
    /// Creates a completed state at `initial_goal`.
    pub fn new(initial_goal: T) -> Self {
        Self {
            animated: Animated::new(initial_goal, Motion::default()),
            revision: 0,
        }
    }

    /// Returns the current logical target.
    pub fn value(&self) -> &T {
        self.animated.value()
    }

    fn progress_at(&self, now: Instant) -> Progress {
        self.animated.progress_at(now)
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use std::time::Duration;

    use crate::{
        AppContext, Context, Render, TestAppContext, WindowHandle, div, prelude::*, px, size,
    };

    use super::*;

    fn create_transition<T: Lerp + Clone + PartialEq + 'static>(
        cx: &mut App,
        motion: impl Into<Motion>,
        initial: T,
    ) -> Transition<T> {
        let state = cx.new(|_| TransitionState::new(initial));
        Transition::new(state, motion)
    }

    #[gpui::test]
    fn transition_uses_executor_time_and_retargets_continuously(cx: &mut TestAppContext) {
        let transition = cx.update(|cx| {
            let transition = create_transition(cx, Duration::from_secs(1), 0.0_f32);
            assert!(transition.update(cx, |value, _| *value = 10.0));
            let (active, value, _) = transition.raw_evaluate(cx);
            assert!(active);
            assert_eq!(value, 0.0);
            transition
        });

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.update(|cx| {
            let (active, value, _) = transition.raw_evaluate(cx);
            assert!(active);
            assert_eq!(value, 5.0);

            assert!(transition.update(cx, |value, _| *value = 20.0));
            let (_, value, _) = transition.raw_evaluate(cx);
            assert_eq!(value, 5.0, "retargeting must preserve current presentation");
        });

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.update(|cx| {
            let (active, value, _) = transition.raw_evaluate(cx);
            assert!(active, "retargeting starts a fresh duration-based pass");
            assert_eq!(value, 12.5);
        });

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.update(|cx| {
            let (active, value, _) = transition.raw_evaluate(cx);
            assert!(!active);
            assert_eq!(value, 20.0);
        });
    }

    #[gpui::test]
    fn transition_supports_motion_delay_and_alternating_finite_playback(cx: &mut TestAppContext) {
        let transition = cx.update(|cx| {
            let motion = Motion::new(Duration::from_millis(100))
                .with_delay(Duration::from_millis(50))
                .iterations(2)
                .alternate();
            let transition = create_transition(cx, motion, 0.0_f32);
            transition.update(cx, |value, _| *value = 10.0);
            transition
        });

        cx.executor().advance_clock(Duration::from_millis(25));
        cx.update(|cx| {
            let (active, value, _) = transition.raw_evaluate(cx);
            assert!(active);
            assert_eq!(value, 0.0);
        });

        cx.executor().advance_clock(Duration::from_millis(225));
        cx.update(|cx| {
            let (active, value, progress) = transition.raw_evaluate(cx);
            assert!(!active);
            assert_eq!(value, 0.0);
            assert_eq!(progress, Progress::START);
        });
    }

    #[gpui::test]
    fn non_continuous_transition_restarts_from_its_initial_value(cx: &mut TestAppContext) {
        let transition = cx.update(|cx| {
            let transition =
                create_transition(cx, Duration::from_secs(1), 0.0_f32).continuous(false);
            transition.update(cx, |value, _| *value = 10.0);
            transition
        });

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.update(|cx| {
            let (_, value, _) = transition.raw_evaluate(cx);
            assert_eq!(value, 5.0);
            assert!(transition.update(cx, |value, _| *value = 20.0));
            let (_, value, _) = transition.raw_evaluate(cx);
            assert_eq!(value, 0.0);
        });
    }

    #[gpui::test]
    fn legacy_transition_mutators_keep_their_value_contract(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transition = create_transition(cx, Duration::from_secs(1), 2.0_f32);
            assert!(transition.update(cx, |value, _| *value = 6.0));
            assert!(!transition.update(cx, |value, _| *value = 6.0));

            transition.jump_to(8.0, cx);
            assert_eq!(transition.raw_evaluate(cx).1, 8.0);

            transition.update(cx, |value, _| *value = 10.0);
            transition.scale_by(2.0, cx);
            assert_eq!(transition.raw_evaluate(cx).1, 16.0);

            transition.reset(cx);
            assert_eq!(transition.raw_evaluate(cx).1, 2.0);
            assert_eq!(*transition.read_goal(cx), 2.0);
        });
    }

    #[gpui::test]
    fn cloned_transitions_share_state_but_independent_transitions_do_not(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let first = create_transition(cx, Duration::from_secs(1), 1.0_f32);
            let first_clone = first.clone();
            let second = create_transition(cx, Duration::from_secs(1), 9.0_f32);

            assert!(first.update(cx, |value, _| *value = 4.0));
            assert_eq!(*first_clone.read_goal(cx), 4.0);
            assert_eq!(*second.read_goal(cx), 9.0);

            assert!(first_clone.update(cx, |value, _| *value = 5.0));
            assert_eq!(*first.read_goal(cx), 5.0);
            assert_eq!(*second.read_goal(cx), 9.0);
        });
    }

    #[gpui::test]
    fn reduced_motion_can_jump_to_the_target(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let transition = create_transition(cx, Duration::from_secs(1), 0.0_f32);
            assert!(transition.update(cx, |value, _| *value = 1.0));
            transition.jump_to(1.0, cx);
            let (active, value, progress) = transition.raw_evaluate(cx);
            assert!(!active);
            assert_eq!(value, 1.0);
            assert_eq!(progress, Progress::END);
        });
    }

    #[test]
    fn transition_state_starts_at_its_initial_value() {
        let state = TransitionState::new(42.0_f32);
        assert_eq!(state.value(), &42.0);
    }

    struct TransitionCacheTestView {
        samples: std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>,
    }

    impl Render for TransitionCacheTestView {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            let transition = window.use_transition(cx, Duration::from_secs(1), |_, _| 0.0_f32);
            transition.update(cx, |value, _| *value = 10.0);
            let value = *transition.evaluate(window, cx);
            let progress = transition.evaluate_delta(cx);
            self.samples.borrow_mut().push((value, progress));
            div().size_full()
        }
    }

    #[gpui::test]
    fn transition_cache_is_scoped_to_the_executor_clock_instant(cx: &mut TestAppContext) {
        let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let window: WindowHandle<TransitionCacheTestView> =
            cx.open_window(size(px(100.), px(100.)), {
                let samples = samples.clone();
                move |_, _| TransitionCacheTestView { samples }
            });
        cx.run_until_parked();
        assert_eq!(samples.borrow().last(), Some(&(0.0, 0.0)));

        cx.executor().advance_clock(Duration::from_millis(500));
        window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(samples.borrow().last(), Some(&(5.0, 0.5)));
    }

    struct TransitionCloneCacheTestView {
        samples: std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>,
    }

    impl Render for TransitionCloneCacheTestView {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            if self.samples.borrow().is_empty() {
                let transition = window.use_transition(cx, Duration::from_secs(1), |_, _| 0.0_f32);
                transition.update(cx, |value, _| *value = 10.0);
                let cloned = transition.clone();

                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));

                cloned.jump_to(5.0, cx);
                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));

                let restarting = cloned.clone().continuous(false);
                restarting.update(cx, |value, _| *value = 9.0);
                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));

                cloned.reset(cx);
                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));

                cloned.jump_to(4.0, cx);
                cloned.update(cx, |value, _| *value = 8.0);
                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));

                cloned.scale_by(2.0, cx);
                self.samples.borrow_mut().push((
                    *transition.evaluate(window, cx),
                    transition.evaluate_delta(cx),
                ));
            }
            div().size_full()
        }
    }

    #[gpui::test]
    fn cloned_transition_mutations_invalidate_shared_same_instant_cache(cx: &mut TestAppContext) {
        let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _window: WindowHandle<TransitionCloneCacheTestView> =
            cx.open_window(size(px(100.), px(100.)), {
                let samples = samples.clone();
                move |_, _| TransitionCloneCacheTestView { samples }
            });
        cx.run_until_parked();

        assert_eq!(
            *samples.borrow(),
            vec![
                (0.0, 0.0),
                (5.0, 1.0),
                (0.0, 0.0),
                (0.0, 1.0),
                (4.0, 0.0),
                (8.0, 0.0),
            ]
        );
    }
}
