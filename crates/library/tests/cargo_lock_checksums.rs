//! Lockfile checksums are exact input commitments, not inferred package identities.
use backend_library::browse::cargo_locked_packages;

#[test]
fn inventory_preserves_exact_checksums_and_keeps_missing_checksums_absent() {
    let checksum = "a0".repeat(32);
    let lock = format!(
        r#"version = 4
[[package]]
name = "registry-package"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{checksum}"
[[package]]
name = "local-package"
version = "0.1.0"
"#
    );
    let packages = cargo_locked_packages(&lock).expect("valid lock inventory");
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0].checksum, Some([0xa0; 32]));
    assert_eq!(packages[1].checksum, None);
    assert_eq!(packages[1].source, None);
    let uppercase = lock.replace(&checksum, &checksum.to_uppercase());
    assert_eq!(
        cargo_locked_packages(&uppercase).expect("hex case")[0].checksum,
        packages[0].checksum
    );
}

#[test]
fn malformed_checksum_is_not_downgraded_to_an_absent_commitment() {
    for checksum in ["7".into(), "0".repeat(63), "g".repeat(64), "é".repeat(32)] {
        let lock = format!(
            r#"version = 4
[[package]]
name = "broken"
version = "1.0.0"
checksum = "{checksum}"
"#
        );
        assert!(
            cargo_locked_packages(&lock).is_err(),
            "malformed hash accepted"
        );
    }
    assert!(
        cargo_locked_packages(
            r#"version = 4
[[package]]
name = "broken"
version = "1.0.0"
checksum = 42
"#
        )
        .is_err(),
        "wrong TOML type accepted"
    );
}
