//! Neutral native-row paging; it grants no document or owner admission.

/// A single render or disclosure action prepares at most this many rows.
pub(crate) const ROWS_PER_PAGE: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NavigationPage {
    start: usize,
}

impl NavigationPage {
    pub(crate) fn containing(index: usize) -> Self {
        Self {
            start: index / ROWS_PER_PAGE * ROWS_PER_PAGE,
        }
    }

    pub(crate) fn range(self, total: usize) -> std::ops::Range<usize> {
        let start = self.start.min(total);
        start..start.saturating_add(ROWS_PER_PAGE).min(total)
    }

    pub(crate) fn previous(self) -> Option<Self> {
        (self.start != 0).then(|| Self {
            start: self.start.saturating_sub(ROWS_PER_PAGE),
        })
    }

    pub(crate) fn next(self, total: usize) -> Option<Self> {
        let start = self.range(total).end;
        (start < total).then_some(Self { start })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_reachable_with_bounded_work_and_exact_return_pages() {
        for total in [0, 1, 32, 33, 512, 513, 2_049] {
            let mut seen = Vec::new();
            let mut page = NavigationPage::default();
            loop {
                let range = page.range(total);
                assert!(range.len() <= ROWS_PER_PAGE);
                seen.extend(range);
                let Some(next) = page.next(total) else {
                    break;
                };
                assert_eq!(next.previous(), Some(page));
                page = next;
            }
            assert_eq!(seen, (0..total).collect::<Vec<_>>());
            for index in 0..total {
                assert!(
                    NavigationPage::containing(index)
                        .range(total)
                        .contains(&index)
                );
            }
        }
        assert_eq!(NavigationPage::default().previous(), None);
        assert_eq!(NavigationPage::containing(2_048).range(2_049), 2_048..2_049);
    }
}
