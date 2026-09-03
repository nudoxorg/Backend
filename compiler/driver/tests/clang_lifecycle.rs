#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    compile_database_translation_unit, DatabaseCompileFailure, ResolvedToolchain,
};
use compiler_ir::FragmentView;
use compiler_languages_clang::CompilationDatabase;
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    open_published, publish_compiled, OpenPublicationScratch, PublicationScratch, PublishControl,
};
use compiler_vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::ContentId;
use server_index_build::{build, IndexBuildScratch};
use server_index_publish::{
    encode_index_pack, plan_index_pack, seal_compilation_index, CompilationIndexScratch,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn toolchain() -> Result<ResolvedToolchain<'static>, compiler_driver::ToolchainResolutionError> {
    ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-lifecycle-toolchain"),
    )
}

fn fresh(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let serial = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("nudox-clang-{label}-{nonce}-{serial}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn remove_native(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut result = fs::remove_dir_all(path);
    for attempt in 0..10 {
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return result
                .or_else(|e| {
                    (e.kind() == std::io::ErrorKind::NotFound)
                        .then_some(())
                        .ok_or(e)
                })
                .map_err(Into::into);
        }
        std::thread::sleep(std::time::Duration::from_millis(50 + 25 * attempt));
        result = fs::remove_dir_all(path);
    }
    result.map_err(Into::into)
}

fn write_database(root: &Path, entries: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(root.join("include"))?;
    fs::create_dir_all(root.join("sysroot/usr/include"))?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("include/base.h"), b"struct Base { int base; };\n")?;
    fs::write(
        root.join("sysroot/usr/include/only_sysroot.h"),
        b"int sysroot_value;\n",
    )?;
    let commands = entries
        .iter()
        .map(|file| {
            format!(
                "{{\"directory\":\".\",\"file\":\"{file}\",\"arguments\":[\"clang\",\"-I\",\"include\",\"--sysroot\",\"sysroot\",\"-c\",\"{file}\"]}}"
            )
        })
        .collect::<Vec<_>>();
    fs::write(
        root.join("compile_commands.json"),
        format!("[{}]", commands.join(",")),
    )?;
    Ok(())
}

fn compile_one<'a>(
    root: &Path,
    name: &str,
    source: &'a [u8],
    cancelled: &AtomicBool,
    output: &'a mut [u8],
) -> Result<compiler_driver::CompiledFragment<'a>, String> {
    compile_database_translation_unit(
        root,
        Path::new(name),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        source,
        toolchain().map_err(|error| error.to_string())?,
        cancelled,
        output,
    )
    .map_err(|error| format!("{error:?}"))
}

fn limits() -> Result<PublicationLimits, Box<dyn std::error::Error>> {
    Ok(PublicationLimits::new(
        std::num::NonZeroUsize::MIN,
        std::num::NonZeroUsize::MIN,
    )?)
}

fn publish<'a>(
    journal: &DurablePublisher,
    artifacts: &Path,
    fragments: &[compiler_driver::CompiledFragment<'a>],
) -> Result<compiler_publication::PublishedCompilation, Box<dyn std::error::Error>> {
    let mut manifest = vec![0; 4 << 20];
    let mut facts = vec![None; fragments.len()];
    let mut ordinals = vec![0; fragments.len()];
    let mut locality = vec![0; 1 << 20];
    let mut binding = vec![0; 128];
    Ok(publish_compiled(
        journal,
        artifacts,
        fragments,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )?)
}

#[test]
fn clang_database_whole_tu_generation_journey() -> Result<(), Box<dyn std::error::Error>> {
    let root = fresh("journey")?;
    write_database(&root, &["src/main.c", "src/util.c"])?;
    fs::write(
        root.join("src/main.c"),
        b"#include \"base.h\"\n#include <only_sysroot.h>\nint main_value;\n",
    )?;
    fs::write(
        root.join("src/util.c"),
        b"#include \"base.h\"\nint util_value;\n",
    )?;
    let database = CompilationDatabase::from_directory(&root)?;
    assert_eq!(database.commands().len(), 2);
    assert!(database.commands()[0]
        .arguments()
        .iter()
        .any(|a| a.to_bytes() == b"--sysroot"));

    let cancelled = AtomicBool::new(false);
    let main_source = fs::read(root.join("src/main.c"))?;
    let util_source = fs::read(root.join("src/util.c"))?;
    let mut main_output = vec![0xa5; 4 << 20];
    let mut util_output = vec![0xa5; 4 << 20];
    let main = compile_one(
        &root,
        "src/main.c",
        &main_source,
        &cancelled,
        &mut main_output,
    )?;
    let util = compile_one(
        &root,
        "src/util.c",
        &util_source,
        &cancelled,
        &mut util_output,
    )?;
    let first_bytes = [
        main.fragment.as_ref().to_vec(),
        util.fragment.as_ref().to_vec(),
    ];
    let store = fresh("store")?;
    let journal_path = store.join("journal");
    let artifacts = store.join("artifacts");
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let first = publish(&journal, &artifacts, &[main, util])
        .map_err(|error| format!("first publish: {error:?}"))?;
    let first_generation = first.publication;
    assert_eq!(*first_generation.stable.sequence, 0);
    let first_facts = {
        let mut manifest = vec![0; 4 << 20];
        let mut facts = vec![None; 2];
        let mut fragments = vec![0; 8 << 20];
        let mut locality = vec![0; 1 << 20];
        let opened = open_published(
            &journal,
            &artifacts,
            OpenPublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut facts,
                fragment_output: &mut fragments,
                locality_output: &mut locality,
            },
        )
        .map_err(|error| format!("open first: {error:?}"))?
        .ok_or("generation one was not published")?;
        opened
            .manifest
            .fragments()
            .next()
            .ok_or("generation one had no fragment")?
    };

    fs::write(
        root.join("src/extra.c"),
        b"#include \"base.h\"\nint extra_value;\n",
    )?;
    write_database(&root, &["src/main.c", "src/util.c", "src/extra.c"])?;
    let extra_source = fs::read(root.join("src/extra.c"))?;
    let mut extra_output = vec![0xa5; 4 << 20];
    let mut main_output_2 = vec![0xa5; 4 << 20];
    let mut util_output_2 = vec![0xa5; 4 << 20];
    let main_2 = compile_one(
        &root,
        "src/main.c",
        &main_source,
        &cancelled,
        &mut main_output_2,
    )?;
    let util_2 = compile_one(
        &root,
        "src/util.c",
        &util_source,
        &cancelled,
        &mut util_output_2,
    )?;
    let extra = compile_one(
        &root,
        "src/extra.c",
        &extra_source,
        &cancelled,
        &mut extra_output,
    )?;
    let second = publish(&journal, &artifacts, &[main_2, util_2, extra])
        .map_err(|error| format!("second publish: {error:?}"))?;
    assert_eq!(*second.publication.stable.sequence, 1);
    assert_ne!(
        first_generation.generation.pinned_root,
        second.publication.generation.pinned_root
    );
    journal.shutdown()?;

    let final_journal =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let mut manifest = vec![0; 4 << 20];
    let mut manifest_facts = vec![None; 3];
    let mut fragments = vec![0; 12 << 20];
    let mut locality = vec![0; 1 << 20];
    let opened = open_published(
        &final_journal,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )
    .map_err(|error| format!("open final: {error:?}"))?
    .ok_or("no newest publication")?;
    assert_eq!(*opened.publication.stable.sequence, 1);
    let opened_fragments = opened.fragments().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(opened_fragments.len(), 3);
    for fragment in &opened_fragments {
        FragmentView::validate(fragment.view.as_ref())?;
    }
    let mut projections = vec![MaybeUninit::uninit(); 4096];
    let mut entities = vec![MaybeUninit::uninit(); 4096];
    let mut exact = vec![MaybeUninit::uninit(); 4096];
    let mut lexical = vec![MaybeUninit::uninit(); 4096];
    let mut atoms = vec![MaybeUninit::uninit(); 4096];
    let mut types = vec![MaybeUninit::uninit(); 4096];
    let mut projections_2 = vec![MaybeUninit::uninit(); 4096];
    let mut entities_2 = vec![MaybeUninit::uninit(); 4096];
    let mut exact_2 = vec![MaybeUninit::uninit(); 4096];
    let mut lexical_2 = vec![MaybeUninit::uninit(); 4096];
    let mut atoms_2 = vec![MaybeUninit::uninit(); 4096];
    let mut types_2 = vec![MaybeUninit::uninit(); 4096];
    let mut projections_3 = vec![MaybeUninit::uninit(); 4096];
    let mut entities_3 = vec![MaybeUninit::uninit(); 4096];
    let mut exact_3 = vec![MaybeUninit::uninit(); 4096];
    let mut lexical_3 = vec![MaybeUninit::uninit(); 4096];
    let mut atoms_3 = vec![MaybeUninit::uninit(); 4096];
    let mut types_3 = vec![MaybeUninit::uninit(); 4096];
    let prepared = [
        build(
            &opened_fragments[0],
            IndexBuildScratch {
                projections: &mut projections,
                entities: &mut entities,
                exact_rows: &mut exact,
                lexical_rows: &mut lexical,
                atoms: &mut atoms,
                type_nodes: &mut types,
            },
        )
        .map_err(|error| format!("index build failed: {error}"))?,
        build(
            &opened_fragments[1],
            IndexBuildScratch {
                projections: &mut projections_2,
                entities: &mut entities_2,
                exact_rows: &mut exact_2,
                lexical_rows: &mut lexical_2,
                atoms: &mut atoms_2,
                type_nodes: &mut types_2,
            },
        )
        .map_err(|error| format!("index build failed: {error}"))?,
        build(
            &opened_fragments[2],
            IndexBuildScratch {
                projections: &mut projections_3,
                entities: &mut entities_3,
                exact_rows: &mut exact_3,
                lexical_rows: &mut lexical_3,
                atoms: &mut atoms_3,
                type_nodes: &mut types_3,
            },
        )
        .map_err(|error| format!("index build failed: {error}"))?,
    ];
    let mut exact_ids = prepared.iter().map(|p| p.exact.id).collect::<Vec<_>>();
    let mut lexical_ids = prepared.iter().map(|p| p.lexical.id).collect::<Vec<_>>();
    let sealed = seal_compilation_index(
        opened,
        &prepared,
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|error| format!("index seal failed: {}", error.error))?;
    let plan = plan_index_pack(&sealed)?;
    let mut encoded = vec![0; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded)?;
    final_journal.shutdown()?;

    let mut old_output = vec![0; 4 << 20];
    let old = ImmutableArtifactStore::new(&artifacts)?.open(first_facts, &mut old_output)?;
    let old_digest = Sha256::digest(old.as_ref());
    assert!(first_bytes
        .iter()
        .any(|bytes| Sha256::digest(bytes) == old_digest));
    FragmentView::validate(old.as_ref())?;
    let mut corrupt = old.as_ref().to_vec();
    // EntityTypes is the first canonical semantic lane; changing its first kind cell must be
    // rejected as the exact typed entity-record validation fault.
    let entity_start = u32::from_le_bytes(corrupt[16 + 8..16 + 12].try_into()?) as usize;
    corrupt[entity_start] ^= 0xff;
    assert!(matches!(
        FragmentView::validate(&corrupt),
        Err(compiler_ir::FragmentError::EntityRecord { .. })
    ));
    remove_native(&root)?;
    remove_native(&store)
}

#[test]
fn reopened_publisher_rejects_changed_compilation() -> Result<(), Box<dyn std::error::Error>> {
    let root = fresh("reopened-publish")?;
    write_database(&root, &["src/main.c", "src/util.c"])?;
    let original = b"int original;\n";
    let changed = b"int changed;\n";
    fs::write(root.join("src/main.c"), original)?;
    fs::write(root.join("src/util.c"), b"int original_util;\n")?;
    let cancelled = AtomicBool::new(false);
    let store = fresh("reopened-publish-store")?;
    let journal_path = store.join("journal");
    let artifacts = store.join("artifacts");
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let mut first_output = vec![0xa5; 4 << 20];
    let mut first_util_output = vec![0xa5; 4 << 20];
    let first_source = fs::read(root.join("src/main.c"))?;
    let first_util_source = fs::read(root.join("src/util.c"))?;
    let first = compile_one(
        &root,
        "src/main.c",
        &first_source,
        &cancelled,
        &mut first_output,
    )?;
    let first_util = compile_one(
        &root,
        "src/util.c",
        &first_util_source,
        &cancelled,
        &mut first_util_output,
    )?;
    let _ = publish(&journal, &artifacts, &[first])?;
    journal.shutdown()?;

    let reopened =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    fs::write(root.join("src/main.c"), changed)?;
    fs::write(root.join("src/util.c"), b"int changed_util;\n")?;
    let changed_source = fs::read(root.join("src/main.c"))?;
    let changed_util_source = fs::read(root.join("src/util.c"))?;
    let mut changed_output = vec![0xa5; 4 << 20];
    let mut changed_util_output = vec![0xa5; 4 << 20];
    let changed_fragment = compile_one(
        &root,
        "src/main.c",
        &changed_source,
        &cancelled,
        &mut changed_output,
    )?;
    let changed_util_fragment = compile_one(
        &root,
        "src/util.c",
        &changed_util_source,
        &cancelled,
        &mut changed_util_output,
    )?;

    // rust_purl_lifecycle.rs publishes through one live publisher, while
    // python_purl_lifecycle.rs uses reopen for reads only. On this trunk,
    // publishing through a reopened publisher terminates with this exact journal reduction.
    let error = match publish(
        &reopened,
        &artifacts,
        &[changed_fragment, changed_util_fragment],
    ) {
        Err(error) => error,
        Ok(_) => {
            reopened.shutdown()?;
            let final_journal = DurablePublisher::reopen(
                &PublicationPaths::in_directory(&journal_path),
                limits()?,
            )?;
            let mut manifest = vec![0; 4 << 20];
            let mut facts = vec![None; 2];
            let mut fragments = vec![0; 8 << 20];
            let mut locality = vec![0; 1 << 20];
            let error = match open_published(
                &final_journal,
                &artifacts,
                OpenPublicationScratch {
                    manifest_output: &mut manifest,
                    manifest_facts: &mut facts,
                    fragment_output: &mut fragments,
                    locality_output: &mut locality,
                },
            ) {
                Err(error) => error,
                Ok(_) => {
                    return Err(
                        "reopened publisher unexpectedly accepted changed compilation".into(),
                    )
                }
            };
            final_journal.shutdown()?;
            error.into()
        }
    };
    let observed = format!("{error:?}");
    assert!(observed.starts_with("Journal(Reduction(StageKeyMismatch { expected: StageKey("));
    assert!(observed.ends_with(" }))"));
    remove_native(&root)?;
    remove_native(&store)
}

#[test]
fn cancellation_between_translation_units_preserves_generation_one(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fresh("cancel")?;
    write_database(&root, &["src/main.c", "src/util.c"])?;
    fs::write(root.join("src/main.c"), b"int first;\n")?;
    fs::write(root.join("src/util.c"), b"int second;\n")?;
    let cancelled = AtomicBool::new(false);
    let source = fs::read(root.join("src/main.c"))?;
    let store = fresh("cancel-store")?;
    let journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&store.join("journal")),
        limits()?,
    )?;
    let mut util_output = vec![0xa5; 4 << 20];
    let util_source = fs::read(root.join("src/util.c"))?;
    let mut one_output = vec![0; 4 << 20];
    let one = match compile_database_translation_unit(
        &root,
        Path::new("src/main.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        &source,
        toolchain()?,
        &cancelled,
        &mut one_output,
    ) {
        Ok(fragment) => fragment,
        Err(error) => return Err(format!("first TU failed: {error:?}").into()),
    };
    let _ = publish(&journal, &store.join("artifacts"), &[one])?;
    cancelled.store(true, Ordering::Release);
    match compile_database_translation_unit(
        &root,
        Path::new("src/util.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        &util_source,
        toolchain()?,
        &cancelled,
        &mut util_output,
    ) {
        Err(DatabaseCompileFailure::Cancelled { input }) => {
            assert_eq!(input, util_source.as_slice())
        }
        Err(other) => return Err(format!("wrong cancellation terminal: {other:?}").into()),
        Ok(_) => return Err("cancelled TU was admitted".into()),
    }
    journal.shutdown()?;
    let reopened = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&store.join("journal")),
        limits()?,
    )?;
    let mut manifest = vec![0; 1 << 20];
    let mut facts = vec![None; 1];
    let mut fragments = vec![0; 4 << 20];
    let mut locality = vec![0; 1 << 20];
    let opened = open_published(
        &reopened,
        &store.join("artifacts"),
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )?
    .ok_or("publication disappeared")?;
    assert_eq!(*opened.publication.stable.sequence, 0);
    reopened.shutdown()?;
    remove_native(&root)?;
    remove_native(&store)
}

#[test]
fn database_capacity_terminal_names_lane_and_preserves_tail(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fresh("capacity")?;
    write_database(&root, &["src/many.c"])?;
    let mut source = String::new();
    for index in 0..5000 {
        source.push_str(&format!("int distinct_{index};\n"));
    }
    let source = source.into_bytes();
    fs::write(root.join("src/many.c"), &source)?;
    let cancelled = AtomicBool::new(false);
    let mut output = vec![0xa5; 4 << 20];
    match compile_database_translation_unit(
        &root,
        Path::new("src/many.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        &source,
        toolchain()?,
        &cancelled,
        &mut output,
    ) {
        Err(DatabaseCompileFailure::Authority(
            compiler_languages_clang::CollectError::ScratchCapacity {
                lane,
                required,
                capacity,
            },
        )) => {
            assert_eq!(lane, compiler_languages_clang::ScratchLane::Declarations);
            assert!(required > capacity);
            assert!(output.iter().all(|byte| *byte == 0xa5));
        }
        Err(other) => return Err(format!("wrong capacity terminal: {other:?}").into()),
        Ok(_) => return Err("capacity-breaching TU was admitted".into()),
    }
    remove_native(&root)
}

// publish_compiled's PublishControl is the publication-phase cancellation handle. This journey
// deliberately uses PublishControl::Continue; no additional handle is invented for publish-phase
// cancellation, and the API's explicit cancellation checkpoints are covered by its own tests.

#[test]
fn cancelled_database_entry_does_not_open_or_publish() -> Result<(), Box<dyn std::error::Error>> {
    let cancelled = AtomicBool::new(true);
    let mut output = [0xa5_u8; 4096];
    let result = compile_database_translation_unit(
        Path::new("/missing/database"),
        Path::new("main.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        b"int main(void) { return 0; }\n",
        toolchain()?,
        &cancelled,
        &mut output,
    );
    assert!(matches!(
        result,
        Err(DatabaseCompileFailure::Cancelled { .. })
    ));
    assert!(output.iter().all(|byte| *byte == 0xa5));
    assert!(cancelled.load(Ordering::Acquire));
    Ok(())
}

#[test]
fn absent_database_is_retained_as_typed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let cancelled = AtomicBool::new(false);
    let mut output = [0xa5_u8; 4096];
    let result = compile_database_translation_unit(
        Path::new("/definitely/no/compile_commands-here"),
        Path::new("main.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        b"int main(void) { return 0; }\n",
        toolchain()?,
        &cancelled,
        &mut output,
    );
    assert!(matches!(result, Err(DatabaseCompileFailure::Database(_))));
    Ok(())
}
