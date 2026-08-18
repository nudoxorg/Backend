//! Probe: can we rasterise a real GPUI scene to a PNG with no display?
//!
//! This exists to answer one question before any screenshot suite is built on
//! top of it. It is deliberately the smallest possible program that proves the
//! whole chain: real text system → real Metal renderer → real scene → pixels.
//!
//! `harness = false` is load-bearing. libtest runs `#[test]` functions on
//! spawned worker threads; macOS platform construction touches AppKit, which
//! aborts off the main thread. With no harness this file *is* `fn main`, so it
//! runs on the process main thread where AppKit is legal.

use std::sync::Arc;

use gpui::Platform as _;
use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, div, px, size,
};

struct Probe;

impl Render for Probe {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(gpui::rgb(0x11131a)).child(
            div()
                .m(px(48.))
                .p(px(24.))
                .bg(gpui::rgb(0x1e2230))
                .text_color(gpui::rgb(0xe6e8ef))
                .child("nudox screenshot probe — real glyphs, real renderer"),
        )
    }
}

fn main() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/shots");
    std::fs::create_dir_all(&out).expect("create tests/shots");

    let platform = gpui_platform::current_platform(true);
    let text_system = platform.text_system();

    let mut cx = gpui::HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_component_assets::Assets),
        || gpui_platform::current_headless_renderer(),
    );

    let window = cx
        .open_window(size(px(900.), px(400.)), |_, cx| cx.new(|_| Probe))
        .expect("open headless window");

    cx.run_until_parked();

    let image = cx
        .capture_screenshot(window.into())
        .expect("capture_screenshot must work: this is the whole point of the probe");

    let path = out.join("probe.png");
    image.save(&path).expect("save png");

    println!(
        "PROBE_OK {}x{} -> {}",
        image.width(),
        image.height(),
        path.display()
    );
    assert!(image.width() > 0 && image.height() > 0, "empty image");

    // A picture of nothing would still "succeed" above. Assert the scene
    // actually painted: the background is a dark blue-grey, so a fully white or
    // fully transparent buffer means we rasterised an empty scene.
    let non_blank = image.pixels().filter(|p| p.0[3] > 0).count();
    assert!(
        non_blank > (image.width() * image.height() / 2) as usize,
        "image is mostly transparent ({non_blank} opaque px) — the scene did not paint",
    );
    println!("PROBE_PAINTED {non_blank} opaque pixels");
}
