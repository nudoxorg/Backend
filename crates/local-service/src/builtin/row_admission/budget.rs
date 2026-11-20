//! A retained requested-layout ledger with separate live scratch checks.

pub(crate) struct RequestedLayoutBudget {
    retained: usize,
    maximum: usize,
}

impl RequestedLayoutBudget {
    pub(crate) fn new(retained: usize, maximum: usize) -> Result<Self, &'static str> {
        if retained > maximum {
            return Err("retained row layout exceeds byte bound");
        }
        Ok(Self { retained, maximum })
    }

    pub(crate) const fn retained(&self) -> usize {
        self.retained
    }

    /// Charges storage that survives this operation. `other_live` is temporary
    /// storage such as the selected catalog or both buffers during a transfer.
    /// Successful retained charges have no release operation in this ledger.
    pub(crate) fn retain(&mut self, bytes: usize, other_live: usize) -> Result<(), &'static str> {
        let retained = self
            .retained
            .checked_add(bytes)
            .ok_or("row requested layout overflow")?;
        let live = retained
            .checked_add(other_live)
            .ok_or("row requested layout overflow")?;
        if live > self.maximum {
            return Err("retained row layout exceeds byte bound");
        }
        self.retained = retained;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_small_rows_exhaust_the_retained_budget() {
        let mut budget = RequestedLayoutBudget::new(128, 1024).expect("budget");
        for _ in 0..6 {
            budget.retain(128, 64).expect("retained row");
        }
        assert_eq!(budget.retained(), 896);
        assert!(budget.retain(128, 64).is_err());
        assert_eq!(
            budget.retained(),
            896,
            "rejection leaves the previous charge intact"
        );
        budget
            .retain(128, 0)
            .expect("exact boundary without catalog");
        assert_eq!(budget.retained(), 1024);
        assert!(budget.retain(1, 0).is_err());
    }

    #[test]
    fn temporary_live_storage_does_not_replace_retained_storage() {
        let mut budget = RequestedLayoutBudget::new(100, 1000).expect("budget");
        budget.retain(300, 600).expect("exact peak");
        assert_eq!(budget.retained(), 400);
        assert!(budget.retain(1, 600).is_err());
        budget.retain(1, 0).expect("scratch retired");
        assert_eq!(budget.retained(), 401);
    }

    #[test]
    fn overflowing_charge_is_refused_before_any_allocation() {
        let mut budget = RequestedLayoutBudget::new(1, usize::MAX).expect("budget");
        assert!(budget.retain(usize::MAX, 0).is_err());
        assert!(budget.retain(0, usize::MAX).is_err());
        assert_eq!(budget.retained(), 1);
    }
}
