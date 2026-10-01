use scheduler::Instant;

use crate::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, Motion,
    MotionExtent, ParentElement, SpringAnimation, SpringConfig, SpringPlayback, SpringState,
    SpringTarget, Window,
};

pub use easing::*;
use smallvec::SmallVec;

/// An animation that can be applied to an element.
#[derive(Clone)]
pub struct Animation {
    /// The timing and playback policy used by this animation.
    pub motion: Motion,
}

impl Animation {
    /// Create a new animation with the given duration or motion.
    /// A duration creates a single linear pass by default.
    pub fn new(motion: impl Into<Motion>) -> Self {
        Self {
            motion: motion.into(),
        }
    }

    /// Set the animation to loop when it finishes.
    pub fn repeat(mut self) -> Self {
        self.motion = self.motion.repeat_forever();
        self
    }

    /// Set the easing function to use for this animation.
    /// The easing function will take a time delta between 0 and 1 and return a new delta
    /// between 0 and 1
    pub fn with_easing(mut self, easing: impl Fn(f32) -> f32 + 'static) -> Self {
        self.motion = self.motion.with_easing(easing);
        self
    }
}

/// An extension trait for adding the animation wrapper to both Elements and Components
///
/// Animations rendered through this trait automatically respect
/// [`App::reduce_motion`](crate::App::reduce_motion): when it is set,
/// the element is rendered at the motion's configured resting progress and no
/// animation frames are scheduled.
pub trait AnimationExt {
    /// Render this component or element with an animation
    fn with_animation(
        self,
        id: impl Into<ElementId>,
        animation: Animation,
        animator: impl Fn(Self, f32) -> Self + 'static,
    ) -> AnimationElement<Self>
    where
        Self: Sized,
    {
        AnimationElement {
            id: id.into(),
            element: Some(self),
            animator: Box::new(move |this, _, value| animator(this, value)),
            animations: smallvec::smallvec![animation],
        }
    }

    /// Render this component or element with a chain of animations
    fn with_animations(
        self,
        id: impl Into<ElementId>,
        animations: Vec<Animation>,
        animator: impl Fn(Self, usize, f32) -> Self + 'static,
    ) -> AnimationElement<Self>
    where
        Self: Sized,
    {
        debug_assert!(!animations.is_empty(), "animations must not be empty");
        AnimationElement {
            id: id.into(),
            element: Some(self),
            animator: Box::new(animator),
            animations: animations.into(),
        }
    }

    /// Render this element at the current position of a retargetable spring.
    /// The element ID keeps spring position and velocity between frames.
    fn with_spring<T>(
        self,
        id: impl Into<ElementId>,
        animation: SpringAnimation<T>,
        animator: impl FnOnce(Self, T::Output) -> Self + 'static,
    ) -> SpringAnimationElement<Self>
    where
        Self: Sized,
        T: SpringTarget,
        T::Output: 'static,
    {
        let SpringAnimation {
            motion,
            target,
            initial,
            playback,
        } = animation;
        let scalar_target = target.target();
        SpringAnimationElement {
            id: id.into(),
            element: Some(self),
            motion,
            target: scalar_target,
            initial,
            playback,
            animator: Some(Box::new(move |this, value| {
                animator(this, target.resolve(value))
            })),
        }
    }
}

impl<E: IntoElement + 'static> AnimationExt for E {}

/// A GPUI element that applies an animation to another element
pub struct AnimationElement<E> {
    id: ElementId,
    element: Option<E>,
    animations: SmallVec<[Animation; 1]>,
    animator: Box<dyn Fn(E, usize, f32) -> E + 'static>,
}

/// A GPUI element driven by a stateful spring.
pub struct SpringAnimationElement<E> {
    id: ElementId,
    element: Option<E>,
    motion: Motion<crate::SpringDescription>,
    target: f32,
    initial: Option<f32>,
    playback: SpringPlayback,
    animator: Option<Box<dyn FnOnce(E, f32) -> E + 'static>>,
}

impl<E: ParentElement> ParentElement for SpringAnimationElement<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        let Some(element) = &mut self.element else {
            return;
        };
        element.extend(elements);
    }
}

impl<E> SpringAnimationElement<E> {
    /// Applies a one-shot transformation to the wrapped element.
    pub fn map_element(mut self, f: impl FnOnce(E) -> E) -> Self {
        self.element = self.element.map(f);
        self
    }
}

impl<E: IntoElement + 'static> IntoElement for SpringAnimationElement<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: ParentElement> ParentElement for AnimationElement<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        let Some(element) = &mut self.element else {
            return;
        };

        element.extend(elements);
    }
}

impl<E> AnimationElement<E> {
    /// Returns a new [`AnimationElement<E>`] after applying the given function
    /// to the element being animated.
    pub fn map_element(mut self, f: impl FnOnce(E) -> E) -> AnimationElement<E> {
        self.element = self.element.map(f);
        self
    }
}

impl<E: IntoElement + 'static> IntoElement for AnimationElement<E> {
    type Element = AnimationElement<E>;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct AnimationState {
    start: Instant,
    animation_ix: usize,
}

struct SpringElementState {
    spring: SpringState,
    target: f32,
    config: SpringConfig,
    initial: f32,
    playback: SpringPlayback,
    updated_at: Instant,
}

impl<E: IntoElement + 'static> Element for SpringAnimationElement<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (crate::LayoutId, Self::RequestLayoutState) {
        window.with_element_state(global_id.unwrap(), |state, window| {
            let now = cx.background_executor().now();
            let initial = self.initial.unwrap_or(self.target);
            let mut state = state.unwrap_or_else(|| SpringElementState {
                spring: SpringState {
                    position: initial,
                    velocity: 0.0,
                },
                target: self.target,
                config: self.motion.config,
                initial,
                playback: self.playback,
                updated_at: now,
            });

            let elapsed = now
                .saturating_duration_since(state.updated_at)
                .as_secs_f32();
            if state.playback == SpringPlayback::Running {
                state.spring = state.config.step(state.spring, state.target, elapsed);
            }

            state.config = self.motion.config;
            state.target = self.target;

            let done = match self.playback {
                SpringPlayback::Running => {
                    if cx.reduce_motion() {
                        state.spring = SpringState {
                            position: state.target,
                            velocity: 0.0,
                        };
                        true
                    } else {
                        let done = state.config.is_settled(
                            state.spring,
                            state.target,
                            self.motion.epsilon,
                        );
                        if done {
                            state.spring = SpringState {
                                position: state.target,
                                velocity: 0.0,
                            };
                        }
                        done
                    }
                }
                SpringPlayback::Paused => true,
                SpringPlayback::Stopped => {
                    state.spring.velocity = 0.0;
                    true
                }
                SpringPlayback::Completed => {
                    state.spring = SpringState {
                        position: state.target,
                        velocity: 0.0,
                    };
                    true
                }
                SpringPlayback::Cancelled => {
                    state.spring = SpringState {
                        position: state.initial,
                        velocity: 0.0,
                    };
                    true
                }
            };
            state.playback = self.playback;
            state.updated_at = now;

            let element = self.element.take().expect("spring element rendered once");
            let animator = self.animator.take().expect("spring animator consumed once");
            let mut element = animator(element, state.spring.position).into_any_element();

            if !done {
                window.request_animation_frame();
            }

            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

impl<E: IntoElement + 'static> Element for AnimationElement<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (crate::LayoutId, Self::RequestLayoutState) {
        // NUDOX: read the scheduler clock (a `TestClock` under headless and
        // test contexts) instead of the wall clock, so `advance_clock` steps
        // animations and any frame at any virtual time is reproducible.
        let now = cx.background_executor().now();
        window.with_element_state(global_id.unwrap(), |state, window| {
            let mut state = state.unwrap_or_else(|| AnimationState {
                start: now,
                animation_ix: 0,
            });
            let (animation_ix, delta, done) = if cx.reduce_motion() {
                let animation_ix = self.animations.len() - 1;
                let delta = self.animations[animation_ix]
                    .motion
                    .resting_progress()
                    .get();
                (animation_ix, delta, true)
            } else {
                loop {
                    let animation_ix = state.animation_ix;
                    let motion = &self.animations[animation_ix].motion;
                    let sample = motion.sample(now.saturating_duration_since(state.start));

                    if sample.is_active || animation_ix == self.animations.len() - 1 {
                        break (animation_ix, sample.progress.get(), !sample.is_active);
                    }

                    // Carry unused frame time through every completed finite
                    // segment. An inactive infinite motion can only be a
                    // zero-span repeat, which consumes its initial delay.
                    let elapsed = now.saturating_duration_since(state.start);
                    let consumed = match motion.checked_extent() {
                        Some(MotionExtent::Finite(extent)) => extent,
                        Some(MotionExtent::Infinite) => motion.delay(),
                        None => elapsed,
                    };
                    state.start = state.start.checked_add(consumed).unwrap_or(now);
                    state.animation_ix += 1;
                }
            };
            debug_assert!(delta.is_finite(), "animation progress should be finite");

            let element = self.element.take().expect("should only be called once");
            let mut element = (self.animator)(element, animation_ix, delta).into_any_element();

            if !done {
                window.request_animation_frame();
            }

            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

mod easing {
    use std::f32::consts::PI;

    /// The linear easing function, or delta itself
    pub fn linear(delta: f32) -> f32 {
        delta
    }

    /// The quadratic easing function, delta * delta
    pub fn quadratic(delta: f32) -> f32 {
        delta * delta
    }

    /// The quadratic ease-in-out function, which starts and ends slowly but speeds up in the middle
    pub fn ease_in_out(delta: f32) -> f32 {
        if delta < 0.5 {
            2.0 * delta * delta
        } else {
            let x = -2.0 * delta + 2.0;
            1.0 - x * x / 2.0
        }
    }

    /// The Quint ease-out function, which starts quickly and decelerates to a stop
    pub fn ease_out_quint() -> impl Fn(f32) -> f32 {
        move |delta| 1.0 - (1.0 - delta).powi(5)
    }

    /// Apply the given easing function, first in the forward direction and then in the reverse direction
    pub fn bounce(easing: impl Fn(f32) -> f32) -> impl Fn(f32) -> f32 {
        move |delta| {
            if delta < 0.5 {
                easing(delta * 2.0)
            } else {
                easing((1.0 - delta) * 2.0)
            }
        }
    }

    /// A custom easing function for pulsating alpha that slows down as it approaches 0.1
    pub fn pulsating_between(min: f32, max: f32) -> impl Fn(f32) -> f32 {
        let range = max - min;

        move |delta| {
            // Use a combination of sine and cubic functions for a more natural breathing rhythm
            let t = (delta * 2.0 * PI).sin();
            let breath = (t * t * t + t) / 2.0;

            // Map the breath to our desired alpha range
            let normalized_alpha = (breath + 1.0) / 2.0;

            min + (normalized_alpha * range)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, time::Duration};

    use crate::{
        Animation, Context, InteractiveElement, Render, SpringAnimation, SpringConfig,
        TestAppContext, WindowHandle, div, prelude::*, px, size,
    };

    use super::*;

    struct AnimationTestView {
        rendered_deltas: Rc<RefCell<Vec<f32>>>,
    }

    impl Render for AnimationTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let rendered_deltas = self.rendered_deltas.clone();
            div().size_full().child(div().with_animation(
                "repeating-animation",
                Animation::new(Duration::from_secs(1)).repeat(),
                move |this, delta| {
                    rendered_deltas.borrow_mut().push(delta);
                    this
                },
            ))
        }
    }

    fn open_test_window(
        cx: &mut TestAppContext,
    ) -> (Rc<RefCell<Vec<f32>>>, WindowHandle<AnimationTestView>) {
        let rendered_deltas = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let rendered_deltas = rendered_deltas.clone();
            move |_, _| AnimationTestView { rendered_deltas }
        });
        cx.run_until_parked();
        (rendered_deltas, window)
    }

    struct TimedAnimationTestView {
        motion: Motion,
        rendered_deltas: Rc<RefCell<Vec<f32>>>,
    }

    impl Render for TimedAnimationTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let rendered_deltas = self.rendered_deltas.clone();
            div().size_full().child(div().with_animation(
                "finite-animation",
                Animation::new(self.motion.clone()),
                move |this, delta| {
                    rendered_deltas.borrow_mut().push(delta);
                    this
                },
            ))
        }
    }

    fn open_timed_test_window(
        cx: &mut TestAppContext,
        motion: Motion,
    ) -> (Rc<RefCell<Vec<f32>>>, WindowHandle<TimedAnimationTestView>) {
        let rendered_deltas = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let rendered_deltas = rendered_deltas.clone();
            move |_, _| TimedAnimationTestView {
                motion,
                rendered_deltas,
            }
        });
        cx.run_until_parked();
        (rendered_deltas, window)
    }

    fn simulate_next_frame<V: Render + 'static>(
        window: &WindowHandle<V>,
        cx: &mut TestAppContext,
    ) -> usize {
        let callback_count = window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        callback_count
    }
    // Before parent-animation-element, using .with_animation
    // would not allow chaining .parent after. This is just a
    // build check that we can call div().id().with_animation().child()
    #[test]
    fn test_animation_parent() {
        div()
            .id("id")
            //
            .with_animation(
                "animation",
                Animation::new(Duration::from_secs(1)),
                |el, _t| {
                    //
                    el
                },
            )
            .child(
                //
                div(),
            );
    }

    #[gpui::test]
    fn test_repeating_animation_schedules_animation_frames(cx: &mut TestAppContext) {
        let (rendered_deltas, window) = open_test_window(cx);

        assert_eq!(rendered_deltas.borrow().len(), 1);

        for expected_frames in 2..=3 {
            assert_eq!(simulate_next_frame(&window, cx), 1);
            assert_eq!(rendered_deltas.borrow().len(), expected_frames);
        }
    }

    #[gpui::test]
    fn test_reduce_motion_renders_single_static_frame(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (rendered_deltas, window) = open_test_window(cx);

        assert_eq!(*rendered_deltas.borrow(), vec![0.0]);

        assert_eq!(simulate_next_frame(&window, cx), 0);
        assert_eq!(*rendered_deltas.borrow(), vec![0.0]);
    }

    #[gpui::test]
    fn delayed_alternating_animation_finishes_without_idle_frames(cx: &mut TestAppContext) {
        let motion = Motion::new(Duration::from_millis(100))
            .with_delay(Duration::from_millis(50))
            .iterations(2)
            .alternate();
        let (rendered_deltas, window) = open_timed_test_window(cx, motion);
        assert_eq!(*rendered_deltas.borrow(), vec![0.0]);

        cx.executor().advance_clock(Duration::from_millis(25));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(rendered_deltas.borrow().last(), Some(&0.0));

        cx.executor().advance_clock(Duration::from_millis(125));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(rendered_deltas.borrow().last(), Some(&1.0));

        cx.executor().advance_clock(Duration::from_millis(100));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(rendered_deltas.borrow().last(), Some(&0.0));
        assert_eq!(simulate_next_frame(&window, cx), 0);
    }

    struct ChainedAnimationTestView {
        animations: Vec<Animation>,
        samples: Rc<RefCell<Vec<(usize, f32)>>>,
    }

    impl Render for ChainedAnimationTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let samples = self.samples.clone();
            div().size_full().child(div().with_animations(
                "animation-chain",
                self.animations.clone(),
                move |this, index, progress| {
                    samples.borrow_mut().push((index, progress));
                    this
                },
            ))
        }
    }

    fn open_chained_test_window(
        cx: &mut TestAppContext,
        animations: Vec<Animation>,
    ) -> (
        Rc<RefCell<Vec<(usize, f32)>>>,
        WindowHandle<ChainedAnimationTestView>,
    ) {
        let samples = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let samples = samples.clone();
            move |_, _| ChainedAnimationTestView {
                animations,
                samples,
            }
        });
        cx.run_until_parked();
        (samples, window)
    }

    #[gpui::test]
    fn animation_chain_carries_elapsed_time_across_finite_segments(cx: &mut TestAppContext) {
        let animations = vec![
            Animation::new(
                Motion::new(Duration::from_millis(100)).with_delay(Duration::from_millis(20)),
            ),
            Animation::new(
                Motion::new(Duration::from_millis(200)).with_delay(Duration::from_millis(10)),
            ),
            Animation::new(Duration::from_millis(300)),
        ];
        let (samples, window) = open_chained_test_window(cx, animations);

        cx.executor().advance_clock(Duration::from_millis(480));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(samples.borrow().last(), Some(&(2, 0.5)));

        cx.executor().advance_clock(Duration::from_millis(150));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(samples.borrow().last(), Some(&(2, 1.0)));
        assert_eq!(simulate_next_frame(&window, cx), 0);
    }

    #[gpui::test]
    fn inactive_infinite_zero_span_chain_segment_consumes_only_its_delay(cx: &mut TestAppContext) {
        let animations = vec![
            Animation::new(
                Motion::new(Duration::ZERO)
                    .repeat_forever()
                    .with_delay(Duration::from_millis(50)),
            ),
            Animation::new(Duration::from_millis(100)),
        ];
        let (samples, window) = open_chained_test_window(cx, animations);

        cx.executor().advance_clock(Duration::from_millis(100));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(samples.borrow().last(), Some(&(1, 0.5)));

        cx.executor().advance_clock(Duration::from_millis(50));
        assert_eq!(simulate_next_frame(&window, cx), 1);
        assert_eq!(samples.borrow().last(), Some(&(1, 1.0)));
        assert_eq!(simulate_next_frame(&window, cx), 0);
    }

    struct SpringAnimationTestView {
        target: Rc<std::cell::Cell<f32>>,
        rendered_values: Rc<RefCell<Vec<f32>>>,
    }

    impl Render for SpringAnimationTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let target = self.target.get();
            let rendered_values = self.rendered_values.clone();
            div().size_full().child(
                div().with_spring(
                    "retargetable-spring",
                    SpringAnimation::new(SpringConfig::new(100.0, 1.0, 1.0))
                        .to(target)
                        .from(0.0),
                    move |element, value| {
                        rendered_values.borrow_mut().push(value);
                        element
                    },
                ),
            )
        }
    }

    #[gpui::test]
    fn spring_element_retarget_preserves_position_and_velocity(cx: &mut TestAppContext) {
        let target = Rc::new(std::cell::Cell::new(1.0));
        let rendered_values = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let target = target.clone();
            let rendered_values = rendered_values.clone();
            move |_, _| SpringAnimationTestView {
                target,
                rendered_values,
            }
        });
        cx.run_until_parked();

        cx.executor().advance_clock(Duration::from_millis(50));
        window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        let before_retarget = *rendered_values.borrow().last().unwrap();
        assert!(before_retarget > 0.0);

        target.set(0.0);
        window.update(cx, |_, window, _| window.refresh()).unwrap();
        cx.run_until_parked();
        let at_retarget = *rendered_values.borrow().last().unwrap();
        assert_eq!(at_retarget, before_retarget);

        cx.executor().advance_clock(Duration::from_millis(10));
        window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        let after_retarget = *rendered_values.borrow().last().unwrap();
        assert!(
            after_retarget > at_retarget,
            "the prior positive velocity should continue briefly after the target reverses"
        );
    }

    #[gpui::test]
    fn spring_element_reduced_motion_settles_without_scheduling_frames(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let target = Rc::new(std::cell::Cell::new(1.0));
        let rendered_values = Rc::new(RefCell::new(Vec::new()));
        let window = cx.open_window(size(px(100.), px(100.)), {
            let target = target.clone();
            let rendered_values = rendered_values.clone();
            move |_, _| SpringAnimationTestView {
                target,
                rendered_values,
            }
        });
        cx.run_until_parked();

        assert_eq!(rendered_values.borrow().last(), Some(&1.0));
        assert_eq!(
            window
                .update(cx, |_, window, cx| window.simulate_next_frame(cx))
                .unwrap(),
            0
        );
        assert_eq!(rendered_values.borrow().last(), Some(&1.0));
    }
}
