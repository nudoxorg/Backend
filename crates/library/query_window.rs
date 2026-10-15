//! Issued catalog credit and the independently bounded delivery at one frontier.

use crate::QueryLimit;

/// One catalog query's immutable issued ceiling and this read's delivery bound.
/// Legacy callers retain fixed-width semantics and their original wire spelling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QueryWindow {
    issued: QueryLimit,
    delivery: QueryLimit,
    windowed: bool,
}

impl QueryWindow {
    /// Retains the historical fixed-width query contract.
    #[must_use]
    pub const fn fixed(limit: QueryLimit) -> Self {
        Self {
            issued: limit,
            delivery: limit,
            windowed: false,
        }
    }

    /// Creates a windowed read without changing its issued query credit.
    #[must_use]
    pub const fn new(issued: QueryLimit, delivery: QueryLimit) -> Option<Self> {
        if delivery.get() > issued.get() {
            return None;
        }
        Some(Self {
            issued,
            delivery,
            windowed: true,
        })
    }

    /// Immutable ceiling bound into the query recipe and public continuation.
    #[must_use]
    pub const fn issued(self) -> QueryLimit {
        self.issued
    }

    /// Maximum complete rows selected by this one native read.
    #[must_use]
    pub const fn delivery(self) -> QueryLimit {
        self.delivery
    }

    /// Whether this read uses the versioned independent-span contract.
    #[must_use]
    pub const fn is_windowed(self) -> bool {
        self.windowed
    }

    /// Selects another positive delivery bound under the same issued ceiling.
    #[must_use]
    pub const fn with_delivery(self, delivery: QueryLimit) -> Option<Self> {
        Self::new(self.issued, delivery)
    }

    /// The exact predecessor contract, which may still be legacy fixed-width.
    /// A windowed predecessor cannot be reinterpreted as a legacy request.
    #[must_use]
    pub const fn predecessor(self, position: QueryPosition) -> Option<Self> {
        match position.predecessor() {
            None => Some(Self::fixed(self.issued)),
            Some(previous) if self.windowed && previous.issued.get() == self.issued.get() => {
                Some(previous)
            }
            Some(_) => None,
        }
    }
}

/// A query position whose predecessor width is independent of the next read.
/// Private state excludes zero/oversized spans and offsets before their span.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QueryPosition(Position);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Position {
    Legacy(u64),
    Windowed {
        offset: u64,
        predecessor: QueryWindow,
    },
}

impl QueryPosition {
    pub(crate) const fn legacy(offset: u64) -> Self {
        Self(Position::Legacy(offset))
    }

    pub(crate) const fn windowed(offset: u64, predecessor: QueryWindow) -> Option<Self> {
        if !predecessor.is_windowed() || offset < predecessor.delivery().get() as u64 {
            return None;
        }
        Some(Self(Position::Windowed {
            offset,
            predecessor,
        }))
    }

    /// Absolute next row offset, never a packed width or a guessed boundary.
    #[must_use]
    pub const fn offset(self) -> u64 {
        match self.0 {
            Position::Legacy(offset) | Position::Windowed { offset, .. } => offset,
        }
    }

    /// Exact issued ceiling and actual predecessor count of a windowed cursor.
    #[must_use]
    pub const fn predecessor(self) -> Option<QueryWindow> {
        match self.0 {
            Position::Legacy(_) => None,
            Position::Windowed { predecessor, .. } => Some(predecessor),
        }
    }

    /// Admits the predecessor span under the unchanged caller-issued ceiling.
    #[must_use]
    pub(crate) const fn predecessor_rows(self, issued: QueryLimit) -> Option<usize> {
        match self.predecessor() {
            None => Some(issued.get() as usize),
            Some(window) if window.issued().get() == issued.get() => {
                Some(window.delivery().get() as usize)
            }
            Some(_) => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn query_window_keeps_credit_and_rejects_impossible_predecessors() {
        let limit = QueryLimit::new(12).expect("credit");
        let one = QueryLimit::new(1).expect("one");
        assert!(QueryWindow::new(one, limit).is_none());
        let window = QueryWindow::new(limit, one).expect("window");
        assert!(QueryPosition::windowed(0, window).is_none());
        assert!(QueryPosition::windowed(12, QueryWindow::fixed(limit)).is_none());
        let position = QueryPosition::windowed(13, window).expect("after actual row");
        assert_eq!(position.offset(), 13);
        assert_eq!(position.predecessor_rows(limit), Some(1));
        assert_eq!(position.predecessor_rows(one), None);
    }
}
