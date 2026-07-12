//! Pipeline part: **mini flake → surface IR** via static + best-effort dynamic.
//!
//! The fixture at `tests/fixtures/nix/mini-flake/` is a self-contained flake
//! with no inputs. Dynamic evaluation via snix may succeed or degrade; the
//! test accepts **static-only** success as long as the attrpath surface for
//! packages / lib / nixosModules is visible from the static layer.

use std::path::{Path, PathBuf};

use compiler::languages::nix::lower_package;
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nix/mini-flake")
}

fn dump(index: &Index) -> String {
    let mut v: Vec<String> = index
        .entries_by_path
        .iter()
        .map(|(p, e)| format!("{}:{}", e.kind_tag(), path_display(p)))
        .collect();
    v.sort();
    v.join(", ")
}

fn path_display(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.display().to_string(),
        NudoxPath::External { path, dependency } => {
            format!("{dependency}:{}", path.display())
        }
    }
}

fn any_path_prefix(index: &Index, prefix: &str) -> bool {
    index.entries_by_path.keys().any(|p| {
        let s = path_display(p);
        s == prefix || s.starts_with(&format!("{prefix}/"))
    })
}

fn has_named(index: &Index, name: &str) -> bool {
    index.entries_by_path.values().any(|e| e.name() == name)
}

#[test]
fn lower_mini_flake_root_and_output_surface() {
    let root = fixture_root();
    assert!(
        root.join("flake.nix").is_file(),
        "fixture missing: {}",
        root.display()
    );

    // Dynamic eval is best-effort: a snix failure must not fail this test.
    let index = lower_package(Path::new(&root)).expect("static layer must always succeed");
    println!("entries: {}", dump(&index));

    // --- Root module ------------------------------------------------------
    assert!(
        !index.root_ids.is_empty(),
        "expected at least one root id (flake module / builtins)"
    );
    let has_root_module = index.entries_by_path.values().any(|e| {
        matches!(e, Entry::Module(sym) if sym.visibility == ir::kind::Visibility::Public)
    });
    assert!(has_root_module, "expected a public Module entry; {}", dump(&index));

    // --- Static surface of the flake outputs body -------------------------
    // Inside `outputs = { self, ... }: { … }`, attrpaths are recovered without
    // an `outputs/` prefix (enclosing lambda is not an AttrpathValue).
    assert!(
        any_path_prefix(&index, "lib") || has_named(&index, "double"),
        "expected lib.double surface; entries: {}",
        dump(&index)
    );
    assert!(
        any_path_prefix(&index, "packages") || has_named(&index, "hello"),
        "expected packages.*.hello surface; entries: {}",
        dump(&index)
    );
    assert!(
        any_path_prefix(&index, "nixosModules") || has_named(&index, "default"),
        "expected nixosModules.default surface; entries: {}",
        dump(&index)
    );

    // lib.double is a documented lambda — must lower as Function with a type.
    if let Some(Entry::Function(sym)) = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "double")
    {
        assert!(
            sym.documentation.as_ref().is_some_and(|d| !d.is_empty())
                || sym.inner.output_parameters.is_some(),
            "double should carry RFC-145 docs or a typed return"
        );
        if let Some(outs) = &sym.inner.output_parameters {
            assert!(
                !outs.is_empty(),
                "double :: Int -> Int should produce an output parameter"
            );
        }
    }

    // Builtins synthetic package is always present.
    assert!(
        any_path_prefix(&index, "builtins"),
        "expected synthetic builtins module; entries: {}",
        dump(&index)
    );
}
