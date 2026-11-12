//! Experimental file-level Landlock preparation, not an execution proof.
//!
//! Landlock does not revoke access via already-open file descriptors. This
//! module therefore must not be connected to production spawning until a
//! launcher closes inherited descriptors and proves the complete grants and
//! snapshot custody. Applying a ruleset permanently restricts the *calling
//! thread*. It must only be called in a dedicated private child.
#![allow(dead_code)]

use std::fs::File;

use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr, RulesetStatus,
};

/// Construct an enforce-or-fail rule set from held file descriptors. Ordinary
/// files only: directory subtree grants would allow unenumerated native loads.
/// This function does not perform process isolation, close inherited FDs, or
/// prove that the files will remain immutable while the child runs.
pub(super) fn prepare(
    read_files: &[File],
    executable_files: &[File],
    enumerate_directories: &[File],
) -> Result<RulesetCreated, ()> {
    if read_files.len().saturating_add(executable_files.len()) > 8192 {
        return Err(());
    }
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V3))
        .map_err(|_| ())?
        .create()
        .map_err(|_| ())?;
    for file in read_files {
        if !file.metadata().map_err(|_| ())?.is_file() {
            return Err(());
        }
        ruleset = ruleset
            .add_rule(PathBeneath::new(file, AccessFs::ReadFile))
            .map_err(|_| ())?;
    }
    for file in executable_files {
        if !file.metadata().map_err(|_| ())?.is_file() {
            return Err(());
        }
        ruleset = ruleset
            .add_rule(PathBeneath::new(
                file,
                AccessFs::ReadFile | AccessFs::Execute,
            ))
            .map_err(|_| ())?;
    }
    for directory in enumerate_directories {
        if !directory.metadata().map_err(|_| ())?.is_dir() {
            return Err(());
        }
        ruleset = ruleset
            .add_rule(PathBeneath::new(directory, AccessFs::ReadDir))
            .map_err(|_| ())?;
    }
    Ok(ruleset)
}

/// Enforce the previously prepared ruleset in the dedicated private child.
/// Failure must terminate the child; it must never continue to the workload.
pub(super) fn enforce(ruleset: RulesetCreated) -> Result<(), ()> {
    let status = ruleset.restrict_self().map_err(|_| ())?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read, process::Command};

    // The child applies the rule only to its test thread. A separate process
    // protects the rest of the harness from irreversible Landlock policy.
    #[test]
    fn private_child_denies_ungranted_file_reads_and_writes() {
        const CHILD: &str = "NUDOX_LANDLOCK_FIXTURE_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = std::path::PathBuf::from(root);
            let allowed = root.join("allowed");
            let outside = root.join("outside");
            let fd = File::open(&allowed).expect("open allowed file");
            let mut inherited = File::open(&outside).expect("open before confinement");
            let rules = match prepare(&[fd], &[], &[]) {
                Ok(rules) => rules,
                Err(()) => std::process::exit(77),
            };
            enforce(rules).expect("fully enforced Landlock");
            assert_eq!(fs::read(&allowed).unwrap(), b"allowed");
            assert!(fs::read(&outside).is_err());
            assert!(fs::write(&outside, b"changed").is_err());
            // Landlock does not revoke pre-existing file descriptions. This
            // demonstrates why this candidate cannot yet launch production.
            let mut prior = String::new();
            inherited.read_to_string(&mut prior).unwrap();
            assert_eq!(prior, "outside");
            return;
        }
        let fixture = tempfile::tempdir().expect("fixture");
        fs::write(fixture.path().join("allowed"), b"allowed").unwrap();
        fs::write(fixture.path().join("outside"), b"outside").unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "legacy::landlock_candidate::tests::private_child_denies_ungranted_file_reads_and_writes", "--nocapture"])
            .env(CHILD, fixture.path())
            .status()
            .expect("spawn Landlock test child");
        if status.code() == Some(77) {
            // Production remains guarded on unsupported kernels. In the
            // designated Linux test environment, require actual enforcement.
            assert!(std::env::var_os("NUDOX_REQUIRE_LANDLOCK_TEST").is_none());
            return;
        }
        assert!(status.success());
        assert_eq!(
            fs::read(fixture.path().join("outside")).unwrap(),
            b"outside"
        );
    }
}
