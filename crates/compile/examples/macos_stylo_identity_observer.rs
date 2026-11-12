//! Build-only observer for the final signed macOS Stylo worker bytes.
use backend_compile::{ExecutableIdentity, ToolchainArtifact};
use std::{
    env,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        return Err("expected one absolute worker path".into());
    }
    let worker = Path::new(&args[1]);
    if !worker.is_absolute() || !std::fs::symlink_metadata(worker)?.file_type().is_file() {
        return Err("invalid worker path".into());
    }
    let captured = ExecutableIdentity::from_path_until(
        worker,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(30),
    )?;
    let identity = ToolchainArtifact::new(captured, Vec::new())
        .identity()
        .to_bytes();
    let hex: String = identity.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("{{\"state\":\"observed-helper\",\"identity\":\"{hex}\"}}");
    Ok(())
}
