//! Prospective requested-allocation accounting for a retirement admission.
//!
//! This meters requested allocations and explicitly charged bookkeeping, not
//! RSS. Callers must reserve before allocation and charge every retained copy.
//! A borrowed value must not be cloned or otherwise escape without a separate
//! reservation. The admission remains disabled until all paths obey this law.
use std::fmt;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy)]
pub struct RetirementBudgetError;

impl fmt::Display for RetirementBudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("retirement allocation budget exceeded or poisoned")
    }
}

impl std::error::Error for RetirementBudgetError {}

struct State {
    available: usize,
    limit: usize,
    failed: bool,
}

/// Single admission authority. Reservations share its counter; the authority
/// itself is deliberately not cloneable.
pub struct RetirementBudget(Arc<Mutex<State>>);

/// A prospective charge. Hold it until the covered temporaries are dropped.
#[must_use]
pub struct RetirementReservation {
    state: Arc<Mutex<State>>,
    bytes: usize,
}

/// A retained value and its inseparable charge. This is deliberately not Clone.
#[must_use]
pub struct Metered<T> {
    value: Option<T>,
    reservation: RetirementReservation,
}

impl RetirementBudget {
    /// Permanently debit one admission. Use for ownership headers whose
    /// deallocation occurs after their contained values are dropped (such as
    /// an Arc allocation). No refund can race the final header deallocation.
    pub fn charge_permanent(&self, bytes: usize) -> Result<(), RetirementBudgetError> {
        let mut state = self.0.lock().map_err(|_| RetirementBudgetError)?;
        if state.failed {
            return Err(RetirementBudgetError);
        }
        let (Some(available), Some(limit)) = (
            state.available.checked_sub(bytes),
            state.limit.checked_sub(bytes),
        ) else {
            state.available = 0;
            state.failed = true;
            return Err(RetirementBudgetError);
        };
        state.available = available;
        state.limit = limit;
        Ok(())
    }

    /// Verify that a retained value is charged to this admission's ledger.
    pub fn owns<T>(&self, value: &Metered<T>) -> bool {
        Arc::ptr_eq(&self.0, &value.reservation.state)
    }

    /// Check ledger identity and a minimum retained charge. This does not
    /// prove that the reservation preceded allocation or cover hidden graphs;
    /// authoritative producers must enforce those properties separately.
    pub fn owns_and_covers<T>(&self, value: &Metered<T>, bytes: usize) -> bool {
        self.owns(value) && value.reservation.bytes >= bytes
    }

    /// Check an opaque producer's retained reservation without exposing it.
    pub fn owns_reservation_and_covers(&self, value: &RetirementReservation, bytes: usize) -> bool {
        Arc::ptr_eq(&self.0, &value.state) && value.bytes >= bytes
    }

    /// Share a metered value without permitting the Arc header charge to be
    /// refunded before its final deallocation. The permanent charge includes
    /// the inline wrapper; the inner value retains its own reservation.
    pub fn share_metered<T>(
        &self,
        value: Metered<T>,
        bookkeeping: usize,
    ) -> Result<Arc<Metered<T>>, RetirementBudgetError> {
        if !self.owns(&value) {
            if let Ok(mut state) = self.0.lock() {
                state.available = 0;
                state.failed = true;
            }
            return Err(RetirementBudgetError);
        }
        let Some(bytes) = std::mem::size_of::<Metered<T>>()
            .checked_add(2 * std::mem::size_of::<usize>())
            .and_then(|n| n.checked_add(bookkeeping))
        else {
            if let Ok(mut state) = self.0.lock() {
                state.available = 0;
                state.failed = true;
            }
            return Err(RetirementBudgetError);
        };
        self.charge_permanent(bytes)?;
        Ok(Arc::new(value))
    }

    /// The caller must include the ledger's own allocation in its limit.
    pub fn new(limit: usize) -> Result<Self, RetirementBudgetError> {
        // Charge the state, Arc header and allocator bookkeeping conservatively.
        let overhead = std::mem::size_of::<Mutex<State>>()
            .checked_add(2 * std::mem::size_of::<usize>())
            .and_then(|n| n.checked_add(64))
            .ok_or(RetirementBudgetError)?;
        let available = limit.checked_sub(overhead).ok_or(RetirementBudgetError)?;
        Ok(Self(Arc::new(Mutex::new(State {
            available,
            limit: available,
            failed: false,
        }))))
    }

    /// Reserve before allocating; no caller code executes while the lock is held.
    pub fn reserve(&self, bytes: usize) -> Result<RetirementReservation, RetirementBudgetError> {
        let mut state = self.0.lock().map_err(|_| RetirementBudgetError)?;
        if state.failed {
            return Err(RetirementBudgetError);
        }
        let Some(available) = state.available.checked_sub(bytes) else {
            state.available = 0;
            state.failed = true;
            return Err(RetirementBudgetError);
        };
        state.available = available;
        drop(state);
        Ok(RetirementReservation {
            state: Arc::clone(&self.0),
            bytes,
        })
    }
}

impl RetirementReservation {
    /// Transfer this charge to a retained value. The value must have been
    /// allocated only after the reservation was obtained.
    pub fn meter<T>(self, value: T) -> Metered<T> {
        Metered {
            value: Some(value),
            reservation: self,
        }
    }
}

impl<T> Metered<T> {
    /// Borrow the charged value. Any retained clone requires its own charge.
    pub fn get(&self) -> &T {
        self.value
            .as_ref()
            .expect("metered value is present until drop")
    }
}

impl<T> Drop for Metered<T> {
    fn drop(&mut self) {
        // Never refund while the value's destructor is running.
        drop(self.value.take());
        let _ = &self.reservation;
    }
}

impl Drop for RetirementReservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if state.failed {
                return;
            }
            match state.available.checked_add(self.bytes) {
                Some(available) if available <= state.limit => state.available = available,
                _ => {
                    state.available = 0;
                    state.failed = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_and_coverage_require_the_exact_ledger_and_charge() {
        let owner = RetirementBudget::new(4096).expect("owner ledger");
        let foreign = RetirementBudget::new(4096).expect("foreign ledger");
        let held = owner.reserve(128).expect("reservation").meter(vec![0u8; 64]);
        assert!(owner.owns(&held));
        assert!(owner.owns_and_covers(&held, 128));
        assert!(!owner.owns_and_covers(&held, 129));
        assert!(!foreign.owns(&held));
        assert!(!foreign.owns_and_covers(&held, 0));

        let reservation = owner.reserve(64).expect("opaque reservation");
        assert!(owner.owns_reservation_and_covers(&reservation, 64));
        assert!(!owner.owns_reservation_and_covers(&reservation, 65));
        assert!(!foreign.owns_reservation_and_covers(&reservation, 0));
    }

    #[test]
    fn sharing_a_foreign_metered_value_fails_the_target_ledger_closed() {
        let owner = RetirementBudget::new(4096).expect("owner ledger");
        let foreign = RetirementBudget::new(4096).expect("foreign ledger");
        let held = owner.reserve(64).expect("reservation").meter(vec![0u8; 1]);
        assert!(foreign.share_metered(held, 0).is_err());
        assert!(foreign.reserve(1).is_err());
    }
}
