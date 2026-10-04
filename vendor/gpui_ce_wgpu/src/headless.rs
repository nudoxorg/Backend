//! NUDOX: [`gpui::PlatformHeadlessRenderer`] over the window renderer's
//! offscreen target, the wgpu counterpart of `gpui_macos`'s
//! `MetalHeadlessRenderer`.
//!
//! Headless captures then draw with the renderer a native window on this
//! platform draws with (the same pipelines, target format, opaque composite
//! and compositing capabilities) on an adapter chosen the way a window
//! chooses one, and read the frame back without a window.

use crate::{HeadlessAdapterPolicy, WgpuContext, WgpuRenderer};
use gpui::{DevicePixels, GpuSpecs, PlatformAtlas, Scene, Size};
use std::sync::Arc;

/// A windowless renderer with readback, for headless windows.
pub struct WgpuHeadlessRenderer {
    // Declared before `context`, so the renderer's GPU resources are released
    // before the device and adapter they were made on.
    renderer: WgpuRenderer,
    context: WgpuContext,
}

impl WgpuHeadlessRenderer {
    /// Creates a device on the first adapter `policy` admits, in a window's
    /// order, and an opaque offscreen renderer on it (a native window is
    /// opaque).
    ///
    /// # Errors
    /// No admitted adapter can create a device, or none can render to a
    /// window's texture formats.
    pub fn new(policy: HeadlessAdapterPolicy) -> anyhow::Result<Self> {
        let context = WgpuContext::new_headless(policy)?;
        let renderer = WgpuRenderer::new_offscreen(&context, false)?;
        Ok(Self { renderer, context })
    }

    /// The adapter this renderer draws on.
    #[must_use]
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.context.adapter.get_info()
    }
}

impl gpui::PlatformHeadlessRenderer for WgpuHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        let pixels = self.renderer.read_offscreen(scene, size)?;
        let (width, height) = (pixels.width, pixels.height);
        image::RgbaImage::from_raw(width, height, pixels.rgba).ok_or_else(|| {
            anyhow::anyhow!("a {width}x{height} readback does not hold {width}x{height} pixels")
        })
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        Ok(self.renderer.render_offscreen(scene, size)?)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }

    // `compositing` keeps the trait's default (no group opacity, no chamfer
    // shadows): the wgpu renderer is not patched for either, so a window on
    // it reports the same and headless frames fall back exactly as it does.

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        Some(self.renderer.gpu_specs())
    }
}
