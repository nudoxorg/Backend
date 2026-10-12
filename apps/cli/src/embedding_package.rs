//! Admission of the optional helper carried by a hash-verified product package.
use backend_engine::application::EmbeddingRuntimeLimits;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: String,
    preset: String,
    source: Source,
    cli: FileRecord,
    helper: FileRecord,
    dependencies: Vec<FileRecord>,
    build_receipt_sha256: String,
    helper_identity: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    commit: String,
    tree: String,
    cargo_lock_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    path: String,
    sha256: String,
    bytes: u64,
}
pub(super) struct BundledHelper {
    pub program: PathBuf,
    pub dependencies: Vec<PathBuf>,
    pub identity: [u8; 32],
}
fn refuse(message: impl Into<String>) -> String {
    message.into()
}
fn hex(raw: &[u8]) -> String {
    raw.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn canonical_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn read(root: &Path, relative: &str, maximum: u64) -> Result<Vec<u8>, String> {
    let path = contained(root, relative)?;
    let directory = backend_platform::DirectoryCapability::open_read_only_source(
        path.parent().ok_or("missing parent")?,
    )
    .map_err(|e| e.to_string())?;
    let mut file = directory
        .open_file_read(
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid filename")?,
        )
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(refuse("package entry is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err(refuse("package metadata exceeds its byte bound"));
    }
    Ok(bytes)
}
fn contained(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
        || relative.len() > 4096
    {
        return Err(refuse("invalid package-relative path"));
    }
    let joined = root.join(path);
    let mut parent = root.to_path_buf();
    for part in path.components() {
        parent.push(part);
        if std::fs::symlink_metadata(&parent)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err(refuse("package path contains a symbolic link"));
        }
    }
    Ok(joined)
}
fn verify(root: &Path, record: &FileRecord) -> Result<PathBuf, String> {
    if record.bytes == 0 || record.bytes > 512 * 1024 * 1024 || !canonical_hex(&record.sha256, 32) {
        return Err(refuse("invalid packaged artifact extent or digest"));
    }
    let path = contained(root, &record.path)?;
    let directory = backend_platform::DirectoryCapability::open_read_only_source(
        path.parent().ok_or("missing parent")?,
    )
    .map_err(|e| e.to_string())?;
    let mut file = directory
        .open_file_read(
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid filename")?,
        )
        .map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.len() != record.bytes {
        return Err(refuse("packaged artifact differs from its declared length"));
    }
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > record.bytes {
            return Err(refuse("packaged artifact grew during admission"));
        }
        hash.update(&buffer[..count]);
    }
    if bytes != record.bytes || hex(&hash.finalize()) != record.sha256 {
        return Err(refuse(
            "packaged artifact differs from its trusted package receipt",
        ));
    }
    Ok(path)
}
pub(super) fn discover() -> Result<BundledHelper, String> {
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let root = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("installed CLI has no package root")?;
    discover_at(root, &executable)
}
fn discover_at(root: &Path, executable: &Path) -> Result<BundledHelper, String> {
    let raw = read(root, "share/nudox/embedding/setup.json", 65536).map_err(|e|format!("This installation has no admitted local embedding helper: {e}. Install a matched package with the Candle helper; advanced setup is available through nudox embedding-setup --help."))?;
    let receipt: Receipt = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if receipt.schema != "nudox.local-embedding-setup.v1"
        || receipt.preset != "candle-minilm-cpu-v1"
        || !canonical_hex(&receipt.source.commit, 20)
        || !canonical_hex(&receipt.source.tree, 20)
        || !canonical_hex(&receipt.source.cargo_lock_sha256, 32)
        || !canonical_hex(&receipt.build_receipt_sha256, 32)
        || !canonical_hex(&receipt.helper_identity, 32)
        || receipt.dependencies.len() > 64
    {
        return Err(refuse(
            "unsupported or incomplete local helper build receipt",
        ));
    }
    let package: serde_json::Value =
        serde_json::from_slice(&read(root, "packaging-manifest.json", 1024 * 1024)?)
            .map_err(|e| e.to_string())?;
    let setup = &package["embedding_setup"];
    if setup["path"].as_str() != Some("share/nudox/embedding/setup.json")
        || setup["sha256"].as_str() != Some(hex(&Sha256::digest(&raw)).as_str())
        || package["source"]["commit"].as_str() != Some(&receipt.source.commit)
        || package["source"]["tree"].as_str() != Some(&receipt.source.tree)
    {
        return Err(refuse(
            "local helper receipt is not bound to this product package/source",
        ));
    }
    if receipt.cli.path != "bin/backend-cli"
        || receipt.helper.path != "share/nudox/embedding/bin/backend-embedding-candle"
        || verify(root, &receipt.cli)? != executable
    {
        return Err(refuse(
            "local helper receipt belongs to a different installed CLI",
        ));
    }
    let program = verify(root, &receipt.helper)?;
    let mut dependencies = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    for dependency in &receipt.dependencies {
        if !dependency.path.starts_with("lib/") || !paths.insert(&dependency.path) {
            return Err(refuse("invalid helper dependency closure"));
        }
        dependencies.push(verify(root, dependency)?);
    }
    let identity =
        backend_local_service::builtin::query::inspect_candle_helper(&program, &dependencies)
            .map_err(|e| e.to_string())?;
    if hex(&identity) != receipt.helper_identity {
        return Err(refuse(
            "observed helper/dependency identity differs from the source-bound package receipt",
        ));
    }
    Ok(BundledHelper {
        program,
        dependencies,
        identity,
    })
}
pub(super) const fn default_limits() -> EmbeddingRuntimeLimits {
    EmbeddingRuntimeLimits {
        stdout_bytes: 1024 * 1024,
        stderr_bytes: 65536,
        output_bytes: 1024 * 1024 + 65536,
        input_bytes: 1024 * 1024,
        wall_time_ms: 120000,
        workspace_bytes: 128 * 1024 * 1024,
        process_count: 4,
        memory_bytes: Some(1024 * 1024 * 1024),
        cpu_time_ms: Some(120000),
        resident_credit_bytes: 2 * 1024 * 1024 * 1024,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_preset_defaults_fit_existing_runtime_admission_without_implicit_growth() {
        default_limits()
            .validate(
                90_868_376,
                466_247,
                std::num::NonZeroU16::new(384).unwrap(),
                std::num::NonZeroU32::new(64 * 1024).unwrap(),
            )
            .unwrap();
        assert!(canonical_hex(&"a".repeat(64), 32));
        assert!(!canonical_hex(&"A".repeat(64), 32));
    }
    #[test]
    fn packaged_receipt_binds_cli_source_and_exact_dependency_identity_without_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "nudox-helper-receipt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&root)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        for directory in ["bin", "lib", "share/nudox/embedding/bin"] {
            std::fs::create_dir_all(root.join(directory))?;
        }
        let record = |path: &str, bytes: &[u8]| -> Result<serde_json::Value, std::io::Error> {
            std::fs::write(root.join(path), bytes)?;
            Ok(
                serde_json::json!({"path":path,"bytes":bytes.len(),"sha256":hex(&Sha256::digest(bytes))}),
            )
        };
        let cli = record(
            "bin/backend-cli",
            b"fixture CLI bytes, not a product executable",
        )?;
        let helper = record(
            "share/nudox/embedding/bin/backend-embedding-candle",
            b"fixture helper metadata, never executed",
        )?;
        let dependency = record("lib/dependency.so", b"fixture dependency")?;
        let identity = backend_local_service::builtin::query::inspect_candle_helper(
            &root.join("share/nudox/embedding/bin/backend-embedding-candle"),
            &[root.join("lib/dependency.so")],
        )?;
        let source = serde_json::json!({"commit":"a".repeat(40),"tree":"b".repeat(40),"cargo_lock_sha256":"c".repeat(64)});
        let receipt = serde_json::json!({"schema":"nudox.local-embedding-setup.v1","preset":"candle-minilm-cpu-v1","source":source,"cli":cli,"helper":helper,"dependencies":[dependency],"helper_identity":hex(&identity),"build_receipt_sha256":"d".repeat(64)});
        let bytes = serde_json::to_vec(&receipt)?;
        std::fs::write(root.join("share/nudox/embedding/setup.json"), &bytes)?;
        let package = serde_json::json!({"source":source,"embedding_setup":{"path":"share/nudox/embedding/setup.json","sha256":hex(&Sha256::digest(&bytes))}});
        std::fs::write(
            root.join("packaging-manifest.json"),
            serde_json::to_vec(&package)?,
        )?;
        let admitted = discover_at(&root, &root.join("bin/backend-cli"))?;
        assert_eq!(admitted.identity, identity);
        assert_eq!(admitted.dependencies, vec![root.join("lib/dependency.so")]);
        assert!(discover_at(&root, &root.join("bin/another-cli")).is_err());
        std::fs::write(root.join("lib/dependency.so"), b"changed dependency")?;
        assert!(discover_at(&root, &root.join("bin/backend-cli")).is_err());
        assert!(contained(&root, "../outside").is_err());
        Ok(())
    }
}
