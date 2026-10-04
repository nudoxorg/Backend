//! NUDOX: an offscreen target for [`WgpuRenderer`].
//!
//! A window's renderer acquires a swapchain texture, encodes the scene into
//! it and presents it. An offscreen renderer runs the same encoding
//! ([`WgpuRenderer::encode_and_submit`]) with the same pipelines, format and
//! composite mode into a texture of its own, and can copy that texture back
//! to the CPU. Nothing here needs a window, a surface or an interactive
//! session.

use super::{FrameTarget, SceneSubmission, TargetModes, WgpuRenderer};
use crate::{WgpuAtlas, WgpuContext, WgpuSurfaceConfig};
use gpui::{DevicePixels, Scene, Size};
use std::sync::{Arc, PoisonError, mpsc};
use std::time::Duration;

/// How long a readback waits for the GPU to finish a frame before it fails.
const READBACK_DEADLINE: Duration = Duration::from_secs(60);

/// The formats an offscreen target may use, in the order a window's surface
/// prefers them (`new_internal`), so the pipelines match the window's.
const TARGET_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Rgba8Unorm,
];

/// The usages an offscreen target needs: drawn into, then copied out.
const TARGET_USAGES: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::COPY_SRC);

/// Bytes in one pixel of either target format.
const BYTES_PER_PIXEL: u32 = 4;

/// The order of the colour channels in a target's texels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelOrder {
    /// `Bgra8Unorm`.
    Bgra,
    /// `Rgba8Unorm`.
    Rgba,
}

impl ChannelOrder {
    fn of(format: wgpu::TextureFormat) -> Option<Self> {
        match format {
            wgpu::TextureFormat::Bgra8Unorm => Some(Self::Bgra),
            wgpu::TextureFormat::Rgba8Unorm => Some(Self::Rgba),
            _ => None,
        }
    }
}

/// What a compositor does with a frame's alpha channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Composite {
    /// An opaque window: the compositor ignores alpha, so every pixel the
    /// user sees is opaque, whatever the blend left in the channel.
    Opaque,
    /// A transparent window: alpha is kept.
    Alpha,
}

impl Composite {
    fn of(alpha_mode: wgpu::CompositeAlphaMode) -> Self {
        match alpha_mode {
            wgpu::CompositeAlphaMode::Opaque => Self::Opaque,
            _ => Self::Alpha,
        }
    }
}

/// The geometry of a readback buffer: whole texel rows, each padded to the
/// copy alignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowLayout {
    width: u32,
    height: u32,
    padded_row_bytes: u32,
}

impl RowLayout {
    /// The layout for a `width` x `height` copy, or `None` if its byte count
    /// overflows.
    fn new(width: u32, height: u32) -> Option<Self> {
        let row_bytes = width.checked_mul(BYTES_PER_PIXEL)?;
        let padded_row_bytes =
            row_bytes.checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)?;
        padded_row_bytes.checked_mul(height)?;
        Some(Self {
            width,
            height,
            padded_row_bytes,
        })
    }

    fn row_bytes(self) -> usize {
        // Lossless: `new` checked `width * 4` fits in a u32, and usize is at
        // least 32 bits on every target wgpu supports.
        (self.width * BYTES_PER_PIXEL) as usize
    }

    fn buffer_bytes(self) -> u64 {
        u64::from(self.padded_row_bytes) * u64::from(self.height)
    }
}

/// Packs a readback's padded rows into tight RGBA8 rows, top to bottom.
///
/// An opaque composite forces alpha to 255, as the window's compositor does
/// when it shows the frame; the colour channels are left exactly as drawn.
fn unpack_rows(
    padded: &[u8],
    layout: RowLayout,
    order: ChannelOrder,
    composite: Composite,
) -> Vec<u8> {
    let row_bytes = layout.row_bytes();
    let mut rgba = Vec::with_capacity(row_bytes * layout.height as usize);
    for row in padded
        .chunks(layout.padded_row_bytes as usize)
        .take(layout.height as usize)
    {
        for texel in row[..row_bytes].chunks_exact(BYTES_PER_PIXEL as usize) {
            let [first, green, third, alpha] = [texel[0], texel[1], texel[2], texel[3]];
            let (red, blue) = match order {
                ChannelOrder::Bgra => (third, first),
                ChannelOrder::Rgba => (first, third),
            };
            let alpha = match composite {
                Composite::Opaque => u8::MAX,
                Composite::Alpha => alpha,
            };
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    rgba
}

/// The texture an offscreen renderer draws into, and the buffer its pixels
/// are copied to. Both are sized to one viewport and replaced when it changes.
pub(super) struct OffscreenTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    layout: RowLayout,
    format: wgpu::TextureFormat,
}

impl OffscreenTexture {
    fn allocate(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        layout: RowLayout,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen_target"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: TARGET_USAGES,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen_readback"),
            size: layout.buffer_bytes(),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            texture,
            view,
            readback,
            layout,
            format: config.format,
        }
    }

    /// Whether this texture is the one `config` describes.
    pub(super) fn matches(&self, config: &wgpu::SurfaceConfiguration) -> bool {
        self.layout.width == config.width
            && self.layout.height == config.height
            && self.format == config.format
    }
}

/// Why an offscreen frame could not be drawn or read back.
#[derive(Debug)]
pub enum OffscreenError {
    /// The requested viewport has a side of zero or less.
    EmptySize(Size<DevicePixels>),
    /// A side is larger than the device's largest texture, or its readback
    /// does not fit in memory.
    TooLarge {
        /// The requested viewport.
        size: Size<DevicePixels>,
        /// The device's largest texture side.
        max_side: u32,
    },
    /// The renderer draws to a window's surface, not an offscreen target.
    NotOffscreen,
    /// The renderer's GPU resources were released ([`WgpuRenderer::destroy`]).
    Released,
    /// The GPU device was lost; an offscreen renderer does not recover.
    DeviceLost,
    /// The scene needs more instance memory than the device's largest buffer.
    InstanceBufferExhausted,
    /// The GPU reported an error while drawing or copying the frame.
    Gpu(String),
    /// The GPU did not finish the frame within the readback deadline.
    Poll(wgpu::PollError),
    /// The readback buffer could not be mapped.
    Map(String),
}

impl std::fmt::Display for OffscreenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySize(size) => write!(formatter, "offscreen viewport {size:?} is empty"),
            Self::TooLarge { size, max_side } => write!(
                formatter,
                "offscreen viewport {size:?} exceeds the device's {max_side}-pixel texture limit"
            ),
            Self::NotOffscreen => formatter.write_str("this renderer draws to a window surface"),
            Self::Released => formatter.write_str("the renderer's GPU resources were released"),
            Self::DeviceLost => formatter.write_str("the GPU device was lost"),
            Self::InstanceBufferExhausted => {
                formatter.write_str("the scene exceeds the device's largest instance buffer")
            }
            Self::Gpu(error) => write!(formatter, "GPU error: {error}"),
            Self::Poll(error) => write!(formatter, "waiting for the GPU: {error}"),
            Self::Map(error) => write!(formatter, "mapping the readback buffer: {error}"),
        }
    }
}

impl std::error::Error for OffscreenError {}

/// One frame read back from an offscreen target: tightly packed RGBA8 rows,
/// top to bottom, as the window's compositor would show them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OffscreenPixels {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl WgpuRenderer {
    /// A renderer whose frames go to an offscreen texture instead of a
    /// window. It draws with the pipelines, target format and composite mode
    /// a window on `context`'s adapter would use (an opaque window unless
    /// `transparent`).
    ///
    /// # Errors
    /// The adapter cannot render to and copy from either window format.
    pub fn new_offscreen(context: &WgpuContext, transparent: bool) -> anyhow::Result<Self> {
        let format = TARGET_FORMATS
            .into_iter()
            .find(|format| {
                context
                    .adapter
                    .get_texture_format_features(*format)
                    .allowed_usages
                    .contains(TARGET_USAGES)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "adapter {:?} can render to and copy from none of {TARGET_FORMATS:?}",
                    context.adapter.get_info().name
                )
            })?;
        let atlas = Arc::new(WgpuAtlas::from_context(context));
        Self::new_with_target(
            None,
            context,
            FrameTarget::Offscreen(None),
            TargetModes {
                format,
                // Nothing composites an offscreen texture, so both modes are
                // just the blends a window of each kind uses.
                transparent_alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                opaque_alpha_mode: wgpu::CompositeAlphaMode::Opaque,
                present_mode: wgpu::PresentMode::Fifo,
            },
            WgpuSurfaceConfig {
                size: Size {
                    width: DevicePixels(1),
                    height: DevicePixels(1),
                },
                transparent,
                preferred_present_mode: None,
            },
            None,
            None,
            atlas,
        )
    }

    /// Draws `scene` at `size` into the offscreen texture and submits it,
    /// without waiting for the GPU or reading pixels back: the headless
    /// analogue of presenting a frame.
    ///
    /// # Errors
    /// See [`OffscreenError`]; GPU validation errors in this frame are
    /// returned, not logged.
    pub fn render_offscreen(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<(), OffscreenError> {
        self.with_error_scopes(|renderer| renderer.draw_offscreen(scene, size))
    }

    /// Draws `scene` at `size` into the offscreen texture and reads the
    /// frame back, blocking until the GPU has finished it.
    ///
    /// # Errors
    /// See [`OffscreenError`].
    pub fn read_offscreen(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<OffscreenPixels, OffscreenError> {
        let (submission, mapped) = self.with_error_scopes(|renderer| {
            renderer.draw_offscreen(scene, size)?;
            renderer.copy_to_readback()
        })?;
        let resources = self.resources.as_ref().ok_or(OffscreenError::Released)?;
        resources
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(READBACK_DEADLINE),
            })
            .map_err(OffscreenError::Poll)?;
        match mapped.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(OffscreenError::Map(error.to_string())),
            Err(_) => {
                return Err(OffscreenError::Map(
                    "the GPU finished the frame without mapping its readback".to_owned(),
                ));
            }
        }
        let FrameTarget::Offscreen(Some(target)) = &resources.target else {
            return Err(OffscreenError::NotOffscreen);
        };
        let order = ChannelOrder::of(target.format).ok_or(OffscreenError::NotOffscreen)?;
        let composite = Composite::of(self.surface_config.alpha_mode);
        let rgba = {
            let padded = target.readback.slice(..).get_mapped_range();
            unpack_rows(&padded, target.layout, order, composite)
        };
        target.readback.unmap();
        Ok(OffscreenPixels {
            width: target.layout.width,
            height: target.layout.height,
            rgba,
        })
    }

    /// Runs `body` inside validation, out-of-memory and internal error
    /// scopes, so a GPU error in this frame fails it instead of reaching the
    /// device's uncaptured-error log.
    fn with_error_scopes<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, OffscreenError>,
    ) -> Result<T, OffscreenError> {
        let device = Arc::clone(
            &self
                .resources
                .as_ref()
                .ok_or(OffscreenError::Released)?
                .device,
        );
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = body(self);
        // Scopes pop innermost first; the first error found is the frame's.
        let errors = [validation, memory, internal].map(|scope| gpui::block_on(scope.pop()));
        if let Some(error) = errors.into_iter().flatten().next() {
            return Err(OffscreenError::Gpu(error.to_string()));
        }
        result
    }

    /// Everything a frame needs before and including its submission.
    fn draw_offscreen(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<(), OffscreenError> {
        let (Ok(width), Ok(height)) = (u32::try_from(size.width.0), u32::try_from(size.height.0))
        else {
            return Err(OffscreenError::EmptySize(size));
        };
        if width == 0 || height == 0 {
            return Err(OffscreenError::EmptySize(size));
        }
        let too_large = OffscreenError::TooLarge {
            size,
            max_side: self.max_texture_size,
        };
        if width > self.max_texture_size || height > self.max_texture_size {
            return Err(too_large);
        }
        let layout = RowLayout::new(width, height).ok_or(too_large)?;
        if self.device_lost() {
            return Err(OffscreenError::DeviceLost);
        }
        // An error raised outside any frame's scopes (an atlas texture made
        // while the scene was painted) belongs to this frame.
        let earlier = self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(error) = earlier {
            return Err(OffscreenError::Gpu(error));
        }

        self.update_drawable_size(size);
        self.atlas.before_frame();
        let config = self.surface_config.clone();
        let resources = self.resources.as_mut().ok_or(OffscreenError::Released)?;
        let FrameTarget::Offscreen(slot) = &mut resources.target else {
            return Err(OffscreenError::NotOffscreen);
        };
        let view = slot
            .get_or_insert_with(|| OffscreenTexture::allocate(&resources.device, &config, layout))
            .view
            .clone();
        match self.encode_and_submit(scene, &view) {
            SceneSubmission::Submitted => Ok(()),
            SceneSubmission::InstanceBufferExhausted => {
                Err(OffscreenError::InstanceBufferExhausted)
            }
        }
    }

    /// Copies the drawn frame into the readback buffer, submits the copy and
    /// requests the buffer's mapping, returning the copy's submission and
    /// where the mapping's outcome arrives.
    fn copy_to_readback(
        &mut self,
    ) -> Result<
        (
            wgpu::SubmissionIndex,
            mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
        ),
        OffscreenError,
    > {
        let resources = self.resources.as_ref().ok_or(OffscreenError::Released)?;
        let FrameTarget::Offscreen(Some(target)) = &resources.target else {
            return Err(OffscreenError::NotOffscreen);
        };
        let mut encoder =
            resources
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("offscreen_readback"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.layout.padded_row_bytes),
                    rows_per_image: Some(target.layout.height),
                },
            },
            wgpu::Extent3d {
                width: target.layout.width,
                height: target.layout.height,
                depth_or_array_layers: 1,
            },
        );
        let submission = resources.queue.submit(std::iter::once(encoder.finish()));
        let (sender, receiver) = mpsc::channel();
        target
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |outcome| {
                // The receiver outlives the poll that runs this callback; a
                // send can only fail if the readback was abandoned.
                let _ = sender.send(outcome);
            });
        Ok((submission, receiver))
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelOrder, Composite, RowLayout, unpack_rows};

    /// Two rows of a 3-pixel-wide BGRA readback, padded to 256 bytes each,
    /// with a distinct value in every byte position that matters.
    fn padded_bgra() -> (Vec<u8>, RowLayout) {
        let layout = RowLayout::new(3, 2).expect("small layout");
        assert_eq!(layout.padded_row_bytes, 256);
        let mut padded = vec![0xEE; 512];
        for (row, offset) in [(0_u8, 0_usize), (1, 256)] {
            for pixel in 0..3_u8 {
                let at = offset + usize::from(pixel) * 4;
                padded[at..at + 4].copy_from_slice(&[
                    10 + row * 100 + pixel, // blue
                    20 + row * 100 + pixel, // green
                    30 + row * 100 + pixel, // red
                    40 + pixel,             // alpha
                ]);
            }
        }
        (padded, layout)
    }

    #[test]
    fn bgra_rows_unpack_to_tight_rgba_and_padding_is_dropped() {
        let (padded, layout) = padded_bgra();
        let rgba = unpack_rows(&padded, layout, ChannelOrder::Bgra, Composite::Alpha);
        assert_eq!(rgba.len(), 3 * 2 * 4);
        assert_eq!(&rgba[..4], &[30, 20, 10, 40]);
        assert_eq!(&rgba[8..12], &[32, 22, 12, 42]);
        assert_eq!(&rgba[12..16], &[130, 120, 110, 40]);
        assert!(!rgba.contains(&0xEE), "row padding leaked into the image");
    }

    #[test]
    fn an_opaque_composite_shows_every_pixel_opaque_without_touching_colour() {
        let (padded, layout) = padded_bgra();
        let rgba = unpack_rows(&padded, layout, ChannelOrder::Bgra, Composite::Opaque);
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == u8::MAX));
        assert_eq!(&rgba[12..15], &[130, 120, 110]);
    }

    #[test]
    fn rgba_rows_keep_their_channel_order() {
        let (padded, layout) = padded_bgra();
        let rgba = unpack_rows(&padded, layout, ChannelOrder::Rgba, Composite::Alpha);
        assert_eq!(&rgba[..4], &[10, 20, 30, 40]);
    }

    #[test]
    fn a_row_already_aligned_has_no_padding_and_huge_layouts_are_refused() {
        let layout = RowLayout::new(64, 1).expect("one aligned row");
        assert_eq!(layout.padded_row_bytes, 256);
        assert_eq!(
            RowLayout::new(65, 1)
                .expect("one padded row")
                .padded_row_bytes,
            512
        );
        assert!(RowLayout::new(u32::MAX, 1).is_none());
        assert!(RowLayout::new(1 << 20, 1 << 20).is_none());
    }
}
