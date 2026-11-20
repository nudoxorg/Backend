//! Build-only observer of the final SQLite worker closure and native basis.
//! The enclosing packaging actor admits these paths and observes this example
//! from the same source/lock build as the worker; this is not runtime admission.
use backend_sql_contract::ToolchainArtifact;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty()
        || paths.len() > 2
        || paths.iter().any(|path| !Path::new(path).is_absolute())
    {
        return Err("expected absolute worker path and optional Linux libgcc path".into());
    }
    let mut dependencies = Vec::new();
    if paths.len() == 2 {
        #[cfg(target_os = "linux")]
        {
            let library = Path::new(&paths[1]);
            if library.file_name().and_then(|name| name.to_str()) != Some("libgcc_s.so.1") {
                return Err("expected fixed libgcc dependency".into());
            }
            dependencies
                .push(backend_sql_contract::ExecutableIdentity::from_path(library)?.digest());
        }
        #[cfg(not(target_os = "linux"))]
        return Err("private libgcc is Linux-only".into());
    }
    for path in &paths {
        if !std::fs::symlink_metadata(path)?.file_type().is_file() {
            return Err("observer inputs must be regular files".into());
        }
    }
    let artifact = ToolchainArtifact::from_path(Path::new(&paths[0]), dependencies)?;
    let identity: String = artifact
        .identity()
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "state": "observed-build-helper",
            "artifact_identity": identity,
            "basis": backend_frontend_sqlite_experimental::observed_basis(),
        })
    );
    Ok(())
}
