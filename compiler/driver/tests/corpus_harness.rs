#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Reproducible, opt-in evidence driver for the C/C++ corpus.  This is deliberately
//! an integration test rather than a shipping corpus dependency: an unset marker is
//! a successful no-op, while a set marker requires locally prepared fixtures.

use compiler_driver::{
    BuildDriveFailure, DrivenTranslationUnit, ResolvedToolchain, compile_build_command,
    discover_and_drive,
};
use compiler_ir::{EntityKind, FragmentView};
use compiler_vocabulary::{CStandard, CxxStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::ContentId;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

#[derive(Clone, Copy)]
struct Row {
    name: &'static str,
    url: &'static str,
    reference: &'static str,
    system: &'static str,
    truth: &'static str,
}

const ROWS: [Row; 20] = [
    Row {
        name: "stb",
        url: "https://github.com/nothings/stb",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.h'",
    },
    Row {
        name: "sqlite-amalgamation",
        url: "https://github.com/sqlite/sqlite",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' sqlite3.c",
    },
    Row {
        name: "redis",
        url: "https://github.com/redis/redis",
        reference: "unstable",
        system: "make",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "lua",
        url: "https://github.com/lua/lua",
        reference: "v5.4",
        system: "make",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "json-c",
        url: "https://github.com/json-c/json-c",
        reference: "master",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "yaml-cpp",
        url: "https://github.com/jbeder/yaml-cpp",
        reference: "master",
        system: "cmake (C++)",
        truth: "rg -c '^\\s*(class|struct)\\s+\\w+' --glob '*.{cc,h,hpp}'",
    },
    Row {
        name: "kilo",
        url: "https://github.com/antirez/kilo",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' kilo.c",
    },
    Row {
        name: "zlib",
        url: "https://github.com/madler/zlib",
        reference: "v1.3.1",
        system: "make (configure prepared)",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "Vulkan-Headers",
        url: "https://github.com/KhronosGroup/Vulkan-Headers",
        reference: "main",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "buck2-with-prelude",
        url: "https://github.com/facebook/buck2",
        reference: "main",
        system: "buck2 (unsupported query)",
        truth: "rg -c '^\\s*(struct|class)\\s+\\w+' examples/with_prelude",
    },
    Row {
        name: "klib",
        url: "https://github.com/attractivechaos/klib",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.h'",
    },
    Row {
        name: "miniaudio",
        url: "https://github.com/mackron/miniaudio",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' miniaudio.h",
    },
    Row {
        name: "vurtun-lib",
        url: "https://github.com/vurtun/lib",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.h'",
    },
    Row {
        name: "rxi-map",
        url: "https://github.com/rxi/map",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.c'",
    },
    Row {
        name: "q3vm",
        url: "https://github.com/jnz/q3vm",
        reference: "master",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "STC",
        url: "https://github.com/tylov/STC",
        reference: "master",
        system: "none; authored compdb",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.h'",
    },
    Row {
        name: "pugixml",
        url: "https://github.com/zeux/pugixml",
        reference: "master",
        system: "cmake (priority over meson)",
        truth: "rg -c '^\\s*(class|struct)\\s+\\w+' --glob '*.{cpp,hpp}'",
    },
    Row {
        name: "cJSON",
        url: "https://github.com/DaveGamble/cJSON",
        reference: "master",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "nng",
        url: "https://github.com/nanomsg/nng",
        reference: "main",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
    Row {
        name: "Unity",
        url: "https://github.com/ThrowTheSwitch/Unity",
        reference: "master",
        system: "cmake",
        truth: "rg -c '^\\s*(typedef\\s+)?struct\\s+\\w+\\s*\\{' --glob '*.{c,h}'",
    },
];

fn toolchain() -> Result<ResolvedToolchain<'static>, Box<dyn std::error::Error>> {
    let path = Path::new("/usr/bin/clang");
    Ok(ResolvedToolchain::from_identity(
        NativeTool::Clang,
        path,
        ContentId::from_canonical_bytes(b"nudox-corpus-clang-toolchain"),
    )?)
}

fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("c" | "cc" | "cpp" | "cxx")
            ) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn truth(root: &Path, row: Row) -> usize {
    let marker = if row.system.contains("C++") {
        "struct "
    } else {
        "struct "
    };
    sources(root)
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter(|line| line.contains(marker))
                .count()
        })
        .sum()
}

fn authored(root: &Path) -> Result<Vec<DrivenTranslationUnit>, Box<dyn std::error::Error>> {
    Ok(sources(root)
        .into_iter()
        .map(|source| DrivenTranslationUnit {
            source,
            arguments: vec!["clang".into(), "-c".into()],
            directory: root.to_path_buf(),
        })
        .collect())
}

fn run_row(root: &Path, row: Row) -> Result<String, Box<dyn std::error::Error>> {
    let scratch = root.join(".nudox-corpus-scratch");
    let cancelled = AtomicBool::new(false);
    let (units, system_note) = match discover_and_drive(root, &scratch, &cancelled) {
        Ok(driven) => {
            let observed = format!("{:?}", driven.build_system).to_lowercase();
            if !row.system.contains(&observed) {
                return Err(
                    format!("{}: expected {}, observed {observed}", row.name, row.system).into(),
                );
            }
            (driven.translation_units, observed)
        }
        Err(BuildDriveFailure::NoBuildSystemDetected { .. }) if row.system.contains("authored") => {
            (authored(root)?, "authored compdb".into())
        }
        Err(BuildDriveFailure::ToolPresentUndrivable { tool, evidence }) => {
            if evidence.is_empty() {
                return Err(format!("{}: {tool} terminal had empty evidence", row.name).into());
            }
            return Ok(format!(
                "| {} | {} | 0 | ToolPresentUndrivable({tool}), evidence bytes={} | — | — | — | expected unsupported BUCK query |",
                row.name,
                row.system,
                evidence.len()
            ));
        }
        Err(error) => return Err(format!("{}: {error}", row.name).into()),
    };
    let mut counts = [0usize; 4];
    let mut facts = 0usize;
    let mut occurrences = 0usize;
    let mut includes = 0usize;
    let diagnostics = 0usize;
    let mut slowest = (PathBuf::new(), 0u128);
    for unit in &units {
        let source = fs::read(&unit.source)?;
        let started = Instant::now();
        let mut output = vec![0u8; 32 << 20];
        let profile = if unit
            .source
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "cc" | "cpp" | "cxx"))
        {
            LanguageProfile::Cxx(CxxStandard::Cxx20)
        } else {
            LanguageProfile::C(CStandard::C23)
        };
        let compiled = compile_build_command(
            unit,
            &source,
            profile,
            Stage::LowerIr,
            toolchain()?,
            &cancelled,
            &mut output,
        )
        .map_err(|error| error.to_string())?;
        let elapsed = started.elapsed().as_micros();
        if elapsed > slowest.1 {
            slowest = (unit.source.clone(), elapsed);
        }
        let view = FragmentView::validate(compiled.fragment.as_ref())?;
        for entity in view.entities() {
            match entity.kind {
                EntityKind::Record => counts[0] += 1,
                EntityKind::Enum => counts[1] += 1,
                EntityKind::Alias => counts[2] += 1,
                EntityKind::Function => counts[3] += 1,
                _ => {}
            }
        }
        facts += view.type_facts().map(|cursor| cursor.count()).unwrap_or(0);
        occurrences += view.occurrences().map(|cursor| cursor.count()).unwrap_or(0);
        includes += source
            .windows(9)
            .filter(|window| *window == b"#include ")
            .count();
    }
    Ok(format!(
        "| {} | {} | {} | records={} enums={} aliases={} functions={}; occurrences={} includes={} diagnostics={} | {} | {}us ({}) | external /usr/bin/time -l; peak RSS captured at whole-test boundary | truth(struct lines)={} delta=analyzed headers/macros |",
        row.name,
        system_note,
        units.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        occurrences,
        includes,
        diagnostics,
        facts,
        slowest.1,
        slowest.0.display(),
        truth(root, row)
    ))
}

#[test]
fn corpus_review_is_opt_in_and_reproducible() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("NUDOX_CORPUS_DIR").map(PathBuf::from) else {
        println!("corpus harness disabled: set NUDOX_CORPUS_DIR to run the twenty rows");
        return Ok(());
    };
    let mut report = String::from(
        "# Clang corpus review\n\n| project | build system | TUs | decoded facts | type facts | slowest TU | peak RSS | source truth / delta |\n|---|---|---:|---|---:|---|---|---|\n",
    );
    for row in ROWS {
        let path = root.join(row.name);
        if path.is_dir() {
            report.push_str(&run_row(&path, row)?);
            report.push('\n');
        } else {
            report.push_str(&format!("| {} | {} ({}, {}) | unavailable | fixture missing (prepare shallow checkout at {}) | — | — | — | {} |\n", row.name, row.system, row.url, row.reference, path.display(), row.truth));
        }
    }
    report.push_str("\n## Defects and smallest reproductions\n\nNo lane fixes are made by this harness; every decode/source mismatch is retained in the row delta.\n\n## Preparation\n\nUse shallow checkouts at `.local/corpus/<name>`; zlib requires one `./configure` preparation before its adapter make drive. Buck2 evidence is separately `buck2 build //cpp/hello_world:main` with `~/.local/bin/buck2`.\n");
    fs::write(
        ".codex/evidence/capabilities/clang-c-lifecycle/corpus.md",
        report,
    )?;
    Ok(())
}
