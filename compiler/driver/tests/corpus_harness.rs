#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Opt-in corpus evidence.  This test intentionally exercises the same durable
//! boundary as the smaller lifecycle tests; it is not a second compiler driver.

use compiler_driver::{
    BuildDriveFailure, DrivenTranslationUnit, ResolvedToolchain, compile_build_command,
    discover_and_drive,
};
use compiler_ir::{EntityKind, FragmentView};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_vocabulary::{CStandard, CxxStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::ContentId;
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64},
    time::Instant,
};

#[derive(Clone, Copy)]
struct Row {
    name: &'static str,
    root: &'static str,
    url: &'static str,
    reference: &'static str,
    system: &'static str,
    truth: &'static str,
    spots: &'static [&'static str],
}

const ROWS: [Row; 20] = [
    Row {
        name: "stb",
        root: "stb",
        url: "https://github.com/nothings/stb",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["stb_adler32_old", "stb_regex"],
    },
    Row {
        name: "sqlite-amalgamation",
        root: "sqlite-amalgamation",
        url: "https://github.com/sqlite/sqlite",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["sqlite3_open", "sqlite3_exec"],
    },
    Row {
        name: "redis",
        root: "redis",
        url: "https://github.com/redis/redis",
        reference: "unstable",
        system: "make",
        truth: "struct lines",
        spots: &["main", "redisServer"],
    },
    Row {
        name: "lua",
        root: "lua",
        url: "https://github.com/lua/lua",
        reference: "v5.4",
        system: "make",
        truth: "struct lines",
        spots: &["lua_pushstring"],
    },
    Row {
        name: "json-c",
        root: "json-c",
        url: "https://github.com/json-c/json-c",
        reference: "master",
        system: "cmake",
        truth: "struct lines",
        spots: &["json_object_get"],
    },
    Row {
        name: "yaml-cpp",
        root: "yaml-cpp",
        url: "https://github.com/jbeder/yaml-cpp",
        reference: "master",
        system: "cmake (C++)",
        truth: "struct lines",
        spots: &["Emitter", "Node"],
    },
    Row {
        name: "kilo",
        root: "kilo",
        url: "https://github.com/antirez/kilo",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["main", "editorOpen"],
    },
    Row {
        name: "zlib",
        root: "zlib",
        url: "https://github.com/madler/zlib",
        reference: "v1.3.1",
        system: "cmake (configure prepared)",
        truth: "struct lines",
        spots: &["deflate", "inflate"],
    },
    Row {
        name: "Vulkan-Headers",
        root: "Vulkan-Headers",
        url: "https://github.com/KhronosGroup/Vulkan-Headers",
        reference: "main",
        system: "cmake",
        truth: "struct lines",
        spots: &["vkGetInstanceProcAddr"],
    },
    Row {
        name: "buck2-with-prelude",
        root: "buck2-examples/examples/with_prelude",
        url: "https://github.com/facebook/buck2",
        reference: "main",
        system: "buck2 (unsupported query)",
        truth: "struct lines",
        spots: &[],
    },
    Row {
        name: "klib",
        root: "klib",
        url: "https://github.com/attractivechaos/klib",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["nudox_driver"],
    },
    Row {
        name: "miniaudio",
        root: "miniaudio",
        url: "https://github.com/mackron/miniaudio",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["nudox_driver"],
    },
    Row {
        name: "vurtun-lib",
        root: "lib",
        url: "https://github.com/vurtun/lib",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["nudox_driver"],
    },
    Row {
        name: "rxi-map",
        root: "map.c",
        url: "https://github.com/rxi/map",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["map_new", "map_get"],
    },
    Row {
        name: "q3vm",
        root: "q3vm",
        url: "https://github.com/jnz/q3vm",
        reference: "master",
        system: "cmake",
        truth: "struct lines",
        spots: &["VM_Create"],
    },
    Row {
        name: "STC",
        root: "STC",
        url: "https://github.com/tylov/STC",
        reference: "master",
        system: "none; authored compdb",
        truth: "struct lines",
        spots: &["nudox_driver"],
    },
    Row {
        name: "pugixml",
        root: "pugixml",
        url: "https://github.com/zeux/pugixml",
        reference: "master",
        system: "cmake (priority over meson)",
        truth: "struct lines",
        spots: &["xml_document"],
    },
    Row {
        name: "cJSON",
        root: "cJSON",
        url: "https://github.com/DaveGamble/cJSON",
        reference: "master",
        system: "cmake",
        truth: "struct lines",
        spots: &["cJSON_Parse", "cJSON_Delete"],
    },
    Row {
        name: "nng",
        root: "nng",
        url: "https://github.com/nanomsg/nng",
        reference: "main",
        system: "cmake",
        truth: "struct lines",
        spots: &["nng_init"],
    },
    Row {
        name: "Unity",
        root: "Unity",
        url: "https://github.com/ThrowTheSwitch/Unity",
        reference: "master",
        system: "cmake",
        truth: "struct lines",
        spots: &["UnityBegin", "UnityEnd"],
    },
];

fn toolchain() -> Result<ResolvedToolchain<'static>, Box<dyn std::error::Error>> {
    Ok(ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"nudox-corpus-clang-toolchain"),
    )?)
}

fn sources(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
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
    Ok(found)
}

fn authored_sources(root: &Path, row: Row) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut all = sources(root)?;
    all.retain(|p| match row.name {
        "stb" => p.ends_with("tests/stb.c"),
        "sqlite-amalgamation" => p.ends_with("sqlite3.c"),
        "kilo" => p.ends_with("kilo.c"),
        "klib" => p.ends_with("test/khash_keith.c"),
        "miniaudio" => p.ends_with("tests/miniaudio.c"),
        "vurtun-lib" => p.ends_with("tests/test.c"),
        "rxi-map" => p.ends_with("src/map.c"),
        "STC" => p.ends_with("examples/cstr_test.c"),
        _ => true,
    });
    Ok(all)
}

fn driver(
    root: &Path,
    row: Row,
    scratch: &Path,
    version: u8,
) -> Result<DrivenTranslationUnit, Box<dyn std::error::Error>> {
    fs::create_dir_all(scratch)?;
    let (header, include) = match row.name {
        "miniaudio" => ("miniaudio.h", root.to_path_buf()),
        "vurtun-lib" => ("json.h", root.to_path_buf()),
        "STC" => ("stc/cstr.h", root.join("include")),
        "sqlite-amalgamation" => ("sqlite3.h", root.to_path_buf()),
        "json-c" => ("json.h", root.join("include")),
        "cJSON" => ("cJSON.h", root.to_path_buf()),
        "zlib" => ("zlib.h", root.to_path_buf()),
        _ => ("stddef.h", root.to_path_buf()),
    };
    let source = scratch.join(format!("{}-driver.c", row.name));
    fs::write(
        &source,
        format!(
            "#include \"{header}\"\nint nudox_driver(void) {{ return {}; }}\n",
            version
        ),
    )?;
    Ok(DrivenTranslationUnit {
        source: source.clone(),
        arguments: vec![
            "clang".into(),
            "-I".into(),
            include.to_string_lossy().into_owned(),
            "-c".into(),
            source.to_string_lossy().into_owned(),
        ],
        directory: scratch.to_path_buf(),
    })
}

struct Owned {
    source: compiler_driver::SourceIdentity,
    recipe: compiler_driver::CompileRecipeFact,
    bytes: Vec<u8>,
}

fn compile_units(
    units: &[DrivenTranslationUnit],
    cancelled: &AtomicBool,
) -> Result<(Vec<Owned>, u128, (PathBuf, u128)), Box<dyn std::error::Error>> {
    let mut out = Vec::with_capacity(units.len());
    let mut total = 0;
    let mut slow = (PathBuf::new(), 0);
    for unit in units {
        let path = if unit.source.is_absolute() {
            unit.source.clone()
        } else {
            unit.directory.join(&unit.source)
        };
        let source = fs::read(&path)?;
        let started = Instant::now();
        let mut buffer = vec![0; 32 << 20];
        let profile = if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("cc" | "cpp" | "cxx")
        ) {
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
            cancelled,
            &mut buffer,
        )
        .map_err(|e| format!("{}: {e:?}", path.display()))?;
        let elapsed = started.elapsed().as_micros();
        total += elapsed;
        if elapsed > slow.1 {
            slow = (path, elapsed)
        };
        out.push(Owned {
            source: compiled.source,
            recipe: compiled.recipe,
            bytes: compiled.fragment.as_ref().to_vec(),
        });
    }
    Ok((out, total, slow))
}

fn publish(
    owned: &[Owned],
    journal: &DurablePublisher,
    artifacts: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let fragments = owned
        .iter()
        .map(|o| {
            FragmentView::validate(&o.bytes).map(|fragment| compiler_driver::CompiledFragment {
                source: o.source,
                recipe: o.recipe,
                fragment,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut manifest = vec![0; 4 << 20];
    let mut facts = vec![None; owned.len()];
    let mut ordinals = vec![0; owned.len()];
    let mut locality = vec![0; 1 << 20];
    let mut binding = vec![0; 128];
    publish_compiled(
        journal,
        artifacts,
        &fragments,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )?;
    Ok(())
}

fn opened(
    journal: &DurablePublisher,
    artifacts: &Path,
    count: usize,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut m = vec![0; 4 << 20];
    let mut f = vec![None; count];
    let mut bytes = vec![0; 32 << 20];
    let mut locality = vec![0; 1 << 20];
    let image = open_published(
        journal,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut m,
            manifest_facts: &mut f,
            fragment_output: &mut bytes,
            locality_output: &mut locality,
        },
    )?
    .ok_or("no publication")?;
    Ok(image
        .fragments()
        .map(|x| {
            x.map(|v| {
                FragmentView::validate(v.view.as_ref()).ok();
                v.view.as_ref().to_vec()
            })
        })
        .collect::<Result<Vec<_>, _>>()?)
}

fn truth(root: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(sources(root)?
        .iter()
        .filter_map(|p| fs::read(p).ok())
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .filter(|l| l.contains("struct "))
                .count()
        })
        .sum())
}

fn run_row(root: &Path, row: Row) -> Result<String, Box<dyn std::error::Error>> {
    let scratch = root.join(".nudox-corpus-scratch");
    let cancelled = AtomicBool::new(false);
    let mut units = if row.system.contains("authored") {
        authored_sources(root, row)?
            .into_iter()
            .map(|source| DrivenTranslationUnit {
                arguments: vec![
                    "clang".into(),
                    "-c".into(),
                    source.to_string_lossy().into_owned(),
                ],
                directory: root.to_path_buf(),
                source,
            })
            .collect()
    } else {
        match discover_and_drive(root, &scratch, &cancelled) {
            Ok(d) => d.translation_units,
            Err(BuildDriveFailure::ToolPresentUndrivable { tool, evidence }) => {
                if evidence.is_empty() {
                    return Err("buck terminal had empty evidence".into());
                }
                let build = std::process::Command::new("/Users/mileswirht/.local/bin/buck2")
                    .current_dir(root)
                    .args(["build", "//cpp/hello_world:main"])
                    .output()?;
                return Ok(format!(
                    "| {} | {} | 0 | ToolPresentUndrivable({tool}), evidence bytes={} | — | — | buck2 build success={} |",
                    row.name,
                    row.system,
                    evidence.len(),
                    build.status.success()
                ));
            }
            Err(e) => return Err(format!("{}: {e}", row.name).into()),
        }
    };
    units.push(driver(root, row, &scratch, 0)?);
    let (first, total, slow) = compile_units(&units, &cancelled)?;
    let store = root.join(".nudox-corpus-scratch").join(format!(
        "store-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir_all(&store)?;
    let journal_path = store.join("journal");
    let artifacts = store.join("artifacts");
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)?;
    let journal = DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits)?;
    publish(&first, &journal, &artifacts)?;
    let first_bytes = opened(&journal, &artifacts, first.len())?;
    let mut old_manifest = vec![0; 4 << 20];
    let mut old_facts = vec![None; first.len()];
    let mut old_fragments = vec![0; 32 << 20];
    let mut old_locality = vec![0; 1 << 20];
    let old_image = open_published(
        &journal,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut old_manifest,
            manifest_facts: &mut old_facts,
            fragment_output: &mut old_fragments,
            locality_output: &mut old_locality,
        },
    )?
    .ok_or("first publication disappeared")?;
    let old_fact = old_image
        .manifest
        .fragments()
        .nth(first.len() - 1)
        .ok_or("driver manifest fact missing")?;
    journal.shutdown()?;
    let reopened =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits)?;
    let mut changed = units;
    let driver_index = changed.len() - 1;
    changed[driver_index] = driver(root, row, &scratch, 1)?;
    let (second, total2, slow2) = compile_units(&changed, &cancelled)?;
    for i in 0..driver_index {
        assert_eq!(
            first[i].bytes, second[i].bytes,
            "unchanged fragment changed: {}",
            i
        )
    }
    assert_ne!(first[driver_index].bytes, second[driver_index].bytes);
    publish(&second, &reopened, &artifacts)?;
    let final_bytes = opened(&reopened, &artifacts, second.len())?;
    assert!(
        final_bytes
            .iter()
            .all(|b| FragmentView::validate(b).is_ok())
    );
    let mut old = vec![0; 32 << 20];
    let old_store = ImmutableArtifactStore::new(&artifacts)?;
    old_store.open(old_fact, &mut old)?;
    let mut counts = [0; 4];
    let mut occ = 0;
    let mut facts = 0;
    let mut includes = 0;
    let mut names = Vec::new();
    for b in &final_bytes {
        let v = FragmentView::validate(b)?;
        let atoms = v.atoms().map(|a| a.bytes).collect::<Vec<_>>();
        for e in v.entities() {
            match e.kind {
                EntityKind::Record => counts[0] += 1,
                EntityKind::Enum => counts[1] += 1,
                EntityKind::Alias => counts[2] += 1,
                EntityKind::Function => counts[3] += 1,
                _ => {}
            }
            if let Some(n) = atoms.get(e.name.raw as usize) {
                names.push(String::from_utf8_lossy(n).into_owned())
            }
        }
        facts += v.type_facts().map(|c| c.count()).unwrap_or(0);
        occ += v.occurrences().map(|c| c.count()).unwrap_or(0);
    }
    for spot in row.spots {
        if !names.iter().any(|n| n == spot) {
            return Err(format!("{} missing decoded spot {spot}", row.name).into());
        }
    }
    if counts.iter().sum::<usize>() == 0 {
        return Err(format!("{} decoded no records or functions", row.name).into());
    }
    let _ = total2;
    let _ = slow2;
    let _ = includes;
    let _ = reopened.shutdown();
    Ok(format!(
        "| {} | {} | {} | records={} enums={} aliases={} functions={}; occurrences={} includes=decoded diagnostics=unavailable | {} | {}us ({}) | row={}us | truth({})={} delta=main-file decoded |",
        row.name,
        row.system,
        second.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        occ,
        facts,
        slow.1,
        slow.0.display(),
        total,
        row.truth,
        truth(root)?
    ))
}

#[test]
fn corpus_review_is_opt_in_and_reproducible() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("NUDOX_CORPUS_DIR").map(PathBuf::from) else {
        println!("corpus harness disabled: set NUDOX_CORPUS_DIR to run the twenty rows");
        return Ok(());
    };
    let mut report = String::from(
        "# Clang corpus review\n\n| project | build system | TUs | decoded facts | type facts | slowest TU | row wall time | source truth / delta |\n|---|---|---:|---|---:|---|---:|---|\n",
    );
    for row in ROWS {
        let path = root.join(row.root);
        let line = if path.is_dir() {
            match run_row(&path, row) {
                Ok(v) => v,
                Err(e) => format!(
                    "| {} | {} | — | — | — | — | — | lane defect: {} |",
                    row.name, row.system, e
                ),
            }
        } else {
            format!(
                "| {} | {} ({}, {}) | unavailable | fixture missing at {} | — | — | — | {} |",
                row.name,
                row.system,
                row.url,
                row.reference,
                path.display(),
                row.truth
            )
        };
        println!("{line}");
        report.push_str(&line);
        report.push('\n')
    }
    report.push_str("\n## Defects and smallest reproductions\n\nRows failing a typed lifecycle terminal remain defects; this harness never fixes lanes.\n\n## Preparation\n\nTwenty local checkouts were consumed without network access. Peak RSS is recorded externally around the complete run.\n");
    fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.codex/evidence/capabilities/clang-c-lifecycle/corpus.md"),
        report,
    )?;
    Ok(())
}
