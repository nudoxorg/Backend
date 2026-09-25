//! Filmstrips, onion skins and contact sheets, composed from captured frames
//! with exact integer blits (no GPU resampling) so they stay reproducible.
//! Labels are set in Geist Mono by a headless GPUI capture of their own.

use super::{GalleryError, Scene, Shot, capture};
use crate::fonts::Typeset;
use crate::theme::ActiveFacet;
use crate::tokens::{Appearance, Face, TypeRole};
use gpui::{
    AnyView, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px,
};
use image::{Rgba, RgbaImage, imageops};

const LABEL: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

/// Logical height of one label cell.
const LABEL_HEIGHT: u32 = 24;
const LABEL_HEIGHT_PX: f32 = 24.0;
/// Gap between tiles, in physical px.
const GAP: u32 = 12;

fn ground(appearance: Appearance) -> Rgba<u8> {
    let tone = appearance.palette().g0.rgba();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let channel = |value: f32| (value * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba([
        channel(tone.red),
        channel(tone.green),
        channel(tone.blue),
        255,
    ])
}

struct Labels {
    lines: Vec<String>,
}

impl Render for Labels {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        div()
            .size_full()
            .bg(palette.g0)
            .flex()
            .flex_col()
            .children(self.lines.iter().map(|line| {
                div()
                    .h(px(LABEL_HEIGHT_PX))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .typeset_at(LABEL, 1.0)
                    .text_color(palette.ink2.hsla())
                    .child(line.clone())
            }))
    }
}

thread_local! {
    static LABEL_LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn build_labels(_window: &mut Window, cx: &mut gpui::App) -> AnyView {
    let lines = LABEL_LINES.with(|lines| lines.borrow().clone());
    cx.new(|_| Labels { lines }).into()
}

/// Renders one label image per line, `width` logical px wide, at `scale`.
///
/// # Errors
/// A capture failure.
pub fn labels(
    lines: &[String],
    width: u32,
    scale: u8,
    appearance: Appearance,
) -> Result<Vec<RgbaImage>, GalleryError> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    LABEL_LINES.with(|cell| *cell.borrow_mut() = lines.to_vec());
    let scene = Scene {
        id: "labels",
        title: "label strip",
        size: (
            width,
            LABEL_HEIGHT * u32::try_from(lines.len()).unwrap_or(u32::MAX),
        ),
        build: build_labels,
    };
    let mut shot = Shot::new(&scene);
    shot.scale = scale;
    shot.appearance = appearance;
    let frames = capture(&scene, &shot)?;
    let Some(frame) = frames.into_iter().next() else {
        return Err(GalleryError("label capture produced no frame".to_owned()));
    };
    let cell = LABEL_HEIGHT * u32::from(scale);
    Ok((0..u32::try_from(lines.len()).unwrap_or(0))
        .map(|index| {
            imageops::crop_imm(&frame.image, 0, index * cell, frame.image.width(), cell).to_image()
        })
        .collect())
}

/// Tiles `frames` in rows of `columns`, each with its label underneath.
#[must_use]
pub fn film(
    frames: &[&RgbaImage],
    labels: &[RgbaImage],
    columns: usize,
    background: Rgba<u8>,
) -> RgbaImage {
    grid(frames, labels, columns, background)
}

/// A contact sheet: the same grid, tiles scaled to `tile_width` physical px.
#[must_use]
pub fn sheet(
    tiles: &[&RgbaImage],
    labels: &[RgbaImage],
    columns: usize,
    tile_width: u32,
    background: Rgba<u8>,
) -> RgbaImage {
    let scaled = tiles
        .iter()
        .map(|tile| {
            if tile.width() <= tile_width {
                (*tile).clone()
            } else {
                let height = u32::try_from(
                    u64::from(tile.height()) * u64::from(tile_width)
                        / u64::from(tile.width().max(1)),
                )
                .unwrap_or(u32::MAX);
                imageops::resize(
                    *tile,
                    tile_width,
                    height.max(1),
                    imageops::FilterType::Triangle,
                )
            }
        })
        .collect::<Vec<_>>();
    let refs = scaled.iter().collect::<Vec<_>>();
    grid(&refs, labels, columns, background)
}

fn grid(
    tiles: &[&RgbaImage],
    labels: &[RgbaImage],
    columns: usize,
    background: Rgba<u8>,
) -> RgbaImage {
    let columns = columns.clamp(1, tiles.len().max(1));
    let cell_width = tiles.iter().map(|tile| tile.width()).max().unwrap_or(1);
    let label_height = labels.iter().map(RgbaImage::height).max().unwrap_or(0);
    let rows = tiles.len().div_ceil(columns);
    let row_heights = (0..rows)
        .map(|row| {
            tiles
                .iter()
                .skip(row * columns)
                .take(columns)
                .map(|tile| tile.height())
                .max()
                .unwrap_or(0)
                + label_height
        })
        .collect::<Vec<_>>();
    let columns_u32 = u32::try_from(columns).unwrap_or(1);
    let width = GAP + columns_u32 * (cell_width + GAP);
    let height = GAP + row_heights.iter().map(|height| height + GAP).sum::<u32>();
    let mut out = RgbaImage::from_pixel(width, height, background);
    let mut top = GAP;
    for (row, row_height) in row_heights.iter().enumerate() {
        for column in 0..columns {
            let index = row * columns + column;
            let Some(tile) = tiles.get(index) else {
                break;
            };
            let left = GAP + u32::try_from(column).unwrap_or(0) * (cell_width + GAP);
            imageops::replace(&mut out, *tile, i64::from(left), i64::from(top));
            if let Some(label) = labels.get(index) {
                let label =
                    imageops::crop_imm(label, 0, 0, label.width().min(cell_width), label.height())
                        .to_image();
                imageops::replace(
                    &mut out,
                    &label,
                    i64::from(left),
                    i64::from(top + tile.height()),
                );
            }
        }
        top += row_height + GAP;
    }
    out
}

/// An onion skin: the per-pixel median of all frames is the still ground;
/// each frame's difference from it is laid over at a strength that grows
/// with the frame's age (oldest faintest), strongest difference winning.
#[must_use]
pub fn onion(frames: &[&RgbaImage]) -> RgbaImage {
    let Some(first) = frames.first() else {
        return RgbaImage::new(1, 1);
    };
    let (width, height) = first.dimensions();
    let count = frames.len();
    let mut out = RgbaImage::new(width, height);
    #[allow(clippy::cast_precision_loss)]
    let weights = (0..count)
        .map(|index| {
            if count == 1 {
                1.0
            } else {
                0.3 + 0.7 * index as f32 / (count - 1) as f32
            }
        })
        .collect::<Vec<f32>>();
    let mut channel = vec![0_u8; count];
    for y in 0..height {
        for x in 0..width {
            let mut median = [0_u8; 4];
            for (c, slot) in median.iter_mut().enumerate() {
                for (index, frame) in frames.iter().enumerate() {
                    channel[index] = frame
                        .get_pixel(x.min(frame.width() - 1), y.min(frame.height() - 1))
                        .0[c];
                }
                channel.sort_unstable();
                *slot = channel[count / 2];
            }
            let mut best = (0.0_f32, [0.0_f32; 4]);
            for (index, frame) in frames.iter().enumerate() {
                let pixel = frame
                    .get_pixel(x.min(frame.width() - 1), y.min(frame.height() - 1))
                    .0;
                let delta = [0, 1, 2, 3].map(|c| f32::from(pixel[c]) - f32::from(median[c]));
                let strength = delta.iter().map(|d| d.abs()).sum::<f32>() * weights[index];
                if strength > best.0 {
                    best = (strength, delta.map(|d| d * weights[index]));
                }
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let blended = [0, 1, 2, 3]
                .map(|c| (f32::from(median[c]) + best.1[c]).round().clamp(0.0, 255.0) as u8);
            out.put_pixel(x, y, Rgba(blended));
        }
    }
    out
}

/// The capture ground colour for an appearance (sheet and film background).
#[must_use]
pub fn background(appearance: Appearance) -> Rgba<u8> {
    ground(appearance)
}
