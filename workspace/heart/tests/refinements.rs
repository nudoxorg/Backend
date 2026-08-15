//! Real (passing) tests for the refinement newtypes — these prove the illegal
//! values are genuinely unconstructible.

use heart::Score;

/// `Score` cannot hold NaN or ±∞.
#[test]
fn score_rejects_non_finite() {
    assert!(Score::try_new(f32::NAN).is_err());
    assert!(Score::try_new(f32::INFINITY).is_err());
    assert!(Score::try_new(f32::NEG_INFINITY).is_err());
    assert!(Score::try_new(0.5).is_ok());
}

/// Because every `Score` is finite, it has a *total* order — `sort` is sound and
/// can never panic.
#[test]
fn score_is_totally_ordered() {
    let lo = Score::try_new(0.1).unwrap();
    let hi = Score::try_new(0.9).unwrap();
    assert!(hi > lo);

    let mut scores = [hi, lo];
    scores.sort();
    assert_eq!(scores[0], lo);
    assert_eq!(scores[1], hi);
}
