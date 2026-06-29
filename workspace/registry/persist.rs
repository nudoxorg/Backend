//! Durable persistence of the tracked-package registry across restarts.
//!
//! On load, transient sync progress is cleared so a process that died
//! mid-sync rehydrates into a clean, re-enqueueable state.
