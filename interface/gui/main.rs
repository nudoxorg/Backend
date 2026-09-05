//! Runs the `interface-gui` executable, which exists to render and control the unified application service through GPUI.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! Production GPUI host for the unified application service.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use compiler_application::LocalCompilerHost;
use gpui::prelude::*;
use gpui::{App, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use interface_core::ApplicationService;
use interface_gui::GpuiShellView;

const WINDOW_TITLE: &str = "Hummingbird Docs";
const APPLICATION_ID: &str = "dev.hummingbird.application";

fn main() {
    let compiler = match LocalCompilerHost::production().open() {
        Ok(compiler) => compiler,
        Err(error) => {
            eprintln!("{WINDOW_TITLE} could not establish its local compiler: {error:#}");
            return;
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(940.0), px(650.0))),
                app_id: Some(APPLICATION_ID.to_owned()),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from(WINDOW_TITLE)),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    GpuiShellView::new(ApplicationService::with_compiler(compiler), window, cx)
                })
            },
        );
        match window {
            Ok(_) => cx.activate(true),
            Err(error) => {
                eprintln!("{WINDOW_TITLE} could not open its application window: {error}");
                cx.quit();
            }
        }
    });
}
