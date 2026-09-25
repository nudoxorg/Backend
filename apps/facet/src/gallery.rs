//! Gallery scenes: every component state, captured headless.
//!
//! A lane adds scenes by exposing `pub(crate) const SCENES: &[Scene]` from a
//! `gallery` submodule of its own module and listing it in [`all`]. A scene
//! builds a complete root view; the gallery binary captures it at a fixed
//! size, virtual time, appearance and text scale, optionally while playing an
//! input script (see [`backend_gui_harness::script`]). A scene declares its
//! default script from its build function with [`declare_script`].
//!
//! [`run`] drives the shared headless session: fonts installed, the facet
//! set, the pulse frozen (a scene may [`thaw`](crate::motion::pulse::thaw) it
//! while building), the motion epoch pinned to the scene's first frame, the
//! platform frame loop simulated on the virtual clock, and the script's acts
//! delivered at their virtual times. The same inputs give byte-identical
//! pixels.

pub mod align;
pub mod bench;
pub mod cli;
pub mod compose;
pub mod json;
pub mod lint;
pub mod matrix;
pub mod perf;
pub mod storm;
pub mod verify;

use crate::measure::Reveal;
use crate::motion::{self, pulse};
use crate::theme::{ActiveFacet, Contrast, Facet, set_facet};
use crate::tokens::{Appearance, ty};
use crate::{Density, Typeset, fonts, probe};
use backend_gui_harness::{
    Act, Drawn, Event, PlayedFrame, Script, Session, SessionOptions, Timeline, Viewport, play,
};
use gpui::{
    AnyView, App, AppContext, Context, FocusHandle, Global, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window, div,
};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// One capturable scene.
#[derive(Clone, Copy)]
pub struct Scene {
    /// Stable id used on the command line and in artifact names (`kebab-case`).
    pub id: &'static str,
    /// What the scene shows, and which board it reproduces.
    pub title: &'static str,
    /// Natural capture size in logical px.
    pub size: (u32, u32),
    /// Builds the root view.
    pub build: fn(&mut Window, &mut App) -> AnyView,
}

/// Every registered scene, in gallery order.
#[must_use]
pub fn all() -> Vec<Scene> {
    let groups: &[&[Scene]] = &[
        fonts::gallery::SCENES,
        motion::gallery::SCENES,
        motion::lab::SCENES,
        crate::paint::gallery::SCENES,
        crate::icons::gallery::SCENES,
        crate::data::gallery::SCENES,
        crate::overlay::gallery::SCENES,
        crate::overlay::gallery_overlays::SCENES,
        crate::controls::gallery::SCENES,
        crate::chrome::gallery::SCENES,
        bench::SCENES,
    ];
    groups
        .iter()
        .flat_map(|group| group.iter().copied())
        .collect()
}

/// The scene registered under `id` (gallery scenes, then the harness's
/// deliberately broken canaries).
#[must_use]
pub fn find(id: &str) -> Option<Scene> {
    all()
        .into_iter()
        .chain(bench::CANARIES.iter().copied())
        .find(|scene| scene.id == id)
}

/// A capture failure, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GalleryError(pub String);

impl fmt::Display for GalleryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GalleryError {}

impl GalleryError {
    fn from_display(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}

/// Everything that fixes a capture's pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    /// Logical size.
    pub size: (u32, u32),
    /// Device scale (1 or 2).
    pub scale: u8,
    /// Abyss or Glacier.
    pub appearance: Appearance,
    /// Text scale (1.0 = 100 %).
    pub text_scale: f32,
    /// Reduced motion.
    pub reduced_motion: bool,
    /// Comfortable, compact or dense.
    pub density: Density,
    /// Normal or high contrast.
    pub contrast: Contrast,
    /// Virtual times (ms after the scene's first frame) to capture.
    pub times: Vec<u64>,
    /// Record the probe ledger at every frame.
    pub probe: bool,
    /// The input script; `None` plays the scene's declared default (if any),
    /// `Some(Script::new())` plays nothing.
    pub script: Option<Script>,
    /// The simulated frame loop's period in virtual ms (0 = frames only at
    /// captures and inputs).
    pub frame_ms: u64,
    /// Keep the frame loop running until at least this time.
    pub until_ms: u64,
}

impl Shot {
    /// The scene's natural size at 2x, Abyss, 100 %, motion on, time 0, the
    /// scene's default script, a 16 ms frame loop.
    #[must_use]
    pub fn new(scene: &Scene) -> Self {
        Self {
            size: scene.size,
            scale: 2,
            appearance: Appearance::Abyss,
            text_scale: 1.0,
            reduced_motion: false,
            density: Density::Comfortable,
            contrast: Contrast::Normal,
            times: vec![0],
            probe: false,
            script: None,
            frame_ms: 16,
            until_ms: 0,
        }
    }

    /// The facet this shot renders under.
    #[must_use]
    pub fn facet(&self) -> Facet {
        Facet {
            appearance: self.appearance,
            text_scale: self.text_scale,
            reduced_motion: self.reduced_motion,
            density: self.density,
            contrast: self.contrast,
            ..Facet::default()
        }
    }
}

/// One captured frame.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Virtual time after the scene's first frame.
    pub time_ms: u64,
    /// The pixels (physical size).
    pub image: image::RgbaImage,
    /// What the probe saw while drawing this frame (empty unless enabled).
    pub ledger: probe::Ledger,
    /// What the frame carried (invalidations, draw time, viewport).
    pub drawn: Drawn,
}

/// One drawn frame of a [`run`], captured or not.
pub struct Tick<'a> {
    /// What the frame carried.
    pub drawn: Drawn,
    /// The pixels, at capture times.
    pub image: Option<&'a image::RgbaImage>,
    /// What the probe saw while drawing this frame.
    pub ledger: &'a probe::Ledger,
    /// The acts delivered at this instant, just before the frame.
    pub events: &'a [Event],
    /// Where the pointer is (None: outside the window).
    pub pointer: Option<(f32, f32)>,
    /// Whether a button is held.
    pub pressed: bool,
    /// The scene's description of this frame's work, if it declared one.
    pub note: Option<String>,
}

/// How a scene applies script acts that have no platform event (settings,
/// `route`), see [`declare_adapter`].
pub type Adapter = fn(&Act, &mut Window, &mut App);

#[derive(Default)]
struct DeclaredAdapter(Option<Adapter>);

impl Global for DeclaredAdapter {}

/// Declares how a scene applies settings and `route` acts (call from its
/// build function). A product scene routes them through its own settings
/// and navigation; without one, [`adapt`] sets the facet directly.
pub fn declare_adapter(adapter: Adapter, cx: &mut App) {
    cx.set_global(DeclaredAdapter(Some(adapter)));
}

/// Describes the work behind the frame just drawn (see [`declare_annotator`]).
pub type Annotator = fn(&mut App) -> String;

#[derive(Default)]
struct DeclaredAnnotator(Option<Annotator>);

impl Global for DeclaredAnnotator {}

/// Declares how to describe each drawn frame's work (which regions
/// re-rendered, what landed), so reports can attribute a slow frame (call
/// from the scene's build function).
pub fn declare_annotator(annotator: Annotator, cx: &mut App) {
    cx.set_global(DeclaredAnnotator(Some(annotator)));
}

/// A scene's "nothing in flight" predicate (see [`declare_quiet`]).
pub type QuietCheck = fn(&mut App) -> bool;

#[derive(Default)]
struct DeclaredQuiet(Option<QuietCheck>);

impl Global for DeclaredQuiet {}

/// Declares how to tell that a scene's asynchronous work (reads, engine
/// replies) has all landed (call from the scene's build function). The run
/// waits for it in real time after the first frame and after every input
/// instant, so I/O takes zero virtual time and frames stay reproducible.
pub fn declare_quiet(check: QuietCheck, cx: &mut App) {
    cx.set_global(DeclaredQuiet(Some(check)));
}

#[derive(Default)]
struct DeclaredScript(Option<&'static str>);

impl Global for DeclaredScript {}

/// Declares the input script a scene plays when the caller gives none (call
/// it from the scene's build function). Scripts use the syntax of
/// [`backend_gui_harness::script`].
pub fn declare_script(source: &'static str, cx: &mut App) {
    cx.set_global(DeclaredScript(Some(source)));
}

/// The script the scene being built declared, parsed.
///
/// # Errors
/// A declared script that does not parse (a scene bug).
pub fn declared_script(cx: &App) -> Result<Script, GalleryError> {
    cx.try_global::<DeclaredScript>()
        .and_then(|declared| declared.0)
        .map_or_else(
            || Ok(Script::new()),
            |source| Script::parse(source).map_err(GalleryError::from_display),
        )
}

/// Installs everything a gallery app needs: component globals, fonts, the
/// facet, reduced motion, a frozen pulse, the probe, and a fresh epoch.
///
/// # Errors
/// A font that fails verification or registration.
pub fn bootstrap(facet: Facet, record: bool, cx: &mut App) -> Result<(), GalleryError> {
    gpui_component::init(cx);
    fonts::install(cx).map_err(GalleryError::from_display)?;
    set_facet(facet, cx);
    cx.set_reduce_motion(facet.reduced_motion);
    pulse::freeze(0.0, cx);
    if record {
        probe::enable(cx);
    }
    motion::reset_epoch(cx);
    Ok(())
}

/// The root every scene is mounted in: the window ground and the default
/// body text style. Its focus handle (never focused itself, never a tab
/// stop) contains every focusable the scene paints, so "the focused element
/// is still painted" is `root.contains_focused(..)`.
pub struct SceneRoot {
    view: AnyView,
    focus: FocusHandle,
}

impl SceneRoot {
    /// Wraps a scene's view.
    #[must_use]
    pub fn new(view: AnyView, cx: &mut App) -> Self {
        let focus = cx.focus_handle();
        cx.set_global(RootFocus(focus.clone()));
        Self { view, focus }
    }
}

/// The mounted scene root's focus handle.
pub struct RootFocus(pub FocusHandle);

impl Global for RootFocus {}

impl Render for SceneRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        probe::draw_started(cx);
        probe::rendered(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(palette.g1)
            .typeset(ty::BODY, &facet)
            .text_color(palette.ink1.hsla())
            .child(self.view.clone())
    }
}

/// Mounts `scene` in a fresh [`SceneRoot`] inside a component `Root`.
pub fn mount(
    scene: &Scene,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Entity<gpui_component::Root> {
    let view = (scene.build)(window, cx);
    let root = cx.new(|cx| SceneRoot::new(view, cx));
    cx.new(|cx| gpui_component::Root::new(root, window, cx).bordered(false))
}

/// Applies a script's settings acts through the facet, the way the product's
/// settings would, and maps held ⌘/⌥ to the facet's [`Reveal`] (the gallery
/// root plays the shell's part).
pub fn adapt(act: &Act, window: &mut Window, cx: &mut App) {
    let mut facet = cx.facet();
    match act {
        Act::TextScale { percent } => facet.text_scale = f32::from(*percent) / 100.0,
        Act::Density { name } => {
            facet.density = match name.as_str() {
                "compact" => Density::Compact,
                "dense" => Density::Dense,
                _ => Density::Comfortable,
            };
        }
        Act::Theme { name } => {
            facet.appearance = if name == "glacier" {
                Appearance::Glacier
            } else {
                Appearance::Abyss
            };
        }
        Act::Contrast { name } => {
            facet.contrast = if name == "high" {
                Contrast::High
            } else {
                Contrast::Normal
            };
        }
        Act::Motion { on } => {
            facet.reduced_motion = !on;
            cx.set_reduce_motion(!on);
        }
        Act::Hold { .. } | Act::Release { .. } => {
            let held = window.modifiers();
            facet.reveal = Reveal {
                keys: held.platform,
                xray: held.alt,
            };
        }
        _ => return,
    }
    set_facet(facet, cx);
}

/// Plays `shot` on `scene` headless: opens the window, plays the script on
/// the simulated frame loop, and hands every drawn frame (pixels at capture
/// times) to `observe` inside the window. Returns the script that was played.
///
/// # Errors
/// An invalid viewport, a renderer or font failure, a bad declared script,
/// or the first error `observe` returns.
pub fn run(
    scene: &Scene,
    shot: &Shot,
    observe: &mut dyn FnMut(&Tick<'_>, &mut Window, &mut App) -> Result<(), GalleryError>,
) -> Result<Script, GalleryError> {
    fonts::verify().map_err(GalleryError::from_display)?;
    let viewport =
        Viewport::new(shot.size.0, shot.size.1, shot.scale).map_err(GalleryError::from_display)?;
    let failure = Rc::new(RefCell::new(None));
    let facet = shot.facet();
    let record = shot.probe;
    let scene_copy = *scene;
    let mut session = Session::open(
        viewport,
        // Icons, kind glyphs and the logo load through FACET's own assets.
        SessionOptions {
            asset_source: std::sync::Arc::new(crate::icons::Assets),
            frame_ms: shot.frame_ms,
        },
        {
            let failure = Rc::clone(&failure);
            move |window, cx| {
                if let Err(error) = bootstrap(facet, record, cx) {
                    *failure.borrow_mut() = Some(error);
                }
                mount(&scene_copy, window, cx)
            }
        },
    )
    .map_err(GalleryError::from_display)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    let script = match &shot.script {
        Some(script) => script.clone(),
        None => session
            .update(|_, cx| declared_script(cx))
            .map_err(GalleryError::from_display)??,
    };
    let quiet = session
        .update(|_, cx| cx.try_global::<DeclaredQuiet>().and_then(|quiet| quiet.0))
        .map_err(GalleryError::from_display)?;
    if let Some(check) = quiet {
        session.set_quiet(Some(backend_gui_harness::Quiet {
            check: Box::new(check),
            deadline: std::time::Duration::from_secs(120),
        }));
        session.quiesce().map_err(GalleryError::from_display)?;
    }
    // Records from the opening draw belong to no frame of the timeline.
    session
        .update(|_, cx| {
            let _ = probe::take(cx);
        })
        .map_err(GalleryError::from_display)?;
    let timeline = Timeline::new(&shot.times, shot.until_ms);
    let observer_error = RefCell::new(None);
    let adapter = session
        .update(|_, cx| cx.try_global::<DeclaredAdapter>().and_then(|declared| declared.0))
        .map_err(GalleryError::from_display)?
        .unwrap_or(adapt);
    let played = play(
        &mut session,
        &script,
        &timeline,
        &mut |act, window, cx| adapter(act, window, cx),
        &mut |frame: &PlayedFrame<'_>, window, cx| {
            let ledger = probe::take(cx);
            let note = cx
                .try_global::<DeclaredAnnotator>()
                .and_then(|declared| declared.0)
                .map(|annotate| annotate(cx));
            let tick = Tick {
                drawn: frame.drawn,
                image: frame.image,
                ledger: &ledger,
                events: frame.events,
                pointer: frame.pointer,
                pressed: frame.pressed.is_some(),
                note,
            };
            observe(&tick, window, cx).map_err(|error| {
                let message = error.0.clone();
                *observer_error.borrow_mut() = Some(error);
                backend_gui_harness::CaptureError::InvalidConfig(message)
            })
        },
    );
    if let Some(error) = observer_error.into_inner() {
        return Err(error);
    }
    played.map_err(GalleryError::from_display)?;
    Ok(script)
}

/// Plays `shot` and keeps every drawn frame's record and ledger (pixels only
/// at capture times, in the returned captures).
///
/// # Errors
/// See [`run`].
pub fn observe(
    scene: &Scene,
    shot: &Shot,
) -> Result<(Vec<align::Observed>, Vec<Frame>, Script), GalleryError> {
    let mut observed = Vec::new();
    let mut frames = Vec::new();
    let script = run(scene, shot, &mut |tick, _, _| {
        observed.push(align::Observed {
            drawn: tick.drawn,
            ledger: tick.ledger.clone(),
            events: tick.events.len(),
        });
        if let Some(image) = tick.image {
            frames.push(Frame {
                time_ms: tick.drawn.at_ms,
                image: image.clone(),
                ledger: tick.ledger.clone(),
                drawn: tick.drawn,
            });
        }
        Ok(())
    })?;
    Ok((observed, frames, script))
}

/// Captures `scene` headless at every time in `shot.times` (sorted, deduped),
/// playing the shot's script.
///
/// # Errors
/// An invalid viewport, a renderer failure, or a font failure.
pub fn capture(scene: &Scene, shot: &Shot) -> Result<Vec<Frame>, GalleryError> {
    let mut frames = Vec::with_capacity(shot.times.len());
    run(scene, shot, &mut |tick, _, _| {
        if let Some(image) = tick.image {
            frames.push(Frame {
                time_ms: tick.drawn.at_ms,
                image: image.clone(),
                ledger: tick.ledger.clone(),
                drawn: tick.drawn,
            });
        }
        Ok(())
    })?;
    Ok(frames)
}
