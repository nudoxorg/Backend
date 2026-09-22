//! Layout transition plans for the canonical U3 timeline seam.
//!
//! This module describes *which named U3 beat* should drive a layout-state
//! change. It owns no clock and defines no independent easing vocabulary; the
//! plan carries the canonical `AnimationChannel`, `Beat`, and `Easing` types.

use super::resolver::ResponsiveLayout;
use crate::runtime::{AnimationChannel, Beat, Easing};

/// Kind of structural layout change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LayoutTransitionKind {
    /// Inputs changed without changing the shell stage.
    Stable,
    /// Compact navigation switch, driven by the U3 touch beat.
    CompactSwitch,
    /// Full pane restructure, driven by the U3 scene beat.
    FullRestructure,
}

/// A named U3 transition plan. `duration_ms` is the effective duration after
/// reduced-motion policy; `canonical_duration_ms` remains the evidence token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionPlan {
    /// Structural change kind.
    pub kind: LayoutTransitionKind,
    /// U3 semantic channel that owns the retargetable pane track.
    pub channel: AnimationChannel,
    /// U3 canonical beat for this structural change.
    pub beat: Beat,
    /// U3 easing paired with [`Self::beat`].
    pub easing: Easing,
    /// Canonical duration before reduced-motion policy.
    pub canonical_duration_ms: u16,
    /// Effective duration for this user preference.
    pub duration_ms: u16,
}

impl TransitionPlan {
    /// Creates a stable plan with no independent animation clock.
    #[must_use]
    pub const fn stable() -> Self {
        Self {
            kind: LayoutTransitionKind::Stable,
            channel: AnimationChannel::PaneCollapse,
            beat: Beat::Touch,
            easing: Easing::Snap,
            canonical_duration_ms: 0,
            duration_ms: 0,
        }
    }
}

/// Builds a deterministic plan from layout state changes.
#[must_use]
pub fn transition_plan(
    previous: Option<ResponsiveLayout>,
    next: ResponsiveLayout,
    reduced_motion: bool,
) -> TransitionPlan {
    let Some(previous) = previous else {
        return TransitionPlan::stable();
    };
    if previous == next {
        return TransitionPlan::stable();
    }
    let compact = previous.collapse.is_compact() || next.collapse.is_compact();
    let (kind, beat) = if compact {
        (LayoutTransitionKind::CompactSwitch, Beat::Touch)
    } else {
        (LayoutTransitionKind::FullRestructure, Beat::Scene)
    };
    TransitionPlan {
        kind,
        channel: AnimationChannel::PaneCollapse,
        beat,
        easing: beat.easing(),
        canonical_duration_ms: duration_ms(beat, false),
        duration_ms: duration_ms(beat, reduced_motion),
    }
}

fn duration_ms(beat: Beat, reduced_motion: bool) -> u16 {
    beat.duration(reduced_motion)
        .as_millis()
        .min(u128::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::{LayoutTransitionKind, transition_plan};
    use crate::core::layout::{LayoutInput, LogicalPx, PanelPreferences, TextScale, resolve};
    use crate::runtime::{AnimationChannel, Beat, Easing};

    fn layout(width: u32) -> crate::core::layout::ResponsiveLayout {
        resolve(LayoutInput::new(
            LogicalPx::new(width),
            LogicalPx::new(800),
            TextScale::DEFAULT,
            PanelPreferences {
                shelf_open: true,
                context_open: true,
            },
        ))
    }

    #[test]
    fn compact_switch_uses_exact_u3_touch_tokens() {
        let plan = transition_plan(Some(layout(380)), layout(239), false);
        assert_eq!(plan.kind, LayoutTransitionKind::CompactSwitch);
        assert_eq!(plan.canonical_duration_ms, 90);
        assert_eq!(plan.duration_ms, 90);
        assert_eq!(plan.channel, AnimationChannel::PaneCollapse);
        assert_eq!(plan.beat, Beat::Touch);
        assert_eq!(plan.easing, Easing::Snap);
    }

    #[test]
    fn full_restructure_uses_exact_u3_scene_tokens() {
        let plan = transition_plan(Some(layout(1280)), layout(618), false);
        assert_eq!(plan.kind, LayoutTransitionKind::FullRestructure);
        assert_eq!(plan.canonical_duration_ms, 620);
        assert_eq!(plan.duration_ms, 620);
        assert_eq!(plan.beat, Beat::Scene);
        assert_eq!(plan.easing, Easing::Glide);
    }

    #[test]
    fn reduced_motion_only_changes_effective_duration() {
        let plan = transition_plan(Some(layout(1280)), layout(618), true);
        assert_eq!(plan.canonical_duration_ms, 620);
        assert_eq!(plan.duration_ms, 0);
    }
}
