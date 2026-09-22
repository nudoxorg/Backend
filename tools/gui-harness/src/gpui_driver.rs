//! Direct offscreen GPUI CE capture and deterministic input driving.

use crate::{
    AnimationFrame, CaptureError, CaptureRecord, CaptureSet, GuiState, InputError, InputStep,
    Viewport, preflight_viewport,
};
use gpui::{
    AnyWindowHandle, App, AssetSource, Capslock, ClipboardItem, Entity, HeadlessAppContext,
    InputEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    NavigationDirection, PlatformTextSystem, Render, ScrollDelta, ScrollWheelEvent, TouchPhase,
    Window, px, size,
};
use std::sync::Arc;

/// Options that control how the direct GPUI driver is initialised.
#[derive(Clone)]
pub struct GpuiCaptureOptions {
    /// Optional real asset source for SVG/icon capture.
    pub asset_source: Arc<dyn AssetSource>,
    /// Whether to fail if the platform has no direct headless renderer.
    pub require_renderer: bool,
}

impl Default for GpuiCaptureOptions {
    fn default() -> Self {
        Self {
            asset_source: Arc::new(()),
            require_renderer: true,
        }
    }
}

/// Captures a real GPUI `Render` root through the platform's offscreen
/// renderer.  The root builder is the desktop adapter boundary: it must build
/// the actual workspace around an admitted projection, never a placeholder.
pub fn capture_gpui_state<V, F>(
    viewport: Viewport,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
    options: GpuiCaptureOptions,
    build_root: F,
) -> Result<CaptureSet, CaptureError>
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
{
    capture_gpui_state_with_adapters(
        viewport,
        state,
        actions,
        frames,
        options,
        |_frame, _window, _cx| {},
        |_step, _window, _cx| {},
        build_root,
    )
}

/// Variant of [`capture_gpui_state`] that lets a desktop adapter perform a
/// real animation retarget/reversal at named timeline phases.  This callback
/// runs inside GPUI's window update, so it can update the actual workspace
/// entity and request a frame; the resulting pixels are what get captured.
pub fn capture_gpui_state_with_hooks<V, F, H>(
    viewport: Viewport,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
    options: GpuiCaptureOptions,
    frame_hook: H,
    build_root: F,
) -> Result<CaptureSet, CaptureError>
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
    H: FnMut(&AnimationFrame, &mut Window, &mut App),
{
    capture_gpui_state_with_adapters(
        viewport,
        state,
        actions,
        frames,
        options,
        frame_hook,
        |_step, _window, _cx| {},
        build_root,
    )
}

/// Capture variant with an adapter callback for product-owned environment changes.
/// Theme and locale are deliberately not global mutable harness state: an adapter
/// must apply them to the actual product workspace, so a capture can prove that
/// the rendered pixels follow the product's own settings and localization path.
pub fn capture_gpui_state_with_adapters<V, F, H, I>(
    viewport: Viewport,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
    options: GpuiCaptureOptions,
    mut frame_hook: H,
    input_hook: I,
    build_root: F,
) -> Result<CaptureSet, CaptureError>
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
    H: FnMut(&AnimationFrame, &mut Window, &mut App),
    I: FnMut(&InputStep, &mut Window, &mut App),
{
    capture_gpui_state_with_adapters_result(
        viewport,
        state,
        actions,
        frames,
        options,
        move |frame, window, cx| {
            frame_hook(frame, window, cx);
            Ok(())
        },
        input_hook,
        move |window, cx| build_root(window, cx),
    )
}

/// Result-returning adapter boundary for product roots that must apply a
/// scenario before the first frame and can fail when live data cannot satisfy
/// the requested state.
pub fn capture_gpui_state_with_adapters_result<V, F, H, I>(
    viewport: Viewport,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
    options: GpuiCaptureOptions,
    mut frame_hook: H,
    mut input_hook: I,
    build_root: F,
) -> Result<CaptureSet, CaptureError>
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
    H: FnMut(&AnimationFrame, &mut Window, &mut App) -> Result<(), CaptureError>,
    I: FnMut(&InputStep, &mut Window, &mut App),
{
    let viewport = preflight_viewport(viewport, actions, frames)?;
    let platform = gpui_platform::current_platform(true);
    let text_system: Arc<dyn PlatformTextSystem> = platform.text_system();
    if options.require_renderer && gpui_platform::current_headless_renderer().is_none() {
        return Err(CaptureError::NoRenderer);
    }

    let mut context = HeadlessAppContext::with_platform(
        text_system,
        options.asset_source,
        gpui_platform::current_headless_renderer,
    );
    let window = context
        .open_window(
            size(px(viewport.width as f32), px(viewport.height as f32)),
            move |window, cx| {
                // TestPlatform starts at 1x; set_scale_factor is the deterministic
                // seam needed for 2x captures and causes device-pixel layout to be
                // exercised by the same scene.
                window.set_scale_factor(f32::from(viewport.scale));
                build_root(window, cx)
            },
        )
        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
    let window: AnyWindowHandle = window.into();
    // Render once before dispatching t=0 input. Production key contexts are
    // installed by the root's first render; without this priming pass a
    // journey's initial shortcut can arrive before GPUI knows which actions
    // the workspace owns.
    context
        .update_window(window, |_, _, _| {})
        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
    context.run_until_parked();

    // Wait steps advance the virtual schedule; other steps are dispatched at
    // their scheduled offset, between animation frames.  This keeps an input
    // sequence from accidentally turning every animation phase into the same
    // post-action screenshot.
    let mut scheduled = Vec::new();
    let mut schedule_time = 0_u64;
    for (index, step) in actions.iter().enumerate() {
        if matches!(step, InputStep::Wait { .. }) {
            schedule_time = schedule_time.saturating_add(step.duration().as_millis() as u64);
        } else {
            scheduled.push((schedule_time, index, step));
        }
    }
    let mut next_action = 0_usize;
    let mut driver_state = DriverState::default();
    let mut current_viewport = viewport;
    let mut input_index = None;
    let mut records = Vec::with_capacity(frames.len());
    let mut elapsed = 0_u64;
    for frame in frames {
        let mut cursor = elapsed;
        while let Some((at, index, step)) = scheduled.get(next_action) {
            if *at > frame.time_ms {
                break;
            }
            // Advance to every event's timestamp before dispatching it. This
            // matters when several retarget/reversal inputs land between two
            // screenshots: GPUI sees the same virtual clock progression that
            // a user would, instead of a burst of post-frame events.
            let delta = at.saturating_sub(cursor);
            if delta != 0 {
                context.advance_clock(std::time::Duration::from_millis(delta));
                context.run_until_parked();
            }
            apply_step(
                &mut context,
                window,
                step,
                &mut driver_state,
                &mut current_viewport,
                &mut input_hook,
            )?;
            // GPUI dispatch can enqueue action/context work behind the
            // platform event. Drain that work before the frame hook observes
            // semantics, otherwise a key at t=0 is captured one frame late.
            context.run_until_parked();
            input_index = Some(*index);
            cursor = *at;
            next_action += 1;
        }
        let delta = frame.time_ms.saturating_sub(cursor);
        if delta != 0 {
            context.advance_clock(std::time::Duration::from_millis(delta));
            context.run_until_parked();
        }
        context
            .update_window(window, |_, window, cx| frame_hook(frame, window, cx))
            .map_err(|error| CaptureError::Gpui(error.to_string()))??;
        let image = normalize_capture_image(
            draw_and_capture(&mut context, window)?,
            current_viewport,
        )?;
        records.push(CaptureRecord {
            label: frame.label.clone(),
            time_ms: frame.time_ms,
            viewport: current_viewport,
            image,
            input_index,
            diff: None,
        });
        elapsed = frame.time_ms;
    }
    Ok(CaptureSet {
        state,
        viewport,
        frames: records,
    })
}

fn apply_step(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    step: &InputStep,
    driver_state: &mut DriverState,
    viewport: &mut Viewport,
    input_hook: &mut impl FnMut(&InputStep, &mut Window, &mut App),
) -> Result<(), CaptureError> {
    match step {
        InputStep::Key { value } => {
            let keystroke = Keystroke::parse(value)
                .map_err(|error| CaptureError::Input(InputError::Keystroke(error.to_string())))?;
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Text { value } => {
            dispatch_text(context, window, value)?;
        }
        InputStep::FocusNext => {
            context
                .update_window(window, |_, window, cx| window.focus_next(cx))
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::FocusPrevious => {
            context
                .update_window(window, |_, window, cx| window.focus_prev(cx))
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Wait { milliseconds } => {
            context.advance_clock(std::time::Duration::from_millis(*milliseconds));
        }
        InputStep::PointerMove {
            x,
            y,
            pressed_button,
        } => {
            let position = crate::position(*x, *y);
            let pressed_button = pressed_button.as_deref().map(mouse_button).transpose()?;
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_event(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position,
                            pressed_button,
                            modifiers: driver_state.modifiers,
                        }),
                        cx,
                    );
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::PointerDown { x, y, button } => {
            dispatch_mouse(context, window, *x, *y, button, true, driver_state)?;
        }
        InputStep::PointerUp { x, y, button } => {
            dispatch_mouse(context, window, *x, *y, button, false, driver_state)?;
        }
        InputStep::Click { x, y, button } => {
            dispatch_mouse(context, window, *x, *y, button, true, driver_state)?;
            dispatch_mouse(context, window, *x, *y, button, false, driver_state)?;
        }
        InputStep::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
            button,
        } => {
            dispatch_mouse(
                context,
                window,
                *from_x,
                *from_y,
                button,
                true,
                driver_state,
            )?;
            dispatch_mouse_move(context, window, *to_x, *to_y, driver_state)?;
            dispatch_mouse(context, window, *to_x, *to_y, button, false, driver_state)?;
        }
        InputStep::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            let event = ScrollWheelEvent {
                position: crate::position(*x, *y),
                delta: ScrollDelta::Pixels(crate::position(*delta_x, *delta_y)),
                modifiers: driver_state.modifiers,
                touch_phase: TouchPhase::Moved,
            };
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_event(event.to_platform_input(), cx);
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Pinch { x, y, delta } => {
            let event = gpui::PinchEvent {
                position: crate::position(*x, *y),
                delta: *delta,
                modifiers: driver_state.modifiers,
                phase: TouchPhase::Moved,
            };
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_event(event.to_platform_input(), cx);
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Clipboard { value } => {
            context
                .update_window(window, |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Paste => {
            let keystroke = Keystroke::parse("cmd-v")
                .map_err(|error| CaptureError::Input(InputError::Keystroke(error.to_string())))?;
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::ImeText { .. }
        | InputStep::ImeCompose { .. }
        | InputStep::ImeCommit { .. }
        | InputStep::ImeCancel => {}
        InputStep::Modifiers {
            shift,
            control,
            alt,
            command,
        } => {
            driver_state.modifiers = crate::modifiers(*shift, *control, *alt, *command);
            let event = gpui::ModifiersChangedEvent {
                modifiers: driver_state.modifiers,
                capslock: Capslock { on: false },
            };
            context
                .update_window(window, |_, window, cx| {
                    window.dispatch_event(event.to_platform_input(), cx);
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Resize { width, height } => {
            if *width == 0 || *height == 0 {
                return Err(CaptureError::InvalidConfig(
                    "resize dimensions must be nonzero".to_owned(),
                ));
            }
            let next_viewport = Viewport::new(*width, *height, viewport.scale)?;
            context
                .update_window(window, |_, window, _| {
                    window.resize(size(px(*width as f32), px(*height as f32)))
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            *viewport = next_viewport;
        }
        InputStep::Scale { factor } => {
            if !matches!(factor, 1 | 2) {
                return Err(CaptureError::InvalidConfig(format!(
                    "unsupported GPUI scale factor {factor}; expected 1 or 2"
                )));
            }
            let next_viewport = Viewport::new(viewport.width, viewport.height, *factor)?;
            context
                .update_window(window, |_, window, _| {
                    window.set_scale_factor(f32::from(*factor))
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            *viewport = next_viewport;
        }
        InputStep::Theme { .. } | InputStep::Locale { .. } => {}
    }
    context
        .update_window(window, |_, window, cx| input_hook(step, window, cx))
        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
    context.run_until_parked();
    // A real desktop presents a new frame after a state changing action
    // before the next physical keystroke arrives. Headless journeys used to
    // dispatch a whole same-timestamp burst against the previous dispatch
    // tree, so a newly opened CE input could not receive the very next text
    // event. Paint the product tree between actions as the platform does; this
    // also makes focus transitions and semantic action snapshots observable
    // instead of relying on a later screenshot to repair them.
    context
        .update_window(window, |_, window, cx| {
            window.simulate_next_frame(cx);
            let clear = window.draw(cx);
            clear.clear(cx);
        })
        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
    // Deferred focus work is scheduled against the next platform tick. Give
    // that queue one deterministic tick before the next input event so a
    // remounted CE field can actually own keyboard text.
    context.advance_clock(std::time::Duration::from_millis(1));
    context.run_until_parked();
    Ok(())
}

fn dispatch_text(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    value: &str,
) -> Result<(), CaptureError> {
    for character in value.chars() {
        let text = character.to_string();
        let keystroke = Keystroke {
            modifiers: Modifiers::none(),
            key: text.clone(),
            key_char: Some(text),
        };
        context
            .update_window(window, |_, window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            })
            .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        // Text input is delivered as a sequence of platform key events. Let
        // CE's InputState consume each character before the next one arrives;
        // otherwise a freshly-mounted field can lose the first character (or
        // the entire burst) while its focus/selection transaction is still
        // pending. This is the same event-loop boundary a native IME gives us.
        context.run_until_parked();
    }
    Ok(())
}

fn dispatch_mouse(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    x: f32,
    y: f32,
    button: &str,
    down: bool,
    driver_state: &mut DriverState,
) -> Result<(), CaptureError> {
    let button = mouse_button(button)?;
    let position = crate::position(x, y);
    context
        .update_window(window, |_, window, cx| {
            let event = if down {
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position,
                    modifiers: driver_state.modifiers,
                    click_count: 1,
                    first_mouse: false,
                })
            } else {
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position,
                    modifiers: driver_state.modifiers,
                    click_count: 1,
                })
            };
            window.dispatch_event(event, cx);
        })
        .map_err(|error| CaptureError::Gpui(error.to_string()))?;
    if down {
        driver_state.pressed_button = Some(button);
    } else {
        driver_state.pressed_button = None;
    }
    Ok(())
}

fn dispatch_mouse_move(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    x: f32,
    y: f32,
    driver_state: &DriverState,
) -> Result<(), CaptureError> {
    let event = gpui::MouseMoveEvent {
        position: crate::position(x, y),
        pressed_button: driver_state.pressed_button,
        modifiers: driver_state.modifiers,
    };
    context
        .update_window(window, |_, window, cx| {
            window.dispatch_event(event.to_platform_input(), cx);
        })
        .map_err(|error| CaptureError::Gpui(error.to_string()))
}

#[derive(Default)]
struct DriverState {
    modifiers: Modifiers,
    pressed_button: Option<MouseButton>,
}

fn mouse_button(value: &str) -> Result<MouseButton, CaptureError> {
    match value {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        "back" => Ok(MouseButton::Navigate(NavigationDirection::Back)),
        "forward" => Ok(MouseButton::Navigate(NavigationDirection::Forward)),
        other => Err(CaptureError::Input(InputError::MouseButton(
            other.to_owned(),
        ))),
    }
}

fn draw_and_capture(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<image::RgbaImage, CaptureError> {
    context
        .update_window(window, |_, window, cx| {
            window.simulate_next_frame(cx);
            let clear = window.draw(cx);
            clear.clear(cx);
            window.render_to_image()
        })
        .map_err(|error| CaptureError::Gpui(error.to_string()))?
        .map_err(|error| CaptureError::Gpui(error.to_string()))
}

/// Normalizes the renderer's device surface to the requested capture scale.
///
/// GPUI's deterministic `TestPlatform` currently renders through a fixed 2x
/// device surface. At a requested 1x scale the scene occupies the logical
/// viewport in the upper-left and the remainder of that surface is unused.
/// Cropping that unused backing area preserves the logical scene and produces
/// the requested physical artifact dimensions. A smaller renderer surface is
/// rejected rather than upscaled: upscaling a clipped scene would make the
/// artifact appear complete while losing the logical layout contract.
fn normalize_capture_image(
    image: image::RgbaImage,
    viewport: Viewport,
) -> Result<image::RgbaImage, CaptureError> {
    let expected = viewport.physical_size();
    if image.width() < expected.0 || image.height() < expected.1 {
        return Err(CaptureError::Gpui(format!(
            "renderer surface {}x{} is smaller than requested {}x{} for {}",
            image.width(),
            image.height(),
            expected.0,
            expected.1,
            viewport.suffix(),
        )));
    }
    if (image.width(), image.height()) == expected {
        return Ok(image);
    }
    Ok(image::imageops::crop_imm(&image, 0, 0, expected.0, expected.1).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
        Window, div, px, rgb,
    };
    use image::Rgba;

    #[test]
    fn one_x_capture_crops_the_fixed_retina_backing_gutter() {
        let viewport = Viewport::new(4, 3, 1).expect("viewport");
        let mut image = image::RgbaImage::from_pixel(8, 6, Rgba([0, 0, 0, 255]));
        for y in 0..3 {
            for x in 0..4 {
                image.put_pixel(x, y, Rgba([40, 80, 120, 255]));
            }
        }
        let normalized = normalize_capture_image(image, viewport).expect("normalized image");
        assert_eq!(normalized.dimensions(), (4, 3));
        assert!(
            normalized
                .pixels()
                .all(|pixel| *pixel == Rgba([40, 80, 120, 255]))
        );
    }

    #[test]
    fn two_x_capture_keeps_the_full_physical_scene() {
        let viewport = Viewport::new(4, 3, 2).expect("viewport");
        let image = image::RgbaImage::from_pixel(8, 6, Rgba([40, 80, 120, 255]));
        let normalized = normalize_capture_image(image, viewport).expect("normalized image");
        assert_eq!(normalized.dimensions(), (8, 6));
        assert!(
            normalized
                .pixels()
                .all(|pixel| *pixel == Rgba([40, 80, 120, 255]))
        );
    }

    #[test]
    fn a_clipped_renderer_surface_fails_closed() {
        let viewport = Viewport::new(4, 3, 2).expect("viewport");
        let image = image::RgbaImage::new(4, 3);
        assert!(normalize_capture_image(image, viewport).is_err());
    }

    #[test]
    fn time_zero_scale_preflight_keeps_a_far_edge_text_landmark() {
        let initial = Viewport::new(4, 3, 1).expect("viewport");
        let actions = [InputStep::Scale { factor: 2 }];
        let frames = [AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        }];
        let effective = preflight_viewport(initial, &actions, &frames).expect("preflight");
        assert_eq!(effective, Viewport::new(4, 3, 2).expect("viewport"));

        // A tiny rasterized "EDGE" landmark occupies the last four columns
        // of the physical scene. Cropping this as the original 1x viewport
        // would erase the final two columns and make the scale claim false.
        let mut image = image::RgbaImage::from_pixel(8, 6, Rgba([0, 0, 0, 255]));
        for (column, mask) in [0b1111_u8, 0b1001, 0b1011, 0b1111].into_iter().enumerate() {
            for row in 0..4 {
                if mask & (1 << row) != 0 {
                    image.put_pixel(4 + column as u32, row as u32, Rgba([255, 255, 255, 255]));
                }
            }
        }
        let normalized = normalize_capture_image(image, effective).expect("normalized image");
        assert_eq!(normalized.dimensions(), effective.physical_size());
        assert!(
            normalized.get_pixel(7, 3).0[0] > 0,
            "far edge landmark was cropped"
        );
    }

    #[test]
    fn viewport_changes_after_first_frame_are_preflighted_at_the_initial_size() {
        let viewport = Viewport::new(4, 3, 1).expect("viewport");
        let actions = [
            InputStep::Wait { milliseconds: 1 },
            InputStep::Scale { factor: 2 },
        ];
        let frames = [AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        }];
        assert_eq!(
            preflight_viewport(viewport, &actions, &frames).expect("preflight"),
            viewport
        );
    }

    struct EdgeLandmark;

    impl Render for EdgeLandmark {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .items_end()
                .justify_end()
                .bg(rgb(0x202020))
                .child(
                    div()
                        .id("edge-landmark")
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(0xff3b30))
                        .child("EDGE"),
                )
        }
    }

    #[test]
    fn headless_scale_preflight_renders_the_right_edge_landmark() {
        let viewport = Viewport::new(64, 32, 1).expect("viewport");
        let frames = [AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        }];
        let capture = capture_gpui_state(
            viewport,
            GuiState::new("edge-landmark", None, None),
            &[InputStep::Scale { factor: 2 }],
            &frames,
            GpuiCaptureOptions::default(),
            |_, cx| cx.new(|_| EdgeLandmark),
        )
        .expect("headless capture");
        assert_eq!(
            capture.viewport,
            Viewport::new(64, 32, 2).expect("viewport")
        );
        assert_eq!(capture.frames[0].image.dimensions(), (128, 64));
        let edge = capture.frames[0].image.get_pixel(127, 63).0;
        assert!(
            edge[0] > 180 && edge[1] < 120,
            "right edge control was cropped"
        );
    }

    #[test]
    fn headless_resize_after_first_frame_records_per_frame_geometry() {
        let viewport = Viewport::new(64, 32, 1).expect("viewport");
        let frames = [
            AnimationFrame {
                label: "start".to_owned(),
                time_ms: 0,
            },
            AnimationFrame {
                label: "resized".to_owned(),
                time_ms: 16,
            },
        ];
        let capture = capture_gpui_state(
            viewport,
            GuiState::new("edge-landmark-resize", None, None),
            &[
                InputStep::Wait { milliseconds: 16 },
                InputStep::Resize {
                    width: 32,
                    height: 16,
                },
            ],
            &frames,
            GpuiCaptureOptions::default(),
            |_, cx| cx.new(|_| EdgeLandmark),
        )
        .expect("headless capture");
        assert_eq!(
            capture.frames[0].viewport,
            Viewport::new(64, 32, 1).expect("viewport")
        );
        assert_eq!(
            capture.frames[1].viewport,
            Viewport::new(32, 16, 1).expect("viewport")
        );
        assert_eq!(capture.frames[0].image.dimensions(), (64, 32));
        assert_eq!(capture.frames[1].image.dimensions(), (32, 16));
    }
}
