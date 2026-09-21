//! Pixel comparison and diff artifact generation.

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Tolerance policy applied to one image comparison.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct DiffPolicy {
    /// Per-channel absolute difference ignored as renderer noise.
    pub channel_tolerance: u8,
    /// Maximum fraction of pixels allowed to differ.
    pub max_changed_fraction: f64,
    /// Maximum mean per-channel error over all pixels.
    pub max_mean_error: f64,
    /// Maximum channel error observed in one pixel.
    pub max_channel_error: u8,
}

impl Default for DiffPolicy {
    fn default() -> Self {
        Self {
            channel_tolerance: 0,
            max_changed_fraction: 0.0,
            max_mean_error: 0.0,
            max_channel_error: 0,
        }
    }
}

/// Bounding rectangle around changed pixels, inclusive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffBounds {
    /// Left coordinate.
    pub left: u32,
    /// Top coordinate.
    pub top: u32,
    /// Right coordinate.
    pub right: u32,
    /// Bottom coordinate.
    pub bottom: u32,
}

/// Machine-readable comparison result.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiffMetrics {
    /// Whether dimensions matched.
    pub same_dimensions: bool,
    /// Width compared, when dimensions matched.
    pub width: u32,
    /// Height compared, when dimensions matched.
    pub height: u32,
    /// Total pixels compared.
    pub pixel_count: u64,
    /// Pixels with any channel above tolerance.
    pub changed_pixels: u64,
    /// Changed pixel fraction.
    pub changed_fraction: f64,
    /// Sum of channel errors divided by all channels and pixels.
    pub mean_channel_error: f64,
    /// Largest channel error.
    pub max_channel_error: u8,
    /// Region containing all changed pixels.
    pub changed_bounds: Option<DiffBounds>,
    /// Whether the result satisfies the supplied policy.
    pub within_policy: bool,
}

impl DiffMetrics {
    /// Returns whether any pixel was changed.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed_pixels != 0
    }
}

/// Comparison or artifact errors.
#[derive(Debug, Error)]
pub enum DiffError {
    /// PNG dimensions differ and no meaningful pixel comparison is possible.
    #[error("image dimensions differ: {left:?} versus {right:?}")]
    Dimensions {
        /// Baseline image dimensions.
        left: (u32, u32),
        /// Actual image dimensions.
        right: (u32, u32),
    },
    /// Diff output could not be written.
    #[error("writing diff image failed: {0}")]
    Image(#[from] image::ImageError),
    /// Filesystem operation failed.
    #[error("diff artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Compares two images and applies a pixel/channel tolerance policy.
pub fn compare(
    baseline: &RgbaImage,
    actual: &RgbaImage,
    policy: DiffPolicy,
) -> Result<DiffMetrics, DiffError> {
    if baseline.dimensions() != actual.dimensions() {
        return Err(DiffError::Dimensions {
            left: baseline.dimensions(),
            right: actual.dimensions(),
        });
    }

    let (width, height) = baseline.dimensions();
    let pixel_count = u64::from(width) * u64::from(height);
    let mut changed_pixels = 0_u64;
    let mut sum = 0_u64;
    let mut max_channel_error = 0_u8;
    let mut bounds: Option<DiffBounds> = None;

    for (index, (left, right)) in baseline.pixels().zip(actual.pixels()).enumerate() {
        let mut changed = false;
        for channel in 0..4 {
            let error = left[channel].abs_diff(right[channel]);
            sum += u64::from(error);
            max_channel_error = max_channel_error.max(error);
            changed |= error > policy.channel_tolerance;
        }
        if changed {
            changed_pixels += 1;
            let index = index as u32;
            let x = index % width;
            let y = index / width;
            bounds = Some(match bounds {
                Some(current) => DiffBounds {
                    left: current.left.min(x),
                    top: current.top.min(y),
                    right: current.right.max(x),
                    bottom: current.bottom.max(y),
                },
                None => DiffBounds {
                    left: x,
                    top: y,
                    right: x,
                    bottom: y,
                },
            });
        }
    }

    let changed_fraction = if pixel_count == 0 {
        0.0
    } else {
        changed_pixels as f64 / pixel_count as f64
    };
    let mean_channel_error = if pixel_count == 0 {
        0.0
    } else {
        sum as f64 / (pixel_count as f64 * 4.0)
    };
    let within_policy = changed_fraction <= policy.max_changed_fraction
        && mean_channel_error <= policy.max_mean_error
        && max_channel_error <= policy.max_channel_error;

    Ok(DiffMetrics {
        same_dimensions: true,
        width,
        height,
        pixel_count,
        changed_pixels,
        changed_fraction,
        mean_channel_error,
        max_channel_error,
        changed_bounds: bounds,
        within_policy,
    })
}

/// Produces a visual diff: unchanged pixels are dimmed, changed pixels are
/// shown in red with the magnitude encoded in alpha.
#[must_use]
pub fn diff_image(baseline: &RgbaImage, actual: &RgbaImage, tolerance: u8) -> RgbaImage {
    let width = baseline.width().min(actual.width());
    let height = baseline.height().min(actual.height());
    let mut output = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let left = baseline.get_pixel(x, y);
            let right = actual.get_pixel(x, y);
            let magnitude = (0..4)
                .map(|channel| left[channel].abs_diff(right[channel]))
                .max()
                .unwrap_or(0);
            if magnitude > tolerance {
                output.put_pixel(x, y, Rgba([255, 0, 0, magnitude.max(64)]));
            } else {
                let value = (u16::from(right[0]) + u16::from(right[1]) + u16::from(right[2])) / 3;
                output.put_pixel(x, y, Rgba([value as u8, value as u8, value as u8, 255]));
            }
        }
    }
    output
}

/// Writes a diff PNG, creating its parent directory first.
pub fn write_diff(
    path: &Path,
    baseline: &RgbaImage,
    actual: &RgbaImage,
    tolerance: u8,
) -> Result<(), DiffError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    diff_image(baseline, actual, tolerance).save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_images_pass_the_default_policy() {
        let image = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let result = compare(&image, &image, DiffPolicy::default()).expect("dimensions match");
        assert!(result.within_policy);
        assert_eq!(result.changed_pixels, 0);
        assert_eq!(result.changed_bounds, None);
    }

    #[test]
    fn changed_bounds_and_policy_are_reported() {
        let baseline = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 255]));
        let mut actual = baseline.clone();
        actual.put_pixel(1, 0, Rgba([12, 0, 0, 255]));
        let result = compare(&baseline, &actual, DiffPolicy::default()).expect("dimensions match");
        assert!(!result.within_policy);
        assert_eq!(
            result.changed_bounds,
            Some(DiffBounds {
                left: 1,
                top: 0,
                right: 1,
                bottom: 0
            })
        );
        assert_eq!(result.max_channel_error, 12);
    }
}
