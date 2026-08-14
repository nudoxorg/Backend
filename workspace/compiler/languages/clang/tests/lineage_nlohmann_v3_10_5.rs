//! `nlohmann-json` is one of two `cpp` corpus packages carrying multiple
//! versions for `nudox-graph` lineage testing (`docs/CORPUS.md`'s
//! multi-version table) — v3.11.3 and v3.10.5. `corpus_sweep.rs` covers one
//! entry per *package* (`nix/corpus.nix`'s 20 `[[packages]]` blocks,
//! matching this ecosystem's 20-package provisioning scope), so it exercises
//! v3.11.3 but not this second version entry. Lineage queries need *both*
//! versions to actually lower, not just the one the main sweep happened to
//! pick, so this file provisions and verifies v3.10.5 on its own — same
//! shim technique as `corpus_sweep.rs` (see that file's module docs for why
//! a same-directory `.cpp` twin is what makes a header's own declarations
//! visitable at all), scaled down to the one file this version needs it for.
use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_producer::{PackageSource, produce};
use nudox_producer_clang::ClangProducer;

static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../result")
        .canonicalize()
        .expect("no result/ checkout — see docs/CORPUS.md to (re)provision it")
}

#[test]
fn nlohmann_json_v3_10_5_lineage_pair_also_resolves() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let root = corpus_root().join("nlohmann-json-v3.10.5");
    assert!(
        root.is_dir(),
        "no checkout at {} — run `nix build .#checks.corpus` from the repo root",
        root.display()
    );

    let header = root.join("single_include/nlohmann/json.hpp");
    let bytes = std::fs::read(&header).expect("read json.hpp");
    let twin = root.join("single_include/nlohmann/json.nudox_probe.cpp");
    if std::fs::read(&twin).ok().as_deref() != Some(bytes.as_slice()) {
        std::fs::write(&twin, &bytes).expect("write probe twin");
    }

    let db = serde_json::json!([{
        "directory": root.to_string_lossy(),
        "file": "single_include/nlohmann/json.nudox_probe.cpp",
        "arguments": ["c++", "-Isingle_include", "-std=c++17", "-x", "c++", "-c"],
    }]);
    std::fs::write(
        root.join("compile_commands.json"),
        serde_json::to_string_pretty(&db).unwrap(),
    )
    .expect("write compile_commands.json");

    let src = PackageSource::new(&root, "nlohmann-json", "v3.10.5");
    let lid = PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new("nlohmann-json"));

    let (produced, _cost) = nudox_test_support::measured("cpp-lineage-nlohmann-json-v3.10.5", &root, || {
        produce(&ClangProducer::new(), &src, &lid, &nudox_ir::foreign::Unlinked)
    });

    let intro = produced.unwrap_or_else(|e| {
        let mut chain = e.to_string();
        let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
        while let Some(s) = cur {
            chain.push_str(" <- ");
            chain.push_str(&s.to_string());
            cur = s.source();
        }
        panic!("nlohmann-json v3.10.5 failed to lower: {chain}");
    });

    let names: Vec<&str> = intro.table.iter().map(|(_, e)| e.sym().name.as_str()).collect();
    assert!(
        names.iter().any(|n| *n == "basic_json"),
        "v3.10.5 must lower basic_json same as v3.11.3; got {} entries, sample: {:?}",
        names.len(),
        &names[..names.len().min(20)]
    );
    assert!(
        intro.table.len() > 500,
        "v3.10.5 lowered suspiciously few entries ({}), not real content",
        intro.table.len()
    );
}
