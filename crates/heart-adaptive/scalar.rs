//! Defines scalar behavior for `heart-adaptive`, whose purpose is to choose local or remote execution from typed resource and consistency facts.
//! This module owns the scalar invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{borrow::Borrow, ops::Deref};

macro_rules! semantic_scalar {
    ($(#[$attribute:meta])* $name:ident($primitive:ty)) => {
        $(#[$attribute])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name($primitive);

        impl From<$primitive> for $name {
            fn from(value: $primitive) -> Self {
                Self(value)
            }
        }

        impl From<$name> for $primitive {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Deref for $name {
            type Target = $primitive;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<$primitive> for $name {
            fn as_ref(&self) -> &$primitive {
                self
            }
        }

        impl Borrow<$primitive> for $name {
            fn borrow(&self) -> &$primitive {
                self
            }
        }
    };
}

semantic_scalar!(
    /// Exact bytes owned by a physical residence or reservation.
    ByteCount(u32)
);

impl ByteCount {
    pub(super) const fn fits_within(self, available: Self) -> bool {
        self.0 <= available.0
    }
}

semantic_scalar!(
    /// Number of independent actions admitted in the current interval.
    OperationBudget(u8)
);

impl OperationBudget {
    pub(super) const fn available(self) -> bool {
        self.0 > 0
    }
}

semantic_scalar!(
    /// Number of safe remote recovery attempts admitted in the current interval.
    RetryBudget(u8)
);

impl RetryBudget {
    pub(super) const fn available(self) -> bool {
        self.0 > 0
    }

    pub(super) const fn after_one(self) -> Self {
        Self(self.0 - 1)
    }
}

semantic_scalar!(
    /// Measured remote latency in microseconds.
    LatencyMicros(u32)
);

impl LatencyMicros {
    pub(super) const fn no_more_than(self, limit: Self) -> bool {
        self.0 <= limit.0
    }
}
