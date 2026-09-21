//! Direct offscreen GPUI CE capture and deterministic input driving.

use crate::{
    AnimationFrame, CaptureError, CaptureRecord, CaptureSet, GuiState, InputError, InputStep,
    Viewport,
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
    mut input_hook: I,
    build_root: F,
) -> Result<CaptureSet, CaptureError>
where
    V: Render + 'static,
    F: FnOnce(&mut Window, &mut App) -> Entity<V>,
    H: FnMut(&AnimationFrame, &mut Window, &mut App),
    I: FnMut(&InputStep, &mut Window, &mut App),
{
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
                &mut input_hook,
            )?;
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
            .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        let image = draw_and_capture(&mut context, window)?;
        records.push(CaptureRecord {
            label: frame.label.clone(),
            time_ms: frame.time_ms,
            image,
            input_index,
            diff: None,
        });
        elapsed = frame.time_ms;
    }
    Ok(CaptureSet {
        state,
        frames: records,
    })
}

fn apply_step(
    context: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    step: &InputStep,
    driver_state: &mut DriverState,
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
        InputStep::ImeText { value } => {
            dispatch_text(context, window, value)?;
        }
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
            context
                .update_window(window, |_, window, _| {
                    window.resize(size(px(*width as f32), px(*height as f32)))
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Scale { factor } => {
            if !matches!(factor, 1 | 2) {
                return Err(CaptureError::InvalidConfig(format!(
                    "unsupported GPUI scale factor {factor}; expected 1 or 2"
                )));
            }
            context
                .update_window(window, |_, window, _| {
                    window.set_scale_factor(f32::from(*factor))
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
        InputStep::Theme { .. } | InputStep::Locale { .. } => {
            context
                .update_window(window, |_, window, cx| input_hook(step, window, cx))
                .map_err(|error| CaptureError::Gpui(error.to_string()))?;
        }
    }
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
