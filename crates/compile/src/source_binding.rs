//! File-local binding identity and independently checked owner-extent evidence.

/// Exact captured-name anchor, scoped by the containing source record/version.
/// This is a half-open UTF-8 byte range, never a line/name display coordinate
/// or UTF-16 SDK position. The owner body's extent is deliberately excluded:
/// editing a parent's tail must not change its children's parent references.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceBinding {
    start: u32,
    end: u32,
}

impl SourceBinding {
    /// Admits a nonempty captured-name range.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if start < end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Returns the name's half-open file byte range.
    #[must_use]
    pub const fn name(self) -> (u32, u32) {
        (self.start, self.end)
    }

    /// Relocates the anchor through a checked immutable page byte base.
    #[must_use]
    pub fn shifted(self, delta: i64) -> Option<Self> {
        let shift = |offset| u32::try_from(i64::from(offset).checked_add(delta)?).ok();
        Self::new(shift(self.start)?, shift(self.end)?)
    }
}

/// One binding plus the declaration extent that the parse actually observed.
/// The constructor proves that the name is inside its owner. Only `binding`
/// addresses a parent; `owner` remains local evidence on this declaration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceBindingCapture {
    binding: SourceBinding,
    owner_start: u32,
    owner_end: u32,
}

impl SourceBindingCapture {
    /// Admits an owner extent containing a nonempty captured-name anchor.
    #[must_use]
    pub const fn new(
        owner_start: u32,
        owner_end: u32,
        name_start: u32,
        name_end: u32,
    ) -> Option<Self> {
        if owner_start <= name_start && name_end <= owner_end {
            if let Some(binding) = SourceBinding::new(name_start, name_end) {
                return Some(Self {
                    binding,
                    owner_start,
                    owner_end,
                });
            }
        }
        None
    }

    /// Minimal identity, independent of the owner body's length.
    #[must_use]
    pub const fn binding(self) -> SourceBinding {
        self.binding
    }

    /// Observed owner extent, kept separate from identity.
    #[must_use]
    pub const fn owner(self) -> (u32, u32) {
        (self.owner_start, self.owner_end)
    }

    /// Captured-name range in UTF-8 bytes.
    #[must_use]
    pub const fn name(self) -> (u32, u32) {
        self.binding.name()
    }

    /// Checks lexical owner containment when both declarations are available.
    #[must_use]
    pub const fn strictly_contains(self, child: Self) -> bool {
        self.owner_start <= child.owner_start
            && child.owner_end <= self.owner_end
            && (self.owner_start != child.owner_start || self.owner_end != child.owner_end)
    }

    /// Relocates all retained evidence, rejecting underflow and overflow.
    #[must_use]
    pub fn shifted(self, delta: i64) -> Option<Self> {
        let shift = |offset| u32::try_from(i64::from(offset).checked_add(delta)?).ok();
        let binding = self.binding.shifted(delta)?;
        Self::new(
            shift(self.owner_start)?,
            shift(self.owner_end)?,
            binding.start,
            binding.end,
        )
    }
}

/// Precision observed by the syntax producer; older facts stay legacy.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum SourceBindingIdentity {
    /// No exact source anchor was retained by this producer/record version.
    #[default]
    Legacy,
    /// A checked captured binding with separately retained owner evidence.
    Exact(SourceBindingCapture),
}

#[cfg(test)]
mod tests {
    use super::{SourceBinding, SourceBindingCapture};

    #[test]
    fn binding_rejects_empty_inverted_outside_and_overflowing_ranges() {
        let capture = SourceBindingCapture::new(10, 30, 12, 16).expect("capture");
        assert_eq!(capture.shifted(4).expect("relocated").owner(), (14, 34));
        assert!(capture.shifted(-11).is_none());
        assert!(capture.shifted(i64::from(u32::MAX)).is_none());
        for ranges in [
            (30, 10, 12, 16),
            (10, 30, 12, 12),
            (10, 30, 9, 16),
            (10, 30, 12, 31),
        ] {
            assert!(SourceBindingCapture::new(ranges.0, ranges.1, ranges.2, ranges.3).is_none());
        }
        assert!(SourceBinding::new(2, 2).is_none());
        assert!(SourceBinding::new(3, 2).is_none());
        let edited = SourceBindingCapture::new(10, 300, 12, 16).expect("edited body");
        assert_eq!(
            capture.binding(),
            edited.binding(),
            "parent identity excludes body extent"
        );
    }
}
