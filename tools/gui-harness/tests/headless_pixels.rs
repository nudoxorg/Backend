//! The headless renderer against independent pixel oracles.
//!
//! Every expectation here is computed from the scene's specification (a
//! rectangle's device-pixel bounds, a colour, an opacity, a clip edge), never
//! from an earlier render: a quad covers exactly its device pixels, alpha
//! composites by `src * a + dst * (1 - a)`, a clip leaves nothing beyond its
//! edge, and a glyph's ink stays inside its box and scales with the window.
//! The windows are the harness's own kind (`HeadlessAppContext` over the
//! platform's `current_headless_renderer`), so these are the pixels a capture
//! writes.
//!
//! `HEADLESS_PIXELS_OUT=<dir>` keeps every frame as `<dir>/<test>-<n>.png`
//! for a person to look at.
//!
//! Windows only: the one headless renderer this lane could run (the wgpu
//! offscreen target); the Metal renderer has its own pixel tests in apps/facet.
#![cfg(target_os = "windows")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    missing_docs
)]

use gpui::{
    AnyElement, AppContext as _, Context, HeadlessAppContext, IntoElement, ParentElement as _,
    PlatformHeadlessRenderer, Render, Scene, Styled as _, Window, div, px, rgb, size,
};
use gpui_wgpu::{HeadlessAdapterPolicy, WgpuHeadlessRenderer};
use image::{Rgba as Pixel, RgbaImage};
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

const GEIST_MONO: &[u8] = include_bytes!("../assets/fonts/GeistMono[wght].ttf");

const GROUND: u32 = 0x10_20_30;
const PLATE: u32 = 0xC0_80_40;

/// Renders whatever `build` returns, every frame.
struct Stage(Rc<dyn Fn() -> AnyElement>);

impl Render for Stage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        (self.0)()
    }
}

/// Draws `build` once in a `width` x `height` window at `scale` and reads
/// the frame back, through the platform's own headless renderer.
fn shoot(
    width: f32,
    height: f32,
    scale: f32,
    build: impl Fn() -> AnyElement + 'static,
) -> RgbaImage {
    shoot_with(
        gpui_platform::current_headless_renderer,
        width,
        height,
        scale,
        build,
    )
}

/// [`shoot`] with the window's renderer made by `renderer`.
fn shoot_with(
    renderer: impl Fn() -> Option<Box<dyn PlatformHeadlessRenderer>> + 'static,
    width: f32,
    height: f32,
    scale: f32,
    build: impl Fn() -> AnyElement + 'static,
) -> RgbaImage {
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(platform.text_system(), Arc::new(()), renderer);
    cx.update(|cx| cx.text_system().add_fonts(vec![Cow::Borrowed(GEIST_MONO)]))
        .expect("Geist Mono loads");
    let build: Rc<dyn Fn() -> AnyElement> = Rc::new(build);
    let handle = cx
        .open_window(size(px(width), px(height)), move |window, cx| {
            window.set_scale_factor(scale);
            cx.new(|_| Stage(build))
        })
        .expect("headless window");
    let image = cx
        .update_window(handle.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.render_to_image()
        })
        .expect("window update")
        .expect("frame read back");
    keep(&image);
    image
}

/// Saves `image` under `HEADLESS_PIXELS_OUT`, named for the running test.
fn keep(image: &RgbaImage) {
    static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Some(dir) = std::env::var_os("HEADLESS_PIXELS_OUT") else {
        return;
    };
    let test = std::thread::current()
        .name()
        .unwrap_or("frame")
        .replace("::", "-");
    let n = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::fs::create_dir_all(&dir).expect("output directory");
    image
        .save(std::path::Path::new(&dir).join(format!("{test}-{n}.png")))
        .expect("save frame");
}

fn opaque(hex: u32) -> Pixel<u8> {
    let [_, red, green, blue] = hex.to_be_bytes();
    Pixel([red, green, blue, u8::MAX])
}

/// A ground with one opaque plate at `(x, y, w, h)` logical pixels.
fn plate_scene(x: f32, y: f32, w: f32, h: f32) -> AnyElement {
    div()
        .size_full()
        .bg(rgb(GROUND))
        .child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .h(px(h))
                .bg(rgb(PLATE)),
        )
        .into_any_element()
}

/// Asserts every pixel is `inside` within the device rectangle and `outside`
/// elsewhere, naming the first mismatch.
fn assert_rectangle(
    image: &RgbaImage,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
    inside: Pixel<u8>,
    outside: Pixel<u8>,
) {
    for (x, y, pixel) in image.enumerate_pixels() {
        let expected = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
            inside
        } else {
            outside
        };
        assert_eq!(
            *pixel,
            expected,
            "pixel ({x}, {y}) of a {}x{} frame",
            image.width(),
            image.height()
        );
    }
}

#[test]
fn a_quad_covers_exactly_its_device_pixels_at_widths_that_need_row_padding() {
    // 37 px rows are 148 bytes, padded to 256 for the copy: the padding must
    // not reach the image.
    let image = shoot(37.0, 23.0, 1.0, || plate_scene(10.0, 7.0, 20.0, 11.0));
    assert_eq!(image.dimensions(), (37, 23));
    assert_rectangle(&image, (10, 7, 30, 18), opaque(PLATE), opaque(GROUND));
}

#[test]
fn scale_factors_multiply_the_device_frame_and_every_edge() {
    for (scale, frame, plate) in [
        (2.0, (80, 60), (20, 16, 60, 40)),
        (1.5, (60, 45), (15, 12, 45, 30)),
    ] {
        let image = shoot(40.0, 30.0, scale, || plate_scene(10.0, 8.0, 20.0, 12.0));
        assert_eq!(image.dimensions(), frame, "scale {scale}");
        assert_rectangle(&image, plate, opaque(PLATE), opaque(GROUND));
    }
}

#[test]
fn opacity_composites_over_the_ground_by_the_alpha_equation() {
    let alpha = 0.5_f32;
    let image = shoot(32.0, 16.0, 1.0, move || {
        div()
            .size_full()
            .bg(rgb(GROUND))
            .child(
                div()
                    .absolute()
                    .left(px(8.0))
                    .top(px(4.0))
                    .w(px(16.0))
                    .h(px(8.0))
                    .bg(rgb(PLATE))
                    .opacity(alpha),
            )
            .into_any_element()
    });
    let [_, gr, gg, gb] = GROUND.to_be_bytes();
    let [_, pr, pg, pb] = PLATE.to_be_bytes();
    let mix = |src: u8, dst: u8| f32::from(src) * alpha + f32::from(dst) * (1.0 - alpha);
    let expected = [mix(pr, gr), mix(pg, gg), mix(pb, gb)];
    for y in 4..12 {
        for x in 8..24 {
            let pixel = image.get_pixel(x, y);
            for (channel, want) in pixel.0[..3].iter().zip(expected) {
                assert!(
                    (f32::from(*channel) - want).abs() <= 1.0,
                    "({x}, {y}) is {pixel:?}, the alpha equation gives {expected:?}"
                );
            }
            assert_eq!(pixel.0[3], u8::MAX, "an opaque window shows opaque pixels");
        }
    }
    assert_eq!(*image.get_pixel(0, 0), opaque(GROUND));
    assert_eq!(*image.get_pixel(24, 12), opaque(GROUND));
}

#[test]
fn a_clip_leaves_nothing_of_its_child_beyond_its_edge() {
    let image = shoot(40.0, 30.0, 1.0, || {
        div()
            .size_full()
            .bg(rgb(GROUND))
            .child(
                div()
                    .absolute()
                    .left(px(5.0))
                    .top(px(6.0))
                    .w(px(10.0))
                    .h(px(12.0))
                    .overflow_hidden()
                    .child(
                        // Four times the clip in each direction.
                        div()
                            .absolute()
                            .left(px(-20.0))
                            .top(px(-20.0))
                            .w(px(60.0))
                            .h(px(60.0))
                            .bg(rgb(PLATE)),
                    ),
            )
            .into_any_element()
    });
    assert_rectangle(&image, (5, 6, 15, 18), opaque(PLATE), opaque(GROUND));
}

/// Black text in Geist Mono on white, inside a box at a known place.
fn text_scene(text: &'static str, opacity: f32) -> impl Fn() -> AnyElement + 'static {
    move || {
        div()
            .size_full()
            .bg(rgb(0xFF_FF_FF))
            .child(
                div()
                    .absolute()
                    .left(px(16.0))
                    .top(px(12.0))
                    .w(px(160.0))
                    .h(px(80.0))
                    .font_family("Geist Mono")
                    .text_size(px(64.0))
                    .line_height(px(80.0))
                    .text_color(rgb(0x00_00_00))
                    .opacity(opacity)
                    .child(text),
            )
            .into_any_element()
    }
}

/// The bounding box of every pixel that is not the white ground, or `None`.
fn ink(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    image
        .enumerate_pixels()
        .filter(|(_, _, pixel)| pixel.0[..3] != [u8::MAX; 3])
        .fold(None, |bounds, (x, y, _)| {
            let (x0, y0, x1, y1) = bounds.unwrap_or((x, y, x, y));
            Some((x0.min(x), y0.min(y), x1.max(x), y1.max(y)))
        })
}

#[test]
fn text_ink_is_real_confined_to_its_box_and_fully_covered_stems_are_the_text_colour() {
    let image = shoot(200.0, 110.0, 1.0, text_scene("Il", 1.0));
    let (x0, y0, x1, y1) = ink(&image).expect("the glyphs drew ink");
    assert!(
        x0 >= 16 && y0 >= 12 && x1 < 176 && y1 < 92,
        "ink {:?} escaped its 160x80 box at (16, 12)",
        (x0, y0, x1, y1)
    );
    // A 64 px stem is several pixels wide: its interior is fully covered.
    let black = image
        .pixels()
        .filter(|pixel| pixel.0 == [0, 0, 0, u8::MAX])
        .count();
    assert!(
        black >= 64,
        "only {black} fully covered pixels: the glyph mask is not reaching full coverage"
    );
    // Two glyphs of a monospace font: the ink spans more than one advance.
    assert!(
        x1 - x0 > 40,
        "ink {:?} is too narrow for two 64 px glyphs",
        (x0, x1)
    );
}

#[test]
fn text_ink_scales_with_the_window_and_repeats_byte_for_byte() {
    let one = shoot(200.0, 110.0, 1.0, text_scene("Il", 1.0));
    let again = shoot(200.0, 110.0, 1.0, text_scene("Il", 1.0));
    assert_eq!(
        one.as_raw(),
        again.as_raw(),
        "the same scene must read back the same bytes"
    );
    let two = shoot(200.0, 110.0, 2.0, text_scene("Il", 1.0));
    let (a, b) = (ink(&one).expect("1x ink"), ink(&two).expect("2x ink"));
    let span =
        |(x0, y0, x1, y1): (u32, u32, u32, u32)| (f64::from(x1 - x0 + 1), f64::from(y1 - y0 + 1));
    let ((w1, h1), (w2, h2)) = (span(a), span(b));
    // Glyphs rasterize at each scale (hinting, pixel snapping), so the ratio
    // holds to a pixel or two at each edge, not exactly.
    assert!(
        (w2 - 2.0 * w1).abs() <= 3.0,
        "ink width {w1} at 1x, {w2} at 2x"
    );
    assert!(
        (h2 - 2.0 * h1).abs() <= 3.0,
        "ink height {h1} at 1x, {h2} at 2x"
    );
    assert!(
        (f64::from(b.0) - 2.0 * f64::from(a.0)).abs() <= 2.0,
        "ink left edge {} at 1x, {} at 2x",
        a.0,
        b.0
    );
}

#[test]
fn transparent_text_draws_no_ink_and_clipped_text_none_beyond_the_clip() {
    let invisible = shoot(200.0, 110.0, 1.0, text_scene("Il", 0.0));
    assert_eq!(
        ink(&invisible),
        None,
        "zero opacity must leave the ground untouched"
    );

    let clipped = shoot(200.0, 110.0, 1.0, || {
        div()
            .size_full()
            .bg(rgb(0xFF_FF_FF))
            .child(
                div()
                    .absolute()
                    .left(px(16.0))
                    .top(px(12.0))
                    .w(px(50.0))
                    .h(px(80.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(400.0))
                            .font_family("Geist Mono")
                            .text_size(px(64.0))
                            .line_height(px(80.0))
                            .text_color(rgb(0x00_00_00))
                            .child("MMMMMM"),
                    ),
            )
            .into_any_element()
    });
    let (x0, _, x1, _) = ink(&clipped).expect("the clipped text drew ink");
    assert!(x0 >= 16, "ink starts at {x0}, left of the clip");
    // The clip's last column is x = 65; the second M's stem crosses it.
    assert!(
        (58..=65).contains(&x1),
        "ink ends at x = {x1}; the clip ends at 65"
    );
}

#[test]
fn the_renderer_names_its_gpu_and_refuses_impossible_frames() {
    let mut renderer =
        gpui_platform::try_current_headless_renderer().expect("a Windows headless renderer");
    let specs = renderer.gpu_specs().expect("a wgpu renderer names its GPU");
    assert!(!specs.device_name.is_empty());
    let scene = Scene::default();
    for (width, height) in [(0, 10), (10, 0), (-1, 10)] {
        let error = renderer
            .render_scene_to_image(
                &scene,
                size(gpui::DevicePixels(width), gpui::DevicePixels(height)),
            )
            .expect_err("an empty frame is refused");
        assert!(error.to_string().contains("empty"), "{error}");
    }
    let error = renderer
        .render_scene_to_image(
            &scene,
            size(gpui::DevicePixels(1 << 20), gpui::DevicePixels(8)),
        )
        .expect_err("a frame wider than any texture is refused");
    assert!(error.to_string().contains("texture limit"), "{error}");
    // The renderer stays usable after a refused frame.
    let image = renderer
        .render_scene_to_image(&scene, size(gpui::DevicePixels(3), gpui::DevicePixels(2)))
        .expect("an empty scene renders");
    assert_eq!(image.dimensions(), (3, 2));
    assert!(
        image.pixels().all(|pixel| pixel.0 == [0, 0, 0, u8::MAX]),
        "an empty opaque frame is black"
    );
}

#[test]
fn the_software_rasterizer_says_so_and_draws_the_same_exact_quads() {
    let warp = WgpuHeadlessRenderer::new(HeadlessAdapterPolicy::SoftwareOnly)
        .expect("WARP ships with every Windows 10+ install");
    let specs = warp.gpu_specs().expect("specs");
    assert!(specs.is_software_emulated, "{specs:?}");
    drop(warp);
    let software = || {
        WgpuHeadlessRenderer::new(HeadlessAdapterPolicy::SoftwareOnly)
            .ok()
            .map(|renderer| Box::new(renderer) as Box<dyn PlatformHeadlessRenderer>)
    };
    let image = shoot_with(software, 37.0, 23.0, 2.0, || {
        plate_scene(10.0, 7.0, 20.0, 11.0)
    });
    assert_eq!(image.dimensions(), (74, 46));
    assert_rectangle(&image, (20, 14, 60, 36), opaque(PLATE), opaque(GROUND));
}

#[test]
fn hardware_only_never_settles_for_the_software_rasterizer() {
    // Where no hardware adapter exists this must fail rather than quietly
    // draw on WARP; where one exists it must be hardware.
    match WgpuHeadlessRenderer::new(HeadlessAdapterPolicy::HardwareOnly) {
        Ok(renderer) => {
            let specs = renderer.gpu_specs().expect("specs");
            assert!(!specs.is_software_emulated, "{specs:?}");
        }
        Err(error) => assert!(error.to_string().contains("hardware"), "{error:#}"),
    }
}
