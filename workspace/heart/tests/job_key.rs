//! JobKey layout stability (historical CacheKey::derive).

use heart::JobKey;

#[test]
fn derive_is_stable_and_matches_historical_layout() {
    let a = JobKey::derive(b"1.0.0", b"tc", b"src", b"lock");
    let b = JobKey::derive(b"1.0.0", b"tc", b"src", b"lock");
    assert_eq!(a, b);
    // Golden: length-prefixed blake3 of the four CacheKey components.
    assert_eq!(
        a.hex(),
        "3ae60363db0e590a112c9a1bf085bc8ad744a168a7263fea78b1390b2bb26e3e"
    );
    assert_ne!(a, JobKey::derive(b"1.0.1", b"tc", b"src", b"lock"));
    assert_ne!(a.with_tag(b"cst"), a.with_tag(b"archive"));
    assert_ne!(a.with_tag(b"cst"), a.as_hash());
}
