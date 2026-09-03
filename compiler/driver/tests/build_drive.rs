#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{BuildDriveFailure, discover_and_drive};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

fn directory(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!("nudox-build-drive-{name}-{nonce}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

#[test]
fn no_marker_is_a_typed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("none")?;
    let result = discover_and_drive(&root, &root, &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(BuildDriveFailure::NoBuildSystemDetected { .. })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn absent_cmake_is_not_silently_skipped() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("cmake")?;
    fs::write(
        root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.0)\n",
    )?;
    let result = discover_and_drive(&root, &root, &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(BuildDriveFailure::ToolAbsent { tool: "cmake" })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cancellation_before_drive_prevents_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("cancel")?;
    fs::write(root.join("Makefile"), "all:\n\tfalse\n")?;
    let cancelled = AtomicBool::new(true);
    let result = discover_and_drive(&root, &root, &cancelled);
    assert!(matches!(result, Err(BuildDriveFailure::Cancelled)));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_dry_run_retains_compiler_arguments_and_database_location()
-> Result<(), Box<dyn std::error::Error>> {
    let root = directory("make")?;
    fs::create_dir(root.join("include"))?;
    fs::write(root.join("include/config.h"), "#define DRIVE_FLAG 1\n")?;
    fs::write(
        root.join("main.c"),
        "#include <config.h>\nint main(void) { return DRIVE_FLAG; }\n",
    )?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tclang -Iinclude -c main.c -o main.o\n",
    )?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(result.translation_units.len(), 1);
    assert!(
        result.translation_units[0]
            .arguments
            .iter()
            .any(|arg| arg == "-Iinclude")
    );
    assert!(
        result
            .database_directory
            .join("compile_commands.json")
            .is_file()
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
