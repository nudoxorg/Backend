//! Convenience crate that re-exports GPUI's platform traits and the
//! `current_platform` constructor so consumers don't need `#[cfg]` gating.

pub use gpui::Platform;

use std::rc::Rc;

/// Returns a background executor for the current platform.
pub fn background_executor() -> gpui::BackgroundExecutor {
    current_platform(true).background_executor()
}

pub fn application() -> gpui::Application {
    #[cfg(target_family = "wasm")]
    {
        let platform = Rc::new(gpui_web::WebPlatform::new(true));
        let http_client = std::sync::Arc::new(platform.fetch_http_client());
        gpui::Application::with_platform(platform).with_http_client(http_client)
    }

    #[cfg(not(target_family = "wasm"))]
    gpui::Application::with_platform(current_platform(false))
}

pub fn headless() -> gpui::Application {
    gpui::Application::with_platform(current_platform(true))
}

/// Unlike `application`, this function returns a single-threaded web application.
#[cfg(target_family = "wasm")]
pub fn single_threaded_web() -> gpui::Application {
    let platform = Rc::new(gpui_web::WebPlatform::new(false));
    let http_client = std::sync::Arc::new(platform.fetch_http_client());
    gpui::Application::with_platform(platform).with_http_client(http_client)
}

/// Initializes panic hooks and logging for the web platform.
/// Call this before running the application in a wasm_bindgen entrypoint.
#[cfg(target_family = "wasm")]
pub fn web_init() {
    console_error_panic_hook::set_once();
    gpui_web::init_logging();
}

/// Returns the default [`Platform`] for the current OS.
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    #[cfg(target_os = "macos")]
    {
        Rc::new(gpui_macos::MacPlatform::new(headless))
    }

    #[cfg(target_os = "windows")]
    {
        Rc::new(
            gpui_windows::WindowsPlatform::new(headless)
                .expect("failed to initialize Windows platform"),
        )
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        gpui_linux::current_platform(headless)
    }

    #[cfg(target_family = "wasm")]
    {
        let _ = headless;
        Rc::new(gpui_web::WebPlatform::new(true))
    }
}

/// Returns a new [`HeadlessRenderer`] for the current platform, if available.
///
/// NUDOX: `None` hides why; [`try_current_headless_renderer`] says.
#[cfg(feature = "test-support")]
pub fn current_headless_renderer() -> Option<Box<dyn gpui::PlatformHeadlessRenderer>> {
    try_current_headless_renderer().ok()
}

/// NUDOX: the variable that chooses which adapters a wgpu headless renderer
/// may use: `prefer-hardware` (the default: a window's order, the software
/// rasterizer last), `hardware`, or `software` (WARP on Windows).
#[cfg(feature = "test-support")]
pub const HEADLESS_ADAPTER_VAR: &str = "GPUI_HEADLESS_ADAPTER";

/// NUDOX: why there is no headless renderer.
#[cfg(feature = "test-support")]
#[derive(Debug)]
pub enum HeadlessRendererError {
    /// This platform (or this build of it) has no headless renderer.
    Unsupported(&'static str),
    /// [`HEADLESS_ADAPTER_VAR`] is set to something that is not a policy.
    AdapterPolicy(String),
    /// The renderer could not be created on any admitted adapter.
    Create(anyhow::Error),
}

#[cfg(feature = "test-support")]
impl std::fmt::Display for HeadlessRendererError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(why) => write!(formatter, "no headless renderer: {why}"),
            Self::AdapterPolicy(error) => write!(formatter, "{HEADLESS_ADAPTER_VAR}: {error}"),
            Self::Create(error) => write!(formatter, "creating the headless renderer: {error:#}"),
        }
    }
}

#[cfg(feature = "test-support")]
impl std::error::Error for HeadlessRendererError {}

/// NUDOX: a new headless renderer for the current platform, or why there is
/// none.
///
/// It is always the renderer this platform's windows draw with, so headless
/// pixels are the product's pixels: Metal on macOS; on Windows the wgpu
/// renderer's offscreen target, offered only when the `wgpu` feature makes
/// windows draw with wgpu too (otherwise they use DirectX 11, which has no
/// offscreen readback here).
///
/// # Errors
/// [`HeadlessRendererError`].
#[cfg(feature = "test-support")]
pub fn try_current_headless_renderer()
-> Result<Box<dyn gpui::PlatformHeadlessRenderer>, HeadlessRendererError> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(
            gpui_macos::metal_renderer::MetalHeadlessRenderer::new(),
        ))
    }

    #[cfg(all(target_os = "windows", feature = "wgpu"))]
    {
        let policy = match std::env::var(HEADLESS_ADAPTER_VAR) {
            Ok(spelling) => spelling
                .parse::<gpui_wgpu::HeadlessAdapterPolicy>()
                .map_err(|error| HeadlessRendererError::AdapterPolicy(error.to_string()))?,
            Err(std::env::VarError::NotPresent) => gpui_wgpu::HeadlessAdapterPolicy::default(),
            Err(error) => return Err(HeadlessRendererError::AdapterPolicy(error.to_string())),
        };
        gpui_wgpu::WgpuHeadlessRenderer::new(policy)
            .map(|renderer| Box::new(renderer) as Box<dyn gpui::PlatformHeadlessRenderer>)
            .map_err(HeadlessRendererError::Create)
    }

    #[cfg(all(target_os = "windows", not(feature = "wgpu")))]
    {
        Err(HeadlessRendererError::Unsupported(
            "Windows windows draw with DirectX 11 unless the `wgpu` feature is on, \
             and only the wgpu renderer has an offscreen target",
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(HeadlessRendererError::Unsupported(
            "only macOS and Windows (with the `wgpu` feature) have one",
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use gpui::{AppContext, Empty, VisualTestAppContext};
    use std::cell::RefCell;
    use std::time::Duration;

    // Note: All VisualTestAppContext tests are ignored by default because they require
    // the macOS main thread. Standard Rust tests run on worker threads, which causes
    // SIGABRT when interacting with macOS AppKit/Cocoa APIs.
    //
    // To run these tests, use:
    // cargo test -p gpui visual_test_context -- --ignored --test-threads=1

    #[test]
    #[ignore] // Requires macOS main thread
    fn test_foreground_tasks_run_with_run_until_parked() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        // Spawn a foreground task via the App's spawn method
        // This should use our TestDispatcher, not the MacDispatcher
        {
            let task_ran = task_ran.clone();
            cx.update(|cx| {
                cx.spawn(async move |_| {
                    *task_ran.borrow_mut() = true;
                })
                .detach();
            });
        }

        // The task should not have run yet
        assert!(!*task_ran.borrow());

        // Run until parked should execute the foreground task
        cx.run_until_parked();

        // Now the task should have run
        assert!(*task_ran.borrow());
    }

    #[test]
    #[ignore] // Requires macOS main thread
    fn test_advance_clock_triggers_delayed_tasks() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        // Spawn a task that waits for a timer
        {
            let task_ran = task_ran.clone();
            let executor = cx.background_executor.clone();
            cx.update(|cx| {
                cx.spawn(async move |_| {
                    executor.timer(Duration::from_millis(500)).await;
                    *task_ran.borrow_mut() = true;
                })
                .detach();
            });
        }

        // Run until parked - the task should be waiting on the timer
        cx.run_until_parked();
        assert!(!*task_ran.borrow());

        // Advance clock past the timer duration
        cx.advance_clock(Duration::from_millis(600));

        // Now the task should have completed
        assert!(*task_ran.borrow());
    }

    #[test]
    #[ignore] // Requires macOS main thread - window creation fails on test threads
    fn test_window_spawn_uses_test_dispatcher() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        let window = cx
            .open_offscreen_window_default(|_, cx| cx.new(|_| Empty))
            .expect("Failed to open window");

        // Spawn a task via window.spawn - this is the critical test case
        // for tooltip behavior, as tooltips use window.spawn for delayed show
        {
            let task_ran = task_ran.clone();
            cx.update_window(window.into(), |_, window, cx| {
                window
                    .spawn(cx, async move |_| {
                        *task_ran.borrow_mut() = true;
                    })
                    .detach();
            })
            .ok();
        }

        // The task should not have run yet
        assert!(!*task_ran.borrow());

        // Run until parked should execute the foreground task spawned via window
        cx.run_until_parked();

        // Now the task should have run
        assert!(*task_ran.borrow());
    }
}
