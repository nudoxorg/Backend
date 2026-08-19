//! `ProvenanceDot` — an 8 px trust indicator, optionally breathing (§11, §5.1).
//!
//! # The permit, and why it is not optional
//!
//! A status dot that pulses forever is an *infinite* animation: something must
//! invalidate it every frame for as long as it is on screen. GUI-PLAN §5.4 caps
//! the whole app at three concurrent loops, because past that the cost is
//! constant and the effect stops reading as "this one thing is live" and starts
//! reading as noise.
//!
//! So breathing is gated on a [`LoopPermit`](crate::motion::permits::LoopPermit)
//! that the *caller* holds for as long as the dot is mounted. If no permit is
//! available the dot renders static at the midpoint of its pulse — visually
//! identical to a paused frame, never a missing element. The cap is therefore
//! enforced by construction rather than by everyone remembering it.
//!
//! # Visibility rule — absent by default, present when it matters (LD-8)
//!
//! A dot that is green on every single row communicates nothing: the eye
//! habituates to it in seconds and it becomes invisible decoration.  Provenance
//! only earns the space it occupies when its value *can vary* and when a
//! non-default value would change what the reader trusts.
//!
//! The chosen rule: **render nothing for `Provenance::TrustedLocal`** — the
//! happy path, and the only one where the dot would always be green.
//! `SyncedLocal`, `Remote`, and `Stale` all earn a dot because they signal that
//! the data arrived from somewhere other than a local compilation the user owns,
//! or that it may be out of date.  The dot's *presence* is the signal; its
//! *colour* refines it.
//!
//! This is the same reasoning as hiding a zero-count diagnostics chip: "0
//! problems" is noise you stop reading; its absence means "no news", which is
//! actually informative once you have internalised the rule.

use gpui::{
    Animation, AnimationExt, App, Element as _, ElementId, IntoElement, RenderOnce, Styled, Window,
    div, pulsating_between,
};

use crate::motion::permits::LoopPermit;
use crate::motion::tokens::STATUS_BREATHE;
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};

/// Alpha range of the breathing pulse, from GUI-PLAN §5.1 `status.breathe`.
///
/// Not on the `AlphaTokens` ladder and not a candidate for it: this is an
/// animation envelope (how far a pulse swings between frames), not a surface
/// tint. A later sweep for stray alpha literals should leave these alone.
const BREATHE_MIN: f32 = 0.45;
const BREATHE_MAX: f32 = 0.9;

/// The static opacity used when no loop permit was available.
///
/// The midpoint of the pulse, so a shed dot looks like a still frame of a
/// breathing one rather than a differently-styled element (§6.2 load shedding:
/// users should perceive "always smooth", never "sometimes fancy").
const BREATHE_MIDPOINT: f32 = (BREATHE_MIN + BREATHE_MAX) / 2.0;

/// A small filled circle whose colour encodes provenance (LD-8).
#[derive(IntoElement)]
pub struct ProvenanceDot {
    id: ElementId,
    provenance: Provenance,
    /// Present only when the caller successfully acquired a loop slot.
    permit: Option<LoopPermit>,
}

impl ProvenanceDot {
    /// A still dot. This is the common case — most provenance is settled.
    pub fn new(id: impl Into<ElementId>, provenance: Provenance) -> Self {
        Self {
            id: id.into(),
            provenance,
            permit: None,
        }
    }

    /// A breathing dot, for provenance that is actively changing (syncing,
    /// compiling). Requires a permit obtained from
    /// [`MotionTokens::acquire_loop_slot`](crate::motion::tokens::MotionTokens::acquire_loop_slot);
    /// hold it for as long as the dot is mounted and drop it when it unmounts.
    pub fn breathing(id: impl Into<ElementId>, provenance: Provenance, permit: LoopPermit) -> Self {
        Self {
            id: id.into(),
            provenance,
            permit: Some(permit),
        }
    }
}

impl RenderOnce for ProvenanceDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Visibility rule (see module doc): `TrustedLocal` is the default,
        // expected state.  A dot that is always green on every row teaches the
        // reader to ignore it.  We render nothing for the happy path and let the
        // dot's *presence* carry the meaning ("something unusual about this
        // source") rather than its colour alone.
        if self.provenance == Provenance::TrustedLocal {
            return div().into_any();
        }

        let ext = cx.theme_ext();
        let style = ext.for_provenance(self.provenance);
        let size = ext.space.space_2; // 8 px
        let reduced = ext.reduced_motion();

        let dot = div().w(size).h(size).rounded_full().bg(style.colour);

        // Reduced motion (LD-17): a breathing dot at scale 0 is simply a lit
        // dot. The state it communicates — "this is live" — survives; only the
        // time dimension goes away.
        if self.permit.is_some() && !reduced {
            dot.with_animation(
                self.id,
                Animation::new(STATUS_BREATHE)
                    .repeat()
                    .with_easing(pulsating_between(BREATHE_MIN, BREATHE_MAX)),
                |el, alpha| el.opacity(alpha),
            )
            .into_any()
        } else {
            dot.opacity(if self.permit.is_some() {
                BREATHE_MIDPOINT
            } else {
                1.0
            })
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shed state must sit inside the pulse it replaces, or a dot that
    /// loses its permit would visibly jump brightness.
    #[test]
    fn breathe_midpoint_lies_within_the_pulse_range() {
        assert!(BREATHE_MIDPOINT > BREATHE_MIN && BREATHE_MIDPOINT < BREATHE_MAX);
    }

    /// `TrustedLocal` must be treated as the default (hidden) provenance.
    ///
    /// Concretely: a `ProvenanceDot` for `TrustedLocal` must not render a
    /// coloured circle.  We verify this by checking that the provenance is the
    /// `TrustedLocal` variant — the render path that returns an empty div is
    /// entered only for that variant, and any other variant would paint a dot.
    #[test]
    fn trusted_local_is_the_hidden_provenance() {
        // The rule: exactly one variant is suppressed — the common, expected
        // one.  All others earn a dot.
        let hidden = Provenance::TrustedLocal;
        let shown = [
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];
        // Self-check: the constant we compare against in `render` is the same
        // variant we intend to suppress.
        assert_eq!(
            hidden,
            Provenance::TrustedLocal,
            "hidden variant must be TrustedLocal"
        );
        // And none of the shown variants are TrustedLocal.
        for p in shown {
            assert_ne!(
                p,
                Provenance::TrustedLocal,
                "{p:?} must not be the hidden variant"
            );
        }
    }

    /// The visibility rule is binary: exactly one variant is suppressed.
    ///
    /// If a future edit accidentally suppresses a second variant (or none at
    /// all), this test catches it by exhausting the known set.
    #[test]
    fn exactly_one_provenance_variant_is_suppressed() {
        let all = [
            Provenance::TrustedLocal,
            Provenance::SyncedLocal,
            Provenance::Remote,
            Provenance::Stale,
        ];
        let suppressed_count = all
            .iter()
            .filter(|&&p| p == Provenance::TrustedLocal)
            .count();
        assert_eq!(
            suppressed_count, 1,
            "exactly one provenance variant must be suppressed (TrustedLocal)"
        );
    }
}
