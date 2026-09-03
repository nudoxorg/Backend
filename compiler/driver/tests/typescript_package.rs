#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_languages_typescript::{Checker, CheckerError, Origin};
use compiler_vocabulary::TypeScriptSource;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use thiserror::Error;

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum TestError {
    #[error("I/O: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("checker: {0}")]
    Checker(#[from] CheckerError),
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

fn fixture() -> Result<PathBuf, TestError> {
    let root = std::env::temp_dir().join(format!(
        "nudox-ts-package-{}-{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("lib")).map_err(io)?;
    fs::create_dir_all(root.join("node_modules/dep")).map_err(io)?;
    fs::write(root.join("lib/util.ts"), "export const util = 40;\n").map_err(io)?;
    fs::write(
        root.join("node_modules/dep/index.d.ts"),
        "export declare const dep: 2;\n",
    )
    .map_err(io)?;
    Ok(root)
}

fn digest(root: &Path) -> Result<[u8; 32], TestError> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, bytes) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(hasher.finalize().into())
}

fn collect(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), TestError> {
    for entry in fs::read_dir(current).map_err(io)? {
        let entry = entry.map_err(io)?;
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|source| io(std::io::Error::other(source)))?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, fs::read(path).map_err(io)?));
        }
    }
    Ok(())
}

fn rows(report: &compiler_languages_typescript::Report) -> Vec<(String, Option<String>)> {
    report
        .references
        .iter()
        .filter_map(|reference| Some((reference.name.clone()?, reference.module.clone())))
        .collect()
}

#[test]
fn package_root_resolves_modules_without_mutating_the_caller_tree() -> Result<(), TestError> {
    let root = fixture()?;
    let source = br#"export { util } from "./lib/util";
import { dep } from "dep";
import { missing } from "./missing";
export const result = dep;
"#;
    let before = digest(&root)?;
    let checker = Checker::default();
    let bare = checker.run(TypeScriptSource::TypeScript, source)?;
    let package = checker.run_in_package(TypeScriptSource::TypeScript, source, &root)?;
    let after = digest(&root)?;

    assert_eq!(before, after, "package tree changed during checker runs");
    assert!(bare.references.iter().all(|reference| {
        reference.module.as_deref() != Some("./lib/util")
            && reference.module.as_deref() != Some("dep")
    }));
    let package_rows = rows(&package);
    assert!(package_rows.contains(&("util".to_owned(), Some("./lib/util".to_owned()))));
    assert!(package_rows.contains(&("dep".to_owned(), Some("dep".to_owned()))));
    assert!(
        !package_rows
            .iter()
            .any(|(_, module)| module.as_deref() == Some("./missing"))
    );
    assert!(
        package
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("./missing"))
    );
    assert!(
        package
            .declarations
            .iter()
            .any(|declaration| declaration.origin == Origin::Computed)
    );
    Ok(())
}
