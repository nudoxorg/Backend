//! Held-modifier reveal: holding ⌘ alone raises key caps on everything that
//! has a key, holding ⌥ alone x-rays every visible datum one rung up.
//!
//! Each waits [`HOLD`] before it shows, so a chord (⌘C, ⌥←) never flashes
//! caps: any key pressed during the hold disarms it. Releasing the modifier,
//! adding another one, or the window losing focus ends it at once.
//!
//! This is a pure state machine; the shell owns the timer (one executor
//! timer per arm, identified by a generation so a stale timer is a no-op).

use facet::Reveal;
use gpui::Modifiers;
use std::time::Duration;

/// How long a lone modifier must be held before its reveal shows.
pub(crate) const HOLD: Duration = Duration::from_millis(260);

/// Which reveal a pending hold arms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Held {
    /// ⌘: key caps.
    Keys,
    /// ⌥: x-ray.
    Xray,
}

/// What a transition asks the shell to do.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Change {
    /// Start a hold timer that calls [`RevealHold::fire`] with this generation.
    pub arm: Option<u64>,
    /// The reveal changed to this value: apply it.
    pub reveal: Option<Reveal>,
}

/// The hold state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RevealHold {
    pending: Option<(Held, u64)>,
    reveal: Reveal,
    generation: u64,
}

impl RevealHold {
    /// The reveal currently shown.
    pub(crate) const fn reveal(&self) -> Reveal {
        self.reveal
    }

    /// Whether a hold is waiting for its timer.
    pub(crate) const fn is_armed(&self) -> bool {
        self.pending.is_some()
    }

    /// The modifier state changed.
    pub(crate) fn modifiers(&mut self, modifiers: Modifiers) -> Change {
        let others = modifiers.shift || modifiers.control || modifiers.function;
        let lone_cmd = modifiers.platform && !modifiers.alt && !others;
        let lone_alt = modifiers.alt && !modifiers.platform && !others;
        let before = self.reveal;
        // A reveal lasts exactly as long as its own modifier is held alone.
        self.reveal.keys &= lone_cmd;
        self.reveal.xray &= lone_alt;
        let wanted = if lone_cmd && !self.reveal.keys {
            Some(Held::Keys)
        } else if lone_alt && !self.reveal.xray {
            Some(Held::Xray)
        } else {
            None
        };
        let mut change = Change::default();
        match (wanted, self.pending) {
            (Some(held), Some((pending, _))) if held == pending => {}
            (Some(held), _) => {
                self.generation = self.generation.wrapping_add(1);
                self.pending = Some((held, self.generation));
                change.arm = Some(self.generation);
            }
            (None, _) => self.pending = None,
        }
        if self.reveal != before {
            change.reveal = Some(self.reveal);
        }
        change
    }

    /// A key went down: a chord, not a hold. Disarms a pending hold; a
    /// reveal already showing stays (its caps name the key being pressed).
    pub(crate) fn key_down(&mut self) {
        self.pending = None;
    }

    /// A hold timer fired.
    pub(crate) fn fire(&mut self, generation: u64) -> Change {
        let Some((held, pending)) = self.pending else {
            return Change::default();
        };
        if pending != generation {
            return Change::default();
        }
        self.pending = None;
        let before = self.reveal;
        match held {
            Held::Keys => self.reveal.keys = true,
            Held::Xray => self.reveal.xray = true,
        }
        Change {
            arm: None,
            reveal: (self.reveal != before).then_some(self.reveal),
        }
    }

    /// The window lost focus: everything ends.
    pub(crate) fn clear(&mut self) -> Change {
        self.pending = None;
        let before = self.reveal;
        self.reveal = Reveal::default();
        Change {
            arm: None,
            reveal: (self.reveal != before).then_some(self.reveal),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> Modifiers {
        Modifiers {
            platform: true,
            ..Modifiers::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn a_held_command_shows_keys_only_after_the_hold_and_hides_on_release() {
        let mut hold = RevealHold::default();
        let armed = hold.modifiers(cmd());
        let generation = armed.arm.expect("armed");
        assert_eq!(armed.reveal, None, "nothing shows before the hold");
        let shown = hold.fire(generation);
        assert_eq!(shown.reveal, Some(Reveal { keys: true, xray: false }));
        let released = hold.modifiers(Modifiers::default());
        assert_eq!(released.reveal, Some(Reveal::default()));
    }

    #[test]
    fn a_chord_never_flashes_caps() {
        let mut hold = RevealHold::default();
        let generation = hold.modifiers(cmd()).arm.expect("armed");
        hold.key_down(); // ⌘C
        assert_eq!(hold.fire(generation), Change::default());
        assert_eq!(hold.reveal(), Reveal::default());
    }

    #[test]
    fn a_stale_timer_is_ignored_and_rearming_does_not_restart_a_live_hold() {
        let mut hold = RevealHold::default();
        let first = hold.modifiers(cmd()).arm.expect("armed");
        // Releasing and pressing again arms a new generation.
        let _ = hold.modifiers(Modifiers::default());
        let second = hold.modifiers(cmd()).arm.expect("re-armed");
        assert_ne!(first, second);
        assert_eq!(hold.fire(first), Change::default(), "the old timer is stale");
        // A repeated identical modifier event keeps the pending hold.
        assert_eq!(hold.modifiers(cmd()).arm, None);
        assert!(hold.fire(second).reveal.is_some());
    }

    #[test]
    fn option_xrays_and_adding_a_modifier_ends_a_reveal() {
        let mut hold = RevealHold::default();
        let generation = hold.modifiers(alt()).arm.expect("armed");
        assert_eq!(hold.fire(generation).reveal, Some(Reveal { keys: false, xray: true }));
        let both = hold.modifiers(Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::default()
        });
        assert_eq!(both.reveal, Some(Reveal::default()), "⌥⇧ is a chord, not x-ray");
    }

    #[test]
    fn losing_focus_clears_everything() {
        let mut hold = RevealHold::default();
        let generation = hold.modifiers(cmd()).arm.expect("armed");
        let _ = hold.fire(generation);
        let cleared = hold.clear();
        assert_eq!(cleared.reveal, Some(Reveal::default()));
        assert!(!hold.is_armed());
    }
}
