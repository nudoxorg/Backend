//! Lower a downloaded npm corpus (see `.config/scripts/npm-popular-census.py`) through
//! the real TypeScript producer and write one JSON object per package.
//!
//! Not part of the default suite: it needs a corpus on disk from
//! `.config/scripts/npm-popular-census.py`.
//!
//! ```text
//! NUDOX_NPM_CORPUS=/tmp/npm-corpus \
//!   cargo test -p nudox-languages --test typescript_popular_census -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::entry::{EntryInner, SourceLocation, Visibility};
use nudox_ir::foreign::Unlinked;
use nudox_languages::typescript::TypescriptProducer;
use nudox_languages::{PackageSource, produce};
use serde_json::{Value, json};

#[test]
#[ignore]
fn census_popular_npm_packages() {
    let root = std::env::var("NUDOX_NPM_CORPUS").unwrap_or_else(|_| "/tmp/npm-corpus".to_owned());
    let manifest_path = PathBuf::from(&root).join("manifest.json");
    let manifest: Vec<Value> = serde_json::from_str(
        &fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
            panic!(
                "read {}: {e}. Run .config/scripts/npm-popular-census.py first.",
                manifest_path.display()
            )
        }),
    )
    .expect("manifest json");

    let only = std::env::var("NUDOX_NPM_ONLY").ok();
    let out_path = PathBuf::from(&root).join(if only.is_some() {
        "ir-census-retry.jsonl"
    } else {
        "ir-census.jsonl"
    });
    let mut out = String::new();
    for (i, row) in manifest.iter().enumerate() {
        let name = row["name"].as_str().unwrap();
        if let Some(filter) = only.as_deref() {
            if !filter.split(',').any(|part| part == name) {
                continue;
            }
        }
        let version = row["version"].as_str().unwrap();
        let dir = PathBuf::from(&root).join(row["dir"].as_str().unwrap());
        eprintln!("[{}/{}] {name}@{version}", i + 1, manifest.len());
        let record = if dir.join("package.json").is_file() {
            lower_one(&dir, name, version)
        } else {
            json!({
                "name": name,
                "version": version,
                "ok": false,
                "error": "missing checkout",
            })
        };
        out.push_str(&record.to_string());
        out.push('\n');
        fs::write(&out_path, &out).expect("write census");
    }
    eprintln!("wrote {}", out_path.display());
}

fn error_chain(error: &impl std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(next) = source {
        parts.push(next.to_string());
        source = next.source();
    }
    parts.join(" | ")
}

fn lower_one(root: &Path, name: &str, version: &str) -> Value {
    let started = Instant::now();
    let src = PackageSource::new(root, name, version);
    let lineage = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new(name));
    match produce(&TypescriptProducer::new(), &src, &lineage, &Unlinked) {
        Ok(produced) => {
            let table = &produced.table;
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut visibility: BTreeMap<String, usize> = BTreeMap::new();
            let mut locations = BTreeMap::from([
                ("declared".to_owned(), 0),
                ("bytes_only".to_owned(), 0),
                ("unlocated".to_owned(), 0),
            ]);
            let mut contributed = 0usize;
            let mut public_non_module = 0usize;
            let mut empty_names = 0usize;
            let mut documented = 0usize;
            let mut references = 0usize;
            let mut public_names: Vec<String> = Vec::new();
            for (id, entry) in table.iter() {
                if table.parent_of(id).is_none() {
                    continue;
                }
                contributed += 1;
                let vis = format!("{:?}", entry.sym().visibility);
                *visibility.entry(vis).or_default() += 1;
                match entry.location() {
                    SourceLocation::Declared { .. } => *locations.get_mut("declared").unwrap() += 1,
                    SourceLocation::BytesOnly { .. } => {
                        *locations.get_mut("bytes_only").unwrap() += 1
                    }
                    SourceLocation::Unlocated(_) => *locations.get_mut("unlocated").unwrap() += 1,
                }
                if entry.sym().name.is_empty() {
                    empty_names += 1;
                }
                if !entry.sym().documentation.is_empty() {
                    documented += 1;
                }
                match entry.kind() {
                    EntryInner::Owned(kind) => {
                        let label = format!("{:?}", kind.discriminant());
                        *kinds.entry(label).or_default() += 1;
                        let is_module = matches!(kind, nudox_ir::kind::Kind::Module(_));
                        if entry.sym().visibility == Visibility::Public && !is_module {
                            public_non_module += 1;
                            if public_names.len() < 12 {
                                public_names.push(entry.sym().name.clone());
                            }
                        }
                    }
                    EntryInner::Reference(_) => {
                        references += 1;
                        *kinds.entry("Reference".to_owned()).or_default() += 1;
                    }
                }
            }
            json!({
                "name": name,
                "version": version,
                "ok": true,
                "wall_ms": started.elapsed().as_millis(),
                "contract": format!("{:?}", produced.contract),
                "contributed": contributed,
                "public_non_module": public_non_module,
                "empty_names": empty_names,
                "documented": documented,
                "reference_entries": references,
                "occurrences": produced.occurrences.len(),
                "bodies": produced.bodies.len(),
                "source_excerpts": produced.source.len(),
                "source_issues": produced.source_issues.len(),
                "kinds": kinds,
                "visibility": visibility,
                "locations": locations,
                "public_sample": public_names,
            })
        }
        Err(error) => json!({
            "name": name,
            "version": version,
            "ok": false,
            "wall_ms": started.elapsed().as_millis(),
            "error": error_chain(&error),
        }),
    }
}
