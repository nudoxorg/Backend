//! The responsive matrix: every scene booted fresh at every width x text
//! scale x theme (x density x motion), settled, linted, and composed into
//! contact sheets (rows = text scale, columns = width) per theme, density
//! and motion setting.
//!
//! Each motion-on cell is also compared with a reduced-motion boot of the
//! same cell when the axes include both: a settled scene must equal a boot
//! that never animated (half-finished transitions show up here). Scenes
//! that lease the ambient pulse are exempt from that comparison and say so.

use super::lint::{self, Linted};
use super::{GalleryError, Scene, Shot, capture, compose};
use crate::Density;
use crate::probe::TrackKind;
use crate::tokens::Appearance;
use backend_gui_harness::Script;
use image::RgbaImage;

/// The matrix's axes.
#[derive(Clone, Debug, PartialEq)]
pub struct Axes {
    /// Logical widths.
    pub widths: Vec<u32>,
    /// Text scales in percent.
    pub text_scales: Vec<u16>,
    /// Appearances.
    pub themes: Vec<Appearance>,
    /// Densities.
    pub densities: Vec<Density>,
    /// Motion on (true) and/or reduced (false).
    pub motion: Vec<bool>,
}

impl Axes {
    /// The full product of the plan's axes.
    #[must_use]
    pub fn full() -> Self {
        Self {
            widths: vec![480, 640, 760, 900, 1100, 1280, 1440, 1920, 2560],
            text_scales: vec![85, 100, 125, 150, 200],
            themes: vec![Appearance::Abyss, Appearance::Glacier],
            densities: vec![Density::Comfortable, Density::Compact, Density::Dense],
            motion: vec![true, false],
        }
    }

    /// The review gate: every width x text scale x theme, comfortable,
    /// motion on and off.
    #[must_use]
    pub fn gate() -> Self {
        Self {
            densities: vec![Density::Comfortable],
            ..Self::full()
        }
    }

    /// Cells in order: theme, density, motion, text scale, width.
    #[must_use]
    pub fn cells(&self) -> Vec<Cell> {
        let mut cells = Vec::new();
        for &theme in &self.themes {
            for &density in &self.densities {
                for &motion in &self.motion {
                    for &text_scale in &self.text_scales {
                        for &width in &self.widths {
                            cells.push(Cell {
                                width,
                                text_scale,
                                theme,
                                density,
                                motion,
                            });
                        }
                    }
                }
            }
        }
        cells
    }
}

/// One matrix cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Logical width.
    pub width: u32,
    /// Text scale in percent.
    pub text_scale: u16,
    /// Appearance.
    pub theme: Appearance,
    /// Density.
    pub density: Density,
    /// Motion on.
    pub motion: bool,
}

impl Cell {
    /// A short label: `760w 150% abyss comfortable motion`.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}w {}% {} {}{}",
            self.width,
            self.text_scale,
            theme_name(self.theme),
            density_name(self.density),
            if self.motion { "" } else { " rm" }
        )
    }
}

/// `abyss` / `glacier`.
#[must_use]
pub const fn theme_name(theme: Appearance) -> &'static str {
    match theme {
        Appearance::Abyss => "abyss",
        Appearance::Glacier => "glacier",
    }
}

/// `comfortable` / `compact` / `dense`.
#[must_use]
pub const fn density_name(density: Density) -> &'static str {
    match density {
        Density::Comfortable => "comfortable",
        Density::Compact => "compact",
        Density::Dense => "dense",
    }
}

/// One cell's result.
#[derive(Clone, Debug)]
pub struct CellResult {
    /// The cell.
    pub cell: Cell,
    /// The settled pixels.
    pub image: RgbaImage,
    /// Its lints.
    pub linted: Linted,
    /// Whether the settled frame equals the reduced-motion boot (motion-on
    /// cells whose twin was captured; `None` otherwise).
    pub equals_reduced: Option<bool>,
    /// The scene leases the ambient pulse (exempt from the comparison).
    pub ambient: bool,
}

/// The settle time the matrix captures at.
pub const SETTLE_MS: u64 = 1_200;

/// Boots `scene` fresh in `cell` (no input) and captures it settled at
/// `scale`.
///
/// # Errors
/// A capture failure.
pub fn shoot(
    scene: &Scene,
    cell: Cell,
    scale: u8,
) -> Result<
    (
        RgbaImage,
        crate::probe::Ledger,
        backend_gui_harness::Viewport,
    ),
    GalleryError,
> {
    let mut shot = Shot::new(scene);
    shot.size = (cell.width, scene.size.1);
    shot.scale = scale;
    shot.appearance = cell.theme;
    shot.text_scale = f32::from(cell.text_scale) / 100.0;
    shot.density = cell.density;
    shot.reduced_motion = !cell.motion;
    shot.script = Some(Script::new());
    shot.probe = true;
    // Settled captures need no frame loop: motion is closed-form in time.
    shot.frame_ms = 0;
    shot.times = vec![if cell.motion { SETTLE_MS } else { 0 }];
    let frame = capture(scene, &shot)?
        .into_iter()
        .next()
        .ok_or_else(|| GalleryError(format!("{}: no frame", scene.id)))?;
    Ok((frame.image, frame.ledger, frame.drawn.viewport))
}

/// Runs the matrix for `scene`: every cell captured at `scale`, linted,
/// motion-on cells compared with their reduced twins.
///
/// # Errors
/// A capture failure.
pub fn run(
    scene: &Scene,
    axes: &Axes,
    scale: u8,
    progress: &mut dyn FnMut(&CellResult),
) -> Result<Vec<CellResult>, GalleryError> {
    let mut results: Vec<CellResult> = Vec::new();
    for cell in axes.cells() {
        let (image, ledger, viewport) = shoot(scene, cell, scale)?;
        let ambient = ledger
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Pulse);
        let linted = lint::lint(&image, &ledger, viewport);
        let mut result = CellResult {
            cell,
            image,
            linted,
            equals_reduced: None,
            ambient,
        };
        if !cell.motion {
            // Pair with the motion-on twin captured earlier.
            let on = Cell {
                motion: true,
                ..cell
            };
            if let Some(twin) = results.iter_mut().find(|other| other.cell == on) {
                let equal = twin.image.as_raw() == result.image.as_raw();
                twin.equals_reduced = Some(equal);
                result.equals_reduced = Some(equal);
            }
        }
        progress(&result);
        results.push(result);
    }
    Ok(results)
}

/// Contact sheets: one per (theme, density, motion), rows = text scale,
/// columns = width, tiles `tile_width` physical px wide.
///
/// # Errors
/// A label capture failure.
pub fn sheets(
    scene: &Scene,
    axes: &Axes,
    results: &[CellResult],
    tile_width: u32,
) -> Result<Vec<(String, RgbaImage)>, GalleryError> {
    let mut out = Vec::new();
    for &theme in &axes.themes {
        for &density in &axes.densities {
            for &motion in &axes.motion {
                let group = results
                    .iter()
                    .filter(|result| {
                        result.cell.theme == theme
                            && result.cell.density == density
                            && result.cell.motion == motion
                    })
                    .collect::<Vec<_>>();
                if group.is_empty() {
                    continue;
                }
                let lines = group
                    .iter()
                    .map(|result| {
                        let verdict = if result.linted.lints.is_empty()
                            && result.equals_reduced != Some(false)
                        {
                            String::new()
                        } else {
                            format!(
                                "  FAIL {}{}",
                                result
                                    .linted
                                    .lints
                                    .iter()
                                    .map(|lint| lint.rule.name())
                                    .collect::<Vec<_>>()
                                    .join(","),
                                if result.equals_reduced == Some(false) {
                                    " settled!=reduced"
                                } else {
                                    ""
                                }
                            )
                        };
                        format!(
                            "{}w {}%{verdict}",
                            result.cell.width, result.cell.text_scale
                        )
                    })
                    .collect::<Vec<_>>();
                let labels = compose::labels(&lines, (tile_width / 2).max(200), 2, theme)?;
                let tiles = group.iter().map(|result| &result.image).collect::<Vec<_>>();
                let sheet = compose::sheet(
                    &tiles,
                    &labels,
                    axes.widths.len(),
                    tile_width,
                    compose::background(theme),
                );
                out.push((
                    format!(
                        "{}-matrix-{}-{}{}",
                        scene.id,
                        theme_name(theme),
                        density_name(density),
                        if motion { "" } else { "-rm" }
                    ),
                    sheet,
                ));
            }
        }
    }
    Ok(out)
}

/// Every cell on one sheet: one row per (theme, density, motion, text
/// scale), one column per width, each tile labelled with its whole cell.
///
/// # Errors
/// A label capture failure.
pub fn one_sheet(
    scene: &Scene,
    axes: &Axes,
    results: &[CellResult],
    tile_width: u32,
) -> Result<(String, RgbaImage), GalleryError> {
    let lines = results
        .iter()
        .map(|result| {
            let failed = !result.linted.lints.is_empty() || result.equals_reduced == Some(false);
            format!(
                "{}{}",
                result.cell.label(),
                if failed { "  FAIL" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    let theme = axes.themes.first().copied().unwrap_or(Appearance::Abyss);
    let labels = compose::labels(&lines, (tile_width / 2).max(200), 2, theme)?;
    let tiles = results
        .iter()
        .map(|result| &result.image)
        .collect::<Vec<_>>();
    Ok((
        format!("{}-matrix", scene.id),
        compose::sheet(
            &tiles,
            &labels,
            axes.widths.len(),
            tile_width,
            compose::background(theme),
        ),
    ))
}
