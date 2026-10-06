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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Kind, Layout, Module, Node, Package, World};
    use std::sync::Arc;

    fn scene(names: &[&str]) -> Scene {
        let world = Arc::new(World::new(
            vec![Package { name: "owner".into(), version: "1".into(), yours: true, external: false, deps: vec![] }],
            vec![Module { pkg: 0, path: "module".into(), file: "module.rs".into() }],
            names.iter().map(|name| Node::new(Kind::Function, *name, 0, 0)).collect(), vec![],
        ).expect("valid geometry world"));
        Scene::new(world.clone(), Arc::new(Layout::compute(&world)))
    }

    #[test]
    fn identical_immutable_basis_preserves_the_actual_camera_exactly() {
        let old = scene(&["A", "B"]);
        let fresh = scene(&["A", "B"]);
        assert!(!Arc::ptr_eq(&old.layout, &fresh.layout));
        let camera = Camera::new(13.25, -28.75, 133.5);
        assert_eq!(Geometry::capture(&old, camera, Some(1)).restore(&fresh, None), Some(camera));
        assert_eq!(Geometry::capture(&old, camera, None).restore(&fresh, None), Some(camera));
    }

    #[test]
    fn changed_basis_keeps_visible_width_and_offset_from_remapped_anchor() {
        let old = scene(&["A", "B"]);
        let fresh = scene(&["B", "A", "C"]);
        assert_ne!(old.layout.key, fresh.layout.key);
        let (old_x, old_y) = anchor(&old, 1).expect("old B");
        let (new_x, new_y) = anchor(&fresh, 0).expect("new B");
        let camera = Camera::new(old_x + 12.25, old_y - 8.75, 71.5);
        let restored = Geometry::capture(&old, camera, Some(1)).restore(&fresh, Some(0)).expect("exact anchor survives");
        assert_eq!(restored.w, camera.w);
        assert!((restored.x - new_x - 12.25).abs() < 1e-10);
        assert!((restored.y - new_y + 8.75).abs() < 1e-10);
    }

    #[test]
    fn changed_basis_needs_both_old_and_new_exact_anchors() {
        let old = scene(&["A", "B"]);
        let fresh = scene(&["A", "C", "D"]);
        let camera = Camera::new(5.0, -3.0, 200.0);
        assert_eq!(Geometry::capture(&old, camera, None).restore(&fresh, Some(1)), None);
        assert_eq!(Geometry::capture(&old, camera, Some(1)).restore(&fresh, None), None);
        assert_eq!(Geometry::capture(&old, camera, Some(1)).restore(&fresh, Some(u32::MAX)), None);
    }
}
