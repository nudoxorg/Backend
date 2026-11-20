//! Build-only identity observer for an installed SQL worker.
//!
//! Prints the executable identity an owner build embeds as
//! `NUDOX_BUILD_SQLITE_WORKER_ID` / `NUDOX_BUILD_POSTGRES_WORKER_ID`, so that
//! the owner admits exactly this worker file when it sits beside it.
use backend_compile::{MAX_EXECUTABLE_BYTES, ToolchainArtifact};
use std::{env, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        return Err("expected one absolute worker path".into());
    }
    let worker = Path::new(&args[1]);
    let metadata = std::fs::symlink_metadata(worker)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_EXECUTABLE_BYTES || !worker.is_absolute() {
        return Err("invalid worker file".into());
    }
    let artifact = ToolchainArtifact::from_path(worker, Vec::new())?;
    let hex: String = artifact.identity().to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
    Ok(())
}
