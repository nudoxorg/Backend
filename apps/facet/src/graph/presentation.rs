//! Camera continuity across immutable scenes. Semantic identity is owned by
//! the embedding application; this packet holds geometry and no scene lease.

use super::{NodeId, scene::Scene};
use crate::motion::Camera;

/// Geometry sampled from the actual camera, never its pending flight target.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    basis: [u8; 32],
    camera: Camera,
    anchor: Option<(f64, f64)>,
}

impl Geometry {
    /// Uses the layout's cached canonical key; no world hashing on the UI.
    #[must_use]
    pub fn capture(scene: &Scene, camera: Camera, selection: Option<NodeId>) -> Self {
        Self { basis: scene.layout.key, camera, anchor: selection.and_then(|node| anchor(scene, node)) }
    }

    /// Equal layout inputs preserve exact placement. A changed layout needs
    /// a uniquely remapped anchor; translate its centre while retaining the
    /// visible world width and the selection's offset from that centre.
    #[must_use]
    pub fn restore(self, scene: &Scene, selection: Option<NodeId>) -> Option<Camera> {
        if self.basis == scene.layout.key { return Some(self.camera); }
        let (old_x, old_y) = self.anchor?;
        let (new_x, new_y) = anchor(scene, selection?)?;
        Some(Camera::new(self.camera.x + new_x - old_x, self.camera.y + new_y - old_y, self.camera.w))
    }
}

fn anchor(scene: &Scene, node: NodeId) -> Option<(f64, f64)> {
    Some((f64::from(*scene.layout.x.get(node as usize)?), f64::from(*scene.layout.y.get(node as usize)?)))
}
