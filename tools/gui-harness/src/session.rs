//! A stepwise headless session: one real GPUI window on the virtual clock,
//! driven act by act and frame by frame.
//!
//! [`Session`] is the primitive every verification tool is built on (scripted
//! captures, films, storms, the matrix, frame budgets). It owns the platform,
//! the window, the virtual clock and the input state (pointer, held button,
//! held modifiers), and it simulates the platform frame loop: [`Session::frame`]
//! delivers the next-frame callbacks and draws, the way a vsync does, and
//! reports how many invalidations the frame carried and how long `draw` took.
//!
//! [`play`] runs a [`Script`] against a session on a timeline of frame ticks
//! and capture times; everything is a pure function of the script, so the same
//! script gives byte-identical pixels.

use crate::script::{Act, Button, Event, Mods, Script};
use crate::{CaptureError, Viewport};
use gpui::{
    AnyWindowHandle, App, AssetSource, Capslock, Entity, FrameTimingCollector, HeadlessAppContext,
    InputEvent, Keystroke, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, PinchEvent, PlatformTextSystem, Pixels, Point,
    Render,
    ScrollDelta, ScrollWheelEvent, TouchPhase, Window, point, px, size,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How a session is set up.
#[derive(Clone)]
pub struct SessionOptions {
    /// Real assets for SVG/icon capture.
    pub asset_source: Arc<dyn AssetSource>,
    /// The simulated frame loop's period in virtual ms (16 ≈ 60 Hz). Zero
    /// turns the loop off: frames are drawn only at captures and right after
    /// each instant that delivered input.
    pub frame_ms: u64,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            asset_source: Arc::new(()),
            frame_ms: 16,
        }
    }
}

/// What one drawn frame carried.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    /// Virtual ms after the first frame.
    pub at_ms: u64,
    /// Invalidations (notifies, refreshes) coalesced into this frame. Zero
    /// means nothing asked for it: a real window would not have drawn.
    pub invalidations: u64,
    /// Next-frame callbacks delivered right before the draw. A callback that
    /// dirties nothing (bookkeeping such as `gpui_component`'s text-selection
    /// layer, which registers one on every paint) does not make a real
    /// window draw, so callbacks alone never count as a request.
    pub callbacks: usize,
    /// Wall time of `Window::draw` (render + layout + prepaint + paint).
    pub cpu: Duration,
    /// Wall time dispatching script acts since the previous draw, including
    /// their immediate foreground tasks. Background quiescence and draw are separate.
    pub input_cpu: Duration,
    /// Script acts dispatched since the previous draw (not physical key count).
    pub input_events: usize,
    /// The slowest single script-act dispatch in this batch.
    pub input_max: Duration,
    /// The logical viewport the frame was drawn at.
    pub viewport: Viewport,
    /// Whether the pixels were read back.
    pub captured: bool,
}

impl Drawn {
    /// Whether anything asked for this frame: a real window draws only when
    /// something invalidated it (a notify from a view, an animation-frame
    /// callback that notifies, a refresh, input that changed state).
    #[must_use]
    pub const fn requested(&self) -> bool {
        self.invalidations > 0
    }
}

/// One live headless window on the virtual clock.
pub struct Session {
    context: HeadlessAppContext,
    window: AnyWindowHandle,
    viewport: Viewport,
    frame_ms: u64,
    now_ms: u64,
    mods: Mods,
    pressed: Option<Button>,
    pointer: Option<Point<Pixels>>,
    timings: FrameTimingCollector,
    retain_frame_timings: bool,
    quiet: Option<Quiet>,
    input_cpu: Duration,
    input_events: usize,
    input_max: Duration,
}

/// A product's "nothing in flight" predicate (it may land finished work
/// while it checks) and how long to wait for it in real time.
pub struct Quiet {
    /// True once no I/O is in flight and every finished result has landed.
    pub check: Box<dyn FnMut(&mut App) -> bool>,
    /// Real-time deadline per wait.
    pub deadline: Duration,
}

fn gpui_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Gpui(error.to_string())
}

const fn gpui_button(button: Button) -> MouseButton {
    match button {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
    }
}

const fn gpui_modifiers(mods: Mods) -> Modifiers {
    Modifiers {
        control: mods.ctrl,
        alt: mods.alt,
        shift: mods.shift,
        platform: mods.cmd,
        function: false,
    }
}

impl Session {
    /// Opens a window of `viewport` and builds its root. The root's first
    /// frame is drawn at virtual time 0.
    ///
    /// # Errors
    /// No headless renderer on this platform, or GPUI failing to open the
    /// window.
    pub fn open<V, F>(
        viewport: Viewport,
        options: SessionOptions,
        build_root: F,
    ) -> Result<Self, CaptureError>
    where
        V: Render + 'static,
        F: FnOnce(&mut Window, &mut App) -> Entity<V>,
    {
        let platform = gpui_platform::current_platform(true);
        let text_system: Arc<dyn PlatformTextSystem> = platform.text_system();
        if gpui_platform::current_headless_renderer().is_none() {
            return Err(CaptureError::NoRenderer);
        }
        // Frame tracing counts invalidations per drawn frame, which is how a
        // session tells a requested frame from an idle one.
        gpui::set_frame_trace_enabled(true);
        let timings = FrameTimingCollector::new();
        let mut context = HeadlessAppContext::with_platform(
            text_system,
            options.asset_source,
            gpui_platform::current_headless_renderer,
        );
        let scale = f32::from(viewport.scale);
        let window = context
            .open_window(
                size(px(viewport.width as f32), px(viewport.height as f32)),
                move |window, cx| {
                    window.set_scale_factor(scale);
                    build_root(window, cx)
                },
            )
            .map_err(gpui_error)?;
        let mut session = Self {
            context,
            window: window.into(),
            viewport,
            frame_ms: options.frame_ms,
            now_ms: 0,
            mods: Mods::default(),
            pressed: None,
            pointer: None,
            timings,
            retain_frame_timings: true,
            quiet: None,
            input_cpu: Duration::ZERO,
            input_events: 0,
            input_max: Duration::ZERO,
        };
        session.settle_tasks();
        // Draws made while opening belong to no frame of the timeline.
        let _ = session.timings.collect_unseen();
        Ok(session)
    }

    /// Whether to retain GPUI's bounded global trace history after consuming
    /// this session's invalidations. Memory experiments disable retention so
    /// trace-buffer growth cannot be mistaken for application retention.
    /// Timing and invalidation samples remain available for every draw.
    pub fn set_frame_timing_retention(&mut self, retain: bool) {
        self.retain_frame_timings = retain;
        if !retain {
            self.clear_frame_timing_history();
        }
    }

    fn clear_frame_timing_history(&mut self) {
        // The existing off transition clears and releases the global ring.
        // Reset the collector's cursor when total_pushed is reset to zero.
        // This policy belongs to a memory-only session; ordinary perf runs
        // retain their trace history and do not pay this allocation overhead.
        gpui::set_frame_trace_enabled(false);
        gpui::set_frame_trace_enabled(true);
        self.timings = FrameTimingCollector::new();
    }

    /// The simulated frame period (0 = no frame loop).
    #[must_use]
    pub const fn frame_ms(&self) -> u64 {
        self.frame_ms
    }

    /// Virtual ms since the session opened.
    #[must_use]
    pub const fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// The current logical viewport.
    #[must_use]
    pub const fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// The held modifiers.
    #[must_use]
    pub const fn mods(&self) -> Mods {
        self.mods
    }

    /// The held button, if any.
    #[must_use]
    pub const fn pressed(&self) -> Option<Button> {
        self.pressed
    }

    /// Where the pointer is, if it is inside the window.
    #[must_use]
    pub fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer
            .map(|position| (f32::from(position.x), f32::from(position.y)))
    }

    fn settle_tasks(&mut self) {
        self.context.run_until_parked();
    }

    /// Installs a quiescence predicate: after the first frame and after
    /// every instant that delivered input, the session waits in real time
    /// (the virtual clock does not move) until it holds, so asynchronous
    /// I/O lands in zero virtual time and frames stay reproducible.
    pub fn set_quiet(&mut self, quiet: Option<Quiet>) {
        self.quiet = quiet;
    }

    /// Waits (real time) for the quiescence predicate, if one is installed.
    ///
    /// # Errors
    /// The predicate did not hold within its deadline.
    pub fn quiesce(&mut self) -> Result<(), CaptureError> {
        let Some(mut quiet) = self.quiet.take() else {
            return Ok(());
        };
        let started = Instant::now();
        let result = loop {
            self.settle_tasks();
            let holds = self
                .context
                .update(|cx| (quiet.check)(cx));
            if holds {
                self.settle_tasks();
                break Ok(());
            }
            if started.elapsed() > quiet.deadline {
                break Err(CaptureError::Gpui(format!(
                    "the product never went quiet within {:?} at virtual {} ms",
                    quiet.deadline, self.now_ms
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        self.quiet = Some(quiet);
        result
    }

    /// Runs `f` inside the window.
    ///
    /// # Errors
    /// The window is gone.
    pub fn update<R>(
        &mut self,
        f: impl FnOnce(&mut Window, &mut App) -> R,
    ) -> Result<R, CaptureError> {
        self.context
            .update_window(self.window, |_, window, cx| f(window, cx))
            .map_err(gpui_error)
    }

    /// Moves the virtual clock forward to `at_ms` (never back), running every
    /// timer and task that comes due.
    pub fn advance_to(&mut self, at_ms: u64) {
        if at_ms > self.now_ms {
            self.context
                .advance_clock(Duration::from_millis(at_ms - self.now_ms));
            self.now_ms = at_ms;
        }
        self.settle_tasks();
    }

    fn dispatch(&mut self, event: impl InputEvent) -> Result<(), CaptureError> {
        let input = event.to_platform_input();
        self.update(|window, cx| {
            window.dispatch_event(input, cx);
        })?;
        self.settle_tasks();
        Ok(())
    }

    fn move_to(&mut self, x: f32, y: f32) -> Result<(), CaptureError> {
        let position = point(px(x), px(y));
        self.pointer = Some(position);
        self.dispatch(MouseMoveEvent {
            position,
            pressed_button: self.pressed.map(gpui_button),
            modifiers: gpui_modifiers(self.mods),
        })
    }

    fn button(&mut self, x: f32, y: f32, button: Button, down: bool) -> Result<(), CaptureError> {
        let position = point(px(x), px(y));
        if self.pointer != Some(position) {
            // A real pointer is where it clicks before it clicks.
            self.move_to(x, y)?;
        }
        let modifiers = gpui_modifiers(self.mods);
        if down {
            self.pressed = Some(button);
            self.dispatch(MouseDownEvent {
                button: gpui_button(button),
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            })
        } else {
            self.pressed = None;
            self.dispatch(MouseUpEvent {
                button: gpui_button(button),
                position,
                modifiers,
                click_count: 1,
            })
        }
    }

    fn set_mods(&mut self, mods: Mods) -> Result<(), CaptureError> {
        if mods == self.mods {
            return Ok(());
        }
        self.mods = mods;
        let modifiers = gpui_modifiers(mods);
        self.update(|window, _| window.set_modifiers(modifiers))?;
        self.dispatch(ModifiersChangedEvent {
            modifiers,
            capslock: Capslock { on: false },
        })
    }

    fn key(&mut self, chord: &str, held: Mods) -> Result<(), CaptureError> {
        let mut keystroke = Keystroke::parse(chord)
            .map_err(|error| CaptureError::InvalidConfig(format!("key `{chord}`: {error}")))?;
        let merged = gpui_modifiers(held);
        keystroke.modifiers.control |= merged.control;
        keystroke.modifiers.alt |= merged.alt;
        keystroke.modifiers.shift |= merged.shift;
        keystroke.modifiers.platform |= merged.platform;
        self.update(|window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        self.settle_tasks();
        Ok(())
    }

    /// Resizes the window to `width` x `height` logical px, re-flowing the
    /// layout at the new size (and keeping the pointer where it was).
    ///
    /// # Errors
    /// A zero or overflowing size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), CaptureError> {
        let next = Viewport::new(width, height, self.viewport.scale)?;
        let scale = f32::from(next.scale);
        self.update(|window, cx| {
            window.resize(size(px(width as f32), px(height as f32)));
            // The test platform stores the size without calling back; the
            // window re-reads its bounds (and the platform's fixed 2x scale,
            // which the capture's own scale then replaces).
            window.bounds_changed(cx);
            window.set_scale_factor(scale);
        })?;
        self.viewport = next;
        self.settle_tasks();
        if let Some(position) = self.pointer {
            // `bounds_changed` re-reads the platform pointer (the origin);
            // put the real one back without inventing a new position.
            self.dispatch(MouseMoveEvent {
                position,
                pressed_button: self.pressed.map(gpui_button),
                modifiers: gpui_modifiers(self.mods),
            })?;
        }
        Ok(())
    }

    /// Delivers one act. Platform acts (pointer, keys, modifiers, resize) are
    /// dispatched as platform events first; then `adapter` sees every act
    /// inside the window. Settings (`text-scale`, `density`, `theme`,
    /// `contrast`, `motion`) have no platform event: the adapter applies them
    /// through the product's own path.
    ///
    /// # Errors
    /// A malformed chord, an invalid size, or the window being gone.
    pub fn apply(
        &mut self,
        act: &Act,
        adapter: &mut dyn FnMut(&Act, &mut Window, &mut App),
    ) -> Result<(), CaptureError> {
        let started = Instant::now();
        let result = (|| {
            self.dispatch_act(act)?;
            self.update(|window, cx| adapter(act, window, cx))?;
            self.settle_tasks();
            Ok(())
        })();
        let elapsed = started.elapsed();
        self.input_cpu += elapsed;
        self.input_events += 1;
        self.input_max = self.input_max.max(elapsed);
        result
    }

    fn dispatch_act(&mut self, act: &Act) -> Result<(), CaptureError> {
        match act {
            Act::Move { x, y } => self.move_to(*x, *y),
            Act::Down { x, y, button } => self.button(*x, *y, *button, true),
            Act::Up { x, y, button } => self.button(*x, *y, *button, false),
            Act::Click { x, y, button } => {
                self.button(*x, *y, *button, true)?;
                self.button(*x, *y, *button, false)
            }
            Act::Scroll { x, y, dx, dy } => {
                if self.pointer != Some(point(px(*x), px(*y))) {
                    self.move_to(*x, *y)?;
                }
                self.dispatch(ScrollWheelEvent {
                    position: point(px(*x), px(*y)),
                    delta: ScrollDelta::Pixels(point(px(*dx), px(*dy))),
                    modifiers: gpui_modifiers(self.mods),
                    touch_phase: TouchPhase::Moved,
                })
            }
            Act::Zoom { x, y, factor } => {
                if self.pointer != Some(point(px(*x), px(*y))) {
                    self.move_to(*x, *y)?;
                }
                let position = point(px(*x), px(*y));
                let modifiers = gpui_modifiers(self.mods);
                for (phase, delta) in [
                    (TouchPhase::Started, 0.0),
                    (TouchPhase::Moved, factor - 1.0),
                    (TouchPhase::Ended, 0.0),
                ] {
                    self.dispatch(PinchEvent {
                        position,
                        delta,
                        modifiers,
                        phase,
                    })?;
                }
                Ok(())
            }
            Act::Drag { .. } => Err(CaptureError::InvalidConfig(
                "a drag reached the session unexpanded; play scripts through `play`".to_owned(),
            )),
            Act::Route { .. } => Ok(()),
            Act::Leave => {
                let position = self.pointer.unwrap_or_default();
                self.pointer = None;
                self.dispatch(MouseExitEvent {
                    position,
                    pressed_button: self.pressed.map(gpui_button),
                    modifiers: gpui_modifiers(self.mods),
                })
            }
            Act::Key { chord } => self.key(chord, self.mods),
            Act::Type { text } => {
                for character in text.chars() {
                    let text = character.to_string();
                    let keystroke = Keystroke {
                        modifiers: Modifiers::none(),
                        key: text.clone(),
                        key_char: Some(text),
                    };
                    self.update(|window, cx| {
                        window.dispatch_keystroke(keystroke, cx);
                    })?;
                    self.settle_tasks();
                }
                Ok(())
            }
            Act::Hold { mods } => self.set_mods(self.mods.with(*mods)),
            Act::Release { mods } => self.set_mods(self.mods.without(*mods)),
            Act::Resize { width, height } => self.resize(*width, *height),
            Act::TextScale { .. }
            | Act::Density { .. }
            | Act::Theme { .. }
            | Act::Contrast { .. }
            | Act::Motion { .. } => Ok(()),
        }
    }

    /// Simulates one platform frame: delivers the next-frame callbacks, draws,
    /// and (when `capture`) reads the pixels back at the viewport's size.
    ///
    /// # Errors
    /// The renderer failing to read back.
    pub fn frame(
        &mut self,
        capture: bool,
    ) -> Result<(Drawn, Option<image::RgbaImage>), CaptureError> {
        let input_cpu = std::mem::take(&mut self.input_cpu);
        let input_events = std::mem::take(&mut self.input_events);
        let input_max = std::mem::take(&mut self.input_max);
        let (callbacks, cpu) = self.update(|window, cx| {
            let callbacks = window.simulate_next_frame(cx);
            let started = Instant::now();
            let clear = window.draw(cx);
            let cpu = started.elapsed();
            clear.clear(cx);
            (callbacks, cpu)
        })?;
        let window_id = self.window.window_id();
        let invalidations = self
            .timings
            .collect_unseen()
            .into_iter()
            .filter(|timing| timing.window_id == window_id)
            .map(|timing| timing.invalidations)
            .sum();
        if !self.retain_frame_timings {
            self.clear_frame_timing_history();
        }
        let image = if capture {
            let image = self
                .update(|window, _| window.render_to_image())?
                .map_err(gpui_error)?;
            Some(crate::gpui_driver::normalize_capture_image(
                image,
                self.viewport,
            )?)
        } else {
            None
        };
        self.settle_tasks();
        Ok((
            Drawn {
                at_ms: self.now_ms,
                invalidations,
                callbacks,
                cpu,
                input_cpu,
                input_events,
                input_max,
                viewport: self.viewport,
                captured: capture,
            },
            image,
        ))
    }
}

/// One frame of a played timeline, handed to the observer.
pub struct PlayedFrame<'a> {
    /// What the frame carried.
    pub drawn: Drawn,
    /// The pixels, for capture times.
    pub image: Option<&'a image::RgbaImage>,
    /// The acts delivered since the previous drawn frame (in order).
    pub events: &'a [Event],
    /// Where the pointer is (None: outside the window).
    pub pointer: Option<(f32, f32)>,
    /// The held button.
    pub pressed: Option<Button>,
    /// The held modifiers.
    pub mods: Mods,
}

/// When a timeline is sampled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    /// Virtual times whose pixels are read back (ascending, deduplicated).
    pub captures: Vec<u64>,
    /// Keep ticking until at least this time.
    pub until_ms: u64,
}

impl Timeline {
    /// The last time the timeline reaches.
    #[must_use]
    pub fn end_ms(&self) -> u64 {
        self.until_ms
    }
}

impl Timeline {
    /// Captures at `times`, running until the last capture or `until_ms`.
    #[must_use]
    pub fn new(times: &[u64], until_ms: u64) -> Self {
        let mut captures = times.to_vec();
        captures.sort_unstable();
        captures.dedup();
        let until_ms = until_ms.max(captures.last().copied().unwrap_or(0));
        Self { captures, until_ms }
    }
}

/// Plays `script` on `session` from its current time: every frame tick and
/// capture time in order, events first within an instant, then the frame.
/// `observe` sees every drawn frame (with pixels at capture times) inside the
/// window, so it can drain probes that belong to exactly that frame.
///
/// # Errors
/// The first act or frame that fails, or an error returned by `observe`.
pub fn play(
    session: &mut Session,
    script: &Script,
    timeline: &Timeline,
    adapter: &mut dyn FnMut(&Act, &mut Window, &mut App),
    observe: &mut dyn FnMut(&PlayedFrame<'_>, &mut Window, &mut App) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    let script = &script.expanded(session.frame_ms());
    let start = session.now_ms();
    let until = timeline.until_ms.max(script.end_ms()).max(start);
    let mut moments = timeline.captures.clone();
    moments.extend(script.times());
    let frame_ms = session.frame_ms();
    if frame_ms > 0 {
        let first = start.div_ceil(frame_ms) * frame_ms;
        moments.extend((first..=until).step_by(usize::try_from(frame_ms).unwrap_or(16)));
    }
    moments.retain(|time| *time >= start);
    moments.sort_unstable();
    moments.dedup();
    let mut next_event = script.events.partition_point(|event| event.at_ms < start);
    // Acts delivered since the last drawn frame: a frame reports all of them.
    let mut undrawn = next_event;
    for at in moments {
        session.advance_to(at);
        let first = next_event;
        while let Some(event) = script.events.get(next_event)
            && event.at_ms == at
        {
            session.apply(&event.act, adapter)?;
            next_event += 1;
        }
        if next_event > first {
            session.quiesce()?;
        }
        let capture = timeline.captures.binary_search(&at).is_ok();
        let tick = frame_ms > 0 && at % frame_ms == 0;
        let immediate = frame_ms == 0 && next_event > first;
        if capture || tick || immediate {
            let events = &script.events[undrawn..next_event];
            undrawn = next_event;
            let (drawn, image) = session.frame(capture)?;
            let played = PlayedFrame {
                drawn,
                image: image.as_ref(),
                events,
                pointer: session.pointer(),
                pressed: session.pressed(),
                mods: session.mods(),
            };
            session.update(|window, cx| observe(&played, window, cx))??;
        }
    }
    Ok(())
}
