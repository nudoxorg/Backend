//! The compositing layer and chamfer shadows through the real Metal renderer
//! (headless), against expectations computed independently of the code
//! under test.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use super::chamfers;
use crate::fonts::Typeset;
use crate::gallery;
use crate::icons::Assets;
use crate::motion::offset;
use crate::paint::{Bevel, Chamfer, cut};
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::{TypeRole, ty};
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, Context, HeadlessAppContext, IntoElement,
    ParentElement, Render, Styled, Window, canvas, div, hsla, layer, point, px, size,
};
use image::RgbaImage;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Where to save evidence images (`FLOW_EVIDENCE=<dir>`), if anywhere.
fn evidence_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("FLOW_EVIDENCE")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The headless macOS platform owns process-global state: one at a time.
static PLATFORM: Mutex<()> = Mutex::new(());

fn platform() -> MutexGuard<'static, ()> {
    PLATFORM.lock().unwrap_or_else(PoisonError::into_inner)
}

type Build = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

struct Stage {
    build: Build,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.palette();
        div()
            .size_full()
            .bg(palette.g1)
            .child((self.build)(window, cx))
    }
}

/// Draws `build` once at 2x in a `w` x `h` window and reads the pixels back.
fn shoot(w: f32, h: f32, build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> RgbaImage {
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| gallery::bootstrap(Facet::default(), false, cx))
        .expect("bootstrap");
    let build: Build = Rc::new(build);
    let handle = cx
        .open_window(size(px(w), px(h)), move |window, cx| {
            window.set_scale_factor(2.0);
            cx.new(|_| Stage { build })
        })
        .expect("window");
    let any: gpui::AnyWindowHandle = handle.into();
    cx.update_window(any, |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        window.render_to_image()
    })
    .expect("update")
    .expect("render")
}

/// A card whose primitives overlap (the cut plate's bevel halves under its
/// fill, text over the fill), every length times `k`.
fn card(k: f32, cx: &App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let scaled = |role: TypeRole| TypeRole {
        size: role.size * k,
        line: role.line * k,
        ..role
    };
    cut()
        .chamfer(Chamfer::Px(14.0 * k))
        .bevel(Bevel::Hot)
        .w(px(260.0 * k))
        .p(px(16.0 * k))
        .flex()
        .flex_col()
        .gap(px(6.0 * k))
        .child(
            div()
                .typeset(scaled(ty::HEAD), &facet)
                .text_color(palette.ink0.hsla())
                .child("serde_json::Value"),
        )
        .child(
            div()
                .typeset(scaled(ty::MONO_ROW), &facet)
                .text_color(palette.ink2.hsla())
                .child("pub enum Value { Null, Bool(bool) }"),
        )
        .into_any_element()
}

fn placed(child: AnyElement) -> AnyElement {
    div().absolute().left(px(40.0)).top(px(40.0)).child(child).into_any_element()
}

fn max_diff(a: &RgbaImage, b: &RgbaImage) -> u8 {
    a.as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

#[test]
fn a_layer_at_rest_paints_bit_identical_pixels() {
    let _platform = platform();
    let bare = shoot(360.0, 200.0, |_, cx| placed(card(1.0, cx)));
    let rested = shoot(360.0, 200.0, |_, cx| {
        placed(layer(card(1.0, cx)).opacity(1.0).scale(1.0).into_any_element())
    });
    assert_eq!(bare.dimensions(), rested.dimensions());
    assert!(bare.as_raw() == rested.as_raw(), "max channel diff {}", max_diff(&bare, &rested));
    // Not trivially equal to an empty frame: the card is really there.
    let empty = shoot(360.0, 200.0, |_, _| div().into_any_element());
    assert!(max_diff(&bare, &empty) > 100);
}

/// Group opacity composites the subtree once: over an opaque background the
/// result is exactly `alpha * rest + (1 - alpha) * background`, whatever the
/// subtree's coverage. Per-primitive opacity is not (the bevel halves show
/// through the fill, the fill shows through the text).
#[test]
fn group_opacity_is_the_rest_frame_blended_over_the_background() {
    let _platform = platform();
    let alpha = 0.45;
    let rest = shoot(360.0, 200.0, |_, cx| placed(card(1.0, cx)));
    let background = shoot(360.0, 200.0, |_, _| div().into_any_element());
    let group = shoot(360.0, 200.0, move |_, cx| {
        placed(layer(card(1.0, cx)).opacity(alpha).into_any_element())
    });
    let per_primitive = shoot(360.0, 200.0, move |_, cx| {
        placed(offset(card(1.0, cx)).opacity(alpha).into_any_element())
    });
    let error = |image: &RgbaImage| {
        let mut worst = 0.0_f32;
        let mut sum = 0.0_f32;
        let mut count = 0.0_f32;
        for ((got, r), b) in image
            .pixels()
            .zip(rest.pixels())
            .zip(background.pixels())
        {
            for c in 0..3 {
                let expected = alpha * f32::from(r[c]) + (1.0 - alpha) * f32::from(b[c]);
                let e = (f32::from(got[c]) - expected).abs();
                worst = worst.max(e);
                sum += e;
                count += 1.0;
            }
        }
        (worst, sum / count)
    };
    let (group_worst, group_mean) = error(&group);
    let (prim_worst, prim_mean) = error(&per_primitive);
    eprintln!(
        "group opacity vs alpha-blend of the rest frame: worst {group_worst:.2}/255, mean {group_mean:.4}; \
         per-primitive: worst {prim_worst:.2}/255, mean {prim_mean:.4}"
    );
    assert!(group_worst <= 2.0, "group opacity is a true group: worst {group_worst}");
    assert!(prim_worst >= 12.0, "per-primitive opacity really differs: worst {prim_worst}");
}

/// Peak edge strength inside `region` (x0, y0, x1, y1): the mean of the
/// strongest 5 % of Sobel gradient magnitudes of the luminance. Resampling
/// blur lowers exactly these; flat areas do not dilute it.
fn sharpness(image: &RgbaImage, region: (u32, u32, u32, u32)) -> f32 {
    let lum = |x: u32, y: u32| {
        let p = image.get_pixel(x, y);
        0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
    };
    let mut magnitudes = Vec::new();
    for y in region.1 + 1..region.3 - 1 {
        for x in region.0 + 1..region.2 - 1 {
            let gx = lum(x + 1, y - 1) + 2.0 * lum(x + 1, y) + lum(x + 1, y + 1)
                - lum(x - 1, y - 1)
                - 2.0 * lum(x - 1, y)
                - lum(x - 1, y + 1);
            let gy = lum(x - 1, y + 1) + 2.0 * lum(x, y + 1) + lum(x + 1, y + 1)
                - lum(x - 1, y - 1)
                - 2.0 * lum(x, y - 1)
                - lum(x + 1, y - 1);
            magnitudes.push((gx * gx + gy * gy).sqrt());
        }
    }
    magnitudes.sort_by(f32::total_cmp);
    let top = &magnitudes[magnitudes.len() * 95 / 100..];
    top.iter().sum::<f32>() / top.len() as f32
}

/// What an offscreen-texture layer would composite at scale `k`: the rest
/// frame sampled bilinearly about `origin` (device px). The GPU's linear
/// sampler computes exactly this.
fn bilinear_zoom(image: &RgbaImage, k: f32, origin: (f32, f32)) -> RgbaImage {
    let (w, h) = image.dimensions();
    RgbaImage::from_fn(w, h, |x, y| {
        let sx = origin.0 + (x as f32 + 0.5 - origin.0) / k - 0.5;
        let sy = origin.1 + (y as f32 + 0.5 - origin.1) / k - 0.5;
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let at = |x: f32, y: f32| {
            let x = (x.max(0.0) as u32).min(w - 1);
            let y = (y.max(0.0) as u32).min(h - 1);
            *image.get_pixel(x, y)
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        let mut out = [0_u8; 4];
        for i in 0..4 {
            let top = f32::from(a[i]) * (1.0 - fx) + f32::from(b[i]) * fx;
            let bottom = f32::from(c[i]) * (1.0 - fx) + f32::from(d[i]) * fx;
            out[i] = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
        image::Rgba(out)
    })
}

fn mean_diff(a: &RgbaImage, b: &RgbaImage, region: (u32, u32, u32, u32)) -> f32 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in region.1..region.3 {
        for x in region.0..region.2 {
            let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
            for c in 0..3 {
                sum += f32::from(p[c].abs_diff(q[c]));
                n += 1.0;
            }
        }
    }
    sum / n
}

/// The design evidence for per-primitive transforms over an offscreen
/// texture: a card scaled 1.5x by the layer against the same card laid out
/// natively at 1.5x, and against what a texture layer would show (the rest
/// frame resampled). The layer re-rasterizes glyphs, so it matches the native
/// card; the texture is a blurred upscale.
#[test]
fn a_scaled_layer_is_as_sharp_as_native_layout() {
    let _platform = platform();
    let k = 1.5;
    let rest = shoot(480.0, 260.0, |_, cx| placed(card(1.0, cx)));
    let native = shoot(480.0, 260.0, move |_, cx| placed(card(k, cx)));
    let scaled = shoot(480.0, 260.0, move |_, cx| {
        placed(layer(card(1.0, cx)).scale(k).origin(0.0, 0.0).into_any_element())
    });
    // The card's top-left is at (40, 40) logical = (80, 80) device.
    let texture = bilinear_zoom(&rest, k, (80.0, 80.0));
    let region = (80, 80, 80 + (260.0 * k * 2.0) as u32, 80 + 150);
    let (s_native, s_scaled, s_texture) = (
        sharpness(&native, region),
        sharpness(&scaled, region),
        sharpness(&texture, region),
    );
    let (d_scaled, d_texture) = (mean_diff(&scaled, &native, region), mean_diff(&texture, &native, region));
    eprintln!(
        "scale {k}: peak edge strength native {s_native:.1}, layer {s_scaled:.1}, texture {s_texture:.1}; \
         mean |diff| to native: layer {d_scaled:.3}, texture {d_texture:.3}"
    );
    if let Some(dir) = evidence_dir() {
        let _ = native.save(dir.join("scale-native.png"));
        let _ = scaled.save(dir.join("scale-layer.png"));
        let _ = texture.save(dir.join("scale-texture.png"));
    }
    assert!((s_scaled - s_native).abs() < 0.05 * s_native, "layer {s_scaled} vs native {s_native}");
    assert!(s_texture < 0.9 * s_native, "texture {s_texture} vs native {s_native}");
    assert!(d_scaled < d_texture, "layer {d_scaled} vs texture {d_texture}");
}

/// A shadow painted black on white reads back as coverage: 1 - value/255.
fn coverage(image: &RgbaImage, x: u32, y: u32) -> f32 {
    1.0 - f32::from(image.get_pixel(x, y)[0]) / 255.0
}

/// The exact blurred chamfered rectangle, on the CPU: the polygon's mask
/// (4x4 supersampled per device pixel) convolved with a separable gaussian.
fn reference_shadow(
    (w, h): (usize, usize),
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    chamfer: f32,
    sigma: f32,
) -> Vec<f32> {
    let inside = |x: f32, y: f32| {
        x >= x0 && x <= x1 && y >= y0 && y <= y1 && (x - x0) + (y - y0) >= chamfer && (x1 - x) + (y1 - y) >= chamfer
    };
    let mut mask = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if inside(px, py) {
                        hits += 1;
                    }
                }
            }
            mask[y * w + x] = hits as f32 / 16.0;
        }
    }
    let radius = (3.0 * sigma).ceil() as i64;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let norm: f32 = kernel.iter().sum();
    let blur = |src: &[f32], horizontal: bool| {
        let mut out = vec![0.0_f32; w * h];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let mut acc = 0.0;
                for (i, weight) in kernel.iter().enumerate() {
                    let d = i as i64 - radius;
                    let (sx, sy) = if horizontal { (x + d, y) } else { (x, y + d) };
                    if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                        acc += weight * src[(sy as usize) * w + sx as usize];
                    }
                }
                out[(y as usize) * w + x as usize] = acc / norm;
            }
        }
        out
    };
    blur(&blur(&mask, true), false)
}

/// The chamfer shadow against the exact blurred polygon, and the rounded box
/// shadow it replaces against the same reference.
#[test]
fn chamfer_shadows_match_the_blurred_polygon() {
    let _platform = platform();
    let (w, h) = (360.0, 240.0);
    let (bx, by, bw, bh) = (80.0, 50.0, 200.0, 90.0);
    let (chamfer, blur, drop) = (22.0, 15.0, 18.0);
    let shadow = BoxShadow {
        color: hsla(0.0, 0.0, 0.0, 1.0),
        offset: point(px(0.0), px(drop)),
        blur_radius: px(blur),
        spread_radius: px(0.0),
        inset: false,
    };
    let paint = move |chamfered: bool| {
        let shadow = shadow.clone();
        move |_: &mut Window, _: &mut App| {
            let shadows = [shadow.clone()];
            div()
                .size_full()
                .bg(gpui::white())
                .child(
                    canvas(
                        |_, _, _| {},
                        move |_, (), window, _| {
                            let bounds = gpui::Bounds::new(point(px(bx), px(by)), size(px(bw), px(bh)));
                            if chamfered {
                                window.paint_chamfer_shadows(bounds, chamfers(chamfer), &shadows);
                            } else {
                                window.paint_drop_shadows(
                                    bounds,
                                    gpui::Corners::all(px(chamfer * 0.5)),
                                    &shadows,
                                );
                            }
                        },
                    )
                    .size_full(),
                )
                .into_any_element()
        }
    };
    let exact = shoot(w, h, paint(true));
    let rounded = shoot(w, h, paint(false));
    let (dw, dh) = (exact.width() as usize, exact.height() as usize);
    let reference = reference_shadow(
        (dw, dh),
        (bx * 2.0, (by + drop) * 2.0, (bx + bw) * 2.0, (by + drop + bh) * 2.0),
        chamfer * 2.0,
        blur * 2.0,
    );
    let error = |image: &RgbaImage| {
        let (mut worst, mut sum) = (0.0_f32, 0.0_f32);
        for y in 0..dh {
            for x in 0..dw {
                let e = (coverage(image, x as u32, y as u32) - reference[y * dw + x]).abs();
                worst = worst.max(e);
                sum += e;
            }
        }
        (worst, sum / (dw * dh) as f32)
    };
    let ((chamfer_worst, chamfer_mean), (rounded_worst, rounded_mean)) = (error(&exact), error(&rounded));
    eprintln!(
        "coverage error vs the exact blurred polygon: chamfer shadow worst {chamfer_worst:.4} mean {chamfer_mean:.5}; \
         rounded box shadow worst {rounded_worst:.4} mean {rounded_mean:.5}"
    );
    if let Some(dir) = evidence_dir() {
        let _ = exact.save(dir.join("shadow-chamfer.png"));
        let _ = rounded.save(dir.join("shadow-rounded.png"));
    }
    assert!(chamfer_worst < 0.03, "chamfer shadow worst error {chamfer_worst}");
    assert!(chamfer_mean < rounded_mean, "{chamfer_mean} vs {rounded_mean}");
}

/// Frame cost (CPU: render + layout + prepaint + paint, then the GPU frame to
/// completion via a read-back) at 1440x900 @2x: 20 cards, with 0 layers, 1
/// animating layer, 20 animating layers (scale + group opacity), and a
/// 200-row FLIP reorder in flight. Release:
/// `cargo test --release -p backend-facet --features gallery --lib layer_timing -- --ignored --nocapture`
#[test]
#[ignore = "timing; run explicitly in release"]
fn layer_timing() {
    use crate::motion::{Flow, request_frame};
    use gpui::ElementId;
    use std::time::Instant;

    #[derive(Clone, Copy, PartialEq)]
    enum Case {
        Bare,
        OneLayer,
        TwentyLayers,
        Flip200,
    }

    struct Bench {
        case: Case,
        frame: u32,
        flow: Flow,
    }

    impl Render for Bench {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // Every frame animates: ask for the next one before borrowing cx.
            request_frame(window, cx);
            let phase = self.frame as f32 / 30.0;
            let facet = cx.facet();
            let root = div()
                .size_full()
                .bg(facet.palette().g1)
                .p(px(20.0))
                .flex()
                .flex_wrap()
                .gap(px(12.0));
            if self.case == Case::Flip200 {
                // A reorder every 20 frames keeps 200 rows flowing.
                self.flow.epoch(self.frame / 20);
                let shift = (self.frame / 20) as usize;
                let rows = (0..200).map(|i| {
                    let key = (i * 37 + shift * 11) % 200;
                    self.flow.item(
                        ElementId::Integer(key as u64),
                        cut().chamfer(Chamfer::Sm).w(px(260.0)).h(px(24.0)).px(px(8.0))
                            .typeset(ty::MONO_SMALL, &facet)
                            .child(format!("row {key}")),
                    )
                });
                return root.children(rows).into_any_element();
            }
            let cards: Vec<_> = (0..20).map(|i| {
                let card = card(1.0, cx);
                let animated = match self.case {
                    Case::Bare => false,
                    Case::OneLayer => i == 0,
                    Case::TwentyLayers => true,
                    Case::Flip200 => false,
                };
                if animated {
                    let wave = (phase + i as f32 * 0.3).sin();
                    layer(card)
                        .scale(1.0 + 0.03 * wave)
                        .opacity(0.6 + 0.3 * wave)
                        .into_any_element()
                } else {
                    card
                }
            }).collect();
            root.children(cards).into_any_element()
        }
    }

    let _platform = platform();
    for case in [Case::Bare, Case::OneLayer, Case::TwentyLayers, Case::Flip200] {
        let platform = gpui_platform::current_platform(true);
        let mut cx = HeadlessAppContext::with_platform(
            platform.text_system(),
            Arc::new(Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.update(|cx| gallery::bootstrap(Facet::default(), false, cx)).expect("bootstrap");
        let handle = cx
            .open_window(size(px(1440.0), px(900.0)), |window, cx| {
                window.set_scale_factor(2.0);
                cx.new(|_| Bench { case, frame: 0, flow: Flow::new("bench") })
            })
            .expect("window");
        let any: gpui::AnyWindowHandle = handle.into();
        let (mut cpu, mut total) = (Vec::new(), Vec::new());
        for frame in 0..200_u32 {
            let _ = cx.update_window(any, |view, window, cx| {
                if let Ok(bench) = view.downcast::<Bench>() {
                    bench.update(cx, |bench, cx| {
                        bench.frame = frame;
                        cx.notify();
                    });
                }
                let started = Instant::now();
                window.draw(cx).clear(cx);
                let drawn = started.elapsed();
                let _ = window.render_to_image();
                if frame >= 20 {
                    cpu.push(drawn);
                    total.push(started.elapsed());
                }
            });
            cx.advance_clock(std::time::Duration::from_millis(16));
            cx.run_until_parked();
        }
        let stat = |times: &mut Vec<std::time::Duration>| {
            times.sort_unstable();
            (times[times.len() / 2], times[times.len() * 95 / 100])
        };
        let ((cpu50, cpu95), (all50, all95)) = (stat(&mut cpu), stat(&mut total));
        let name = match case {
            Case::Bare => "20 cards, no layer        ",
            Case::OneLayer => "20 cards, 1 animated layer",
            Case::TwentyLayers => "20 cards, 20 animated layers",
            Case::Flip200 => "200-row FLIP reorder       ",
        };
        eprintln!(
            "{name}: cpu p50 {cpu50:?} p95 {cpu95:?} | cpu+gpu(readback) p50 {all50:?} p95 {all95:?}"
        );
    }
}
