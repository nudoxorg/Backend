//! Bind native authority to the source and declaration inputs compiled into it.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

fn collect(root: &Path, relative: &Path, inputs: &mut Vec<PathBuf>) -> io::Result<()> {
    let absolute = root.join(relative);
    let metadata = fs::symlink_metadata(&absolute)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "compiler identity input is a symlink: {}",
            relative.display()
        )));
    }
    if metadata.is_dir() {
        println!("cargo:rerun-if-changed={}", absolute.display());
        for entry in fs::read_dir(absolute)? {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("target" | ".target" | "tests" | "benches" | ".git")
            ) {
                continue;
            }
            collect(root, &relative.join(name), inputs)?;
        }
    } else if metadata.is_file() {
        inputs.push(relative.to_path_buf());
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| io::Error::other("missing compiler manifest directory"))?,
    );
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("missing compiler workspace root"))?;
    let mut inputs = Vec::new();
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "third_party/tsz/Cargo.toml",
        "third_party/tsz/Cargo.lock",
        "third_party/tsz/UPSTREAM_REVISION",
        "third_party/tsz/crates",
        "frontends/typescript",
        "crates/engine/src/application/typescript_host.rs",
        "crates/engine/src/application/typescript_program.rs",
        "crates/engine/src/application/typescript_program",
        "crates/engine/src/driver/lower/typescript.rs",
    ] {
        collect(root, Path::new(relative), &mut inputs)?;
    }
    let mut inputs = inputs
        .into_iter()
        .map(|relative| {
            let normalized = relative
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| io::Error::other("non-UTF8 compiler input path"))
                })
                .collect::<io::Result<Vec<_>>>()?
                .join("/");
            Ok((normalized, relative))
        })
        .collect::<io::Result<Vec<_>>>()?;
    inputs.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hash = Sha256::new();
    // v3 retains both anonymous-class parameter-property declaration roles.
    hash.update(b"backend.tsz.native-source-authority.v3\0");
    let mut buffer = [0u8; 64 * 1024];
    for (normalized, relative) in inputs {
        let absolute = root.join(&relative);
        println!("cargo:rerun-if-changed={}", absolute.display());
        hash.update((normalized.len() as u64).to_be_bytes());
        hash.update(normalized.as_bytes());
        let mut file = fs::File::open(absolute)?;
        let length = file.metadata()?.len();
        hash.update(length.to_be_bytes());
        let mut observed = 0u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            observed += count as u64;
        }
        if observed != length {
            return Err(io::Error::other("compiler input changed while hashing"));
        }
    }
    let output = PathBuf::from(
        env::var_os("OUT_DIR")
            .ok_or_else(|| io::Error::other("missing compiler build output directory"))?,
    );
    fs::write(output.join("tsz-native-authority.sha256"), hash.finalize())
}
