//! Build-only identity observer for the source-receipted Stylo packaging actor.
use backend_compile::{ExecutableIdentity, MAX_EXECUTABLE_BYTES, ToolchainArtifact};
use std::{env, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 3 {
        return Err("expected worker and libgcc paths".into());
    }
    let worker = Path::new(&args[1]);
    let dependency = Path::new(&args[2]);
    let worker_meta = std::fs::symlink_metadata(worker)?;
    let dependency_meta = std::fs::symlink_metadata(dependency)?;
    if !worker_meta.file_type().is_file()
        || !dependency_meta.file_type().is_file()
        || worker_meta.len() > MAX_EXECUTABLE_BYTES
        || dependency_meta.len() > 16 * 1024 * 1024
        || !worker.is_absolute()
        || !dependency.is_absolute()
        || dependency.file_name().and_then(|v| v.to_str()) != Some("libgcc_s.so.1")
    {
        return Err("invalid worker closure".into());
    }
    let library = ExecutableIdentity::from_path(dependency)?;
    let artifact = ToolchainArtifact::from_path(worker, vec![library.digest()])?;
    let hex: String = artifact
        .identity()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("{{\"state\":\"observed-helper\",\"identity\":\"{hex}\"}}");
    Ok(())
}
