//! TTL jitter — spread expiries so a batch of entries written together (a warm
//! fill, a reindex wave) doesn't all expire on the same instant and stampede the
//! backend at once. Complements the per-read probabilistic refresh in
//! [`crate::stampede`]: jitter de-synchronises *writes*, XFetch de-synchronises
//! *refreshes*.

use std::time::Duration;

/// Return `base` scaled by a uniform random factor in `[1 - frac, 1 + frac]`.
///
/// `frac` is clamped to `[0.0, 1.0]`; `frac = 0.0` returns `base` unchanged,
/// `frac = 0.2` yields ±20%. Use when seeding many entries with the "same" TTL.
pub fn jittered(base: Duration, frac: f64) -> Duration {
    let frac = frac.clamp(0.0, 1.0);
    if frac == 0.0 {
        return base;
    }
    let u: f64 = rand::random::<f64>(); // [0, 1)
    let factor = 1.0 - frac + u * (2.0 * frac);
    base.mul_f64(factor.max(0.0))
}
