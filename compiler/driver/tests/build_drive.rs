#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    BuildDriveFailure, DrivenTranslationUnit, ResolvedToolchain, compile_build_command,
    discover_and_drive,
};
use compiler_ir::FragmentView;
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::ContentId;
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::Path,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Distinguishes concurrent test threads that read the same wall-clock nonce.
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn directory(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let serial = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("nudox-build-drive-{name}-{nonce}-{serial}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn toolchain() -> Result<ResolvedToolchain<'static>, Box<dyn std::error::Error>> {
    Ok(ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-build-drive-toolchain"),
    )?)
}

fn compile_unit<'source>(
    _database: &Path,
    unit: &DrivenTranslationUnit,
    source: &'source [u8],
    cancelled: &AtomicBool,
    output: &'source mut [u8],
) -> Result<compiler_driver::CompiledFragment<'source>, String> {
    compile_build_command(
        unit,
        source,
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        toolchain().map_err(|error| error.to_string())?,
        cancelled,
        output,
    )
    .map_err(|error| error.to_string())
}

fn limits() -> Result<PublicationLimits, Box<dyn std::error::Error>> {
    Ok(PublicationLimits::new(
        std::num::NonZeroUsize::MIN,
        std::num::NonZeroUsize::MIN,
    )?)
}

/// Reopens a shut-down journal, validates the selected publication's sequence and
/// every fragment, builds and seals its compilation index, and shuts down again.
/// Returns the first fragment's manifest facts so an old generation can still be
/// reopened from the immutable artifact store after later publications.
fn reopen_validate_seal_and_shutdown(
    journal_path: &Path,
    artifacts: &Path,
    expected_sequence: u64,
    expected_fragments: usize,
) -> Result<compiler_publication::manifest::StoredFragmentFacts, Box<dyn std::error::Error>> {
    let publisher =
        DurablePublisher::reopen(&PublicationPaths::in_directory(journal_path), limits()?)?;
    let mut manifest = vec![0; 4 << 20];
    let mut facts = vec![None; expected_fragments];
    let mut fragments = vec![0; expected_fragments * (4 << 20)];
    let mut locality = vec![0; 1 << 20];
    let opened = open_published(
        &publisher,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )?
    .ok_or("reopened journal had no publication")?;
    assert_eq!(*opened.publication.stable.sequence, expected_sequence);
    let opened_fragments = opened.fragments().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(opened_fragments.len(), expected_fragments);
    for fragment in &opened_fragments {
        FragmentView::validate(fragment.view.as_ref())?;
    }
    let first_facts = opened_fragments
        .first()
        .ok_or("reopened publication had no fragments")?
        .facts;
    const SLOTS: usize = 4096;
    let mut projections = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut entities = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut exact_rows = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut lexical_rows = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut atoms = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut type_nodes = vec![MaybeUninit::uninit(); SLOTS * expected_fragments];
    let mut split_projections = projections.chunks_mut(SLOTS);
    let mut split_entities = entities.chunks_mut(SLOTS);
    let mut split_exact_rows = exact_rows.chunks_mut(SLOTS);
    let mut split_lexical_rows = lexical_rows.chunks_mut(SLOTS);
    let mut split_atoms = atoms.chunks_mut(SLOTS);
    let mut split_type_nodes = type_nodes.chunks_mut(SLOTS);
    let mut slots = Vec::with_capacity(expected_fragments);
    for _ in 0..expected_fragments {
        slots.push((
            split_projections.next().ok_or("projection scratch split")?,
            split_entities.next().ok_or("entity scratch split")?,
            split_exact_rows.next().ok_or("exact scratch split")?,
            split_lexical_rows.next().ok_or("lexical scratch split")?,
            split_atoms.next().ok_or("atom scratch split")?,
            split_type_nodes.next().ok_or("type scratch split")?,
        ));
    }
    let prepared = opened_fragments
        .iter()
        .zip(slots)
        .map(
            |(fragment, (projections, entities, exact_rows, lexical_rows, atoms, type_nodes))| {
                build(
                    fragment,
                    IndexBuildScratch {
                        projections,
                        entities,
                        exact_rows,
                        lexical_rows,
                        atoms,
                        type_nodes,
                    },
                )
            },
        )
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("index build failed: {error}"))?;
    let mut exact = prepared.iter().map(|p| p.exact.id).collect::<Vec<_>>();
    let mut lexical = prepared.iter().map(|p| p.lexical.id).collect::<Vec<_>>();
    let sealed = seal_compilation_index(
        opened,
        &prepared,
        CompilationIndexScratch {
            exact: &mut exact,
            lexical: &mut lexical,
        },
    )
    .map_err(|error| format!("index seal failed: {}", error.error))?;
    let plan = plan_index_pack(&sealed)?;
    let mut encoded = vec![0; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded)?;
    publisher.shutdown()?;
    Ok(first_facts)
}

#[test]
fn no_marker_is_a_typed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("none")?;
    let result = discover_and_drive(&root, &root, &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(BuildDriveFailure::NoBuildSystemDetected { .. })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn absent_cmake_is_not_silently_skipped() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("cmake")?;
    fs::write(
        root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.0)\n",
    )?;
    let result = discover_and_drive(&root, &root, &AtomicBool::new(false));
    let cmake_available = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/Users/mileswirht/.local/bin"),
    ]
    .into_iter()
    .chain(
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()),
    )
    .any(|directory| directory.join("cmake").is_file());
    if cmake_available {
        assert!(!matches!(
            result,
            Err(BuildDriveFailure::ToolAbsent { tool: "cmake" })
        ));
    } else {
        assert!(matches!(
            result,
            Err(BuildDriveFailure::ToolAbsent { tool: "cmake" })
        ));
    }
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_command_stream_is_not_capped_at_diagnostic_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let root = directory("large-stream")?;
    let mut makefile = String::from("all:\n");
    for _ in 0..10_000 {
        makefile.push_str("\techo filler\n");
    }
    makefile.push_str("\tclang -c main.c -o main.o\n");
    fs::write(root.join("Makefile"), makefile)?;
    fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(result.translation_units.len(), 1);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn ccache_compile_argv_is_transported_verbatim() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("ccache")?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tccache clang --sysroot /sdk -c main.c -o main.o\n",
    )?;
    fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(result.translation_units[0].arguments[0], "ccache");
    assert_eq!(result.translation_units[0].arguments[1], "clang");
    assert_eq!(result.translation_units[0].arguments[2], "--sysroot");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_compound_line_splits_transport_and_redirections() -> Result<(), Box<dyn std::error::Error>>
{
    let root = directory("compound")?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tprintf 'compile: %s\\n' one.c 1>&2; clang -Iinclude -D'VERBOSE=1' -c one.c -o one.o\n",
    )?;
    fs::write(root.join("one.c"), "int one(void) { return 1; }\n")?;
    let driven = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(driven.translation_units.len(), 1);
    assert_eq!(
        driven.translation_units[0].arguments,
        [
            "clang",
            "-Iinclude",
            "-DVERBOSE=1",
            "-c",
            "one.c",
            "-o",
            "one.o"
        ]
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn compound_compile_with_unknown_compiler_remains_typed() -> Result<(), Box<dyn std::error::Error>>
{
    let root = directory("compound-unknown")?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tprintf ready; mystery-cc -c one.c -o one.o\n",
    )?;
    fs::write(root.join("one.c"), "int one(void) { return 1; }\n")?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(BuildDriveFailure::UnrecognizedCompileCommand { .. })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_recognizes_gnu_and_posix_compiler_cells() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("gnu-compilers")?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tgcc -c main.c -o main.o\n\tg++ -c main.cpp -o main.o\n\tc99 -c main.c -o main.o\n\tc11 -c main.c -o main.o\n\tx86_64-linux-gnu-gcc -c main.c -o main.o\n",
    )?;
    fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
    fs::write(root.join("main.cpp"), "int main() { return 0; }\n")?;
    let driven = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(driven.translation_units.len(), 5);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn buck2_nested_marker_returns_query_terminal_at_cell_root()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(corpus) = std::env::var_os("NUDOX_CORPUS_DIR") else {
        return Ok(());
    };
    let root = PathBuf::from(corpus).join("buck2-examples/examples/with_prelude");
    if !root.is_dir() || !Path::new("/Users/mileswirht/.local/bin/buck2").is_file() {
        return Ok(());
    }
    match discover_and_drive(
        &root,
        &root.join(".nudox-test-scratch"),
        &AtomicBool::new(false),
    ) {
        Err(BuildDriveFailure::ToolPresentUndrivable { tool, evidence }) => {
            assert_eq!(tool, "buck2");
            assert!(!evidence.is_empty());
            assert!(evidence.contains("compilation_database"));
        }
        other => return Err(format!("unexpected buck2 result: {other:?}").into()),
    }
    Ok(())
}

#[test]
fn buck2_compilation_database_target_is_driven() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/Users/mileswirht/.local/bin/buck2").is_file() {
        return Ok(());
    }
    let root = directory("buck-positive")?;
    fs::write(root.join(".buckconfig"), "[cells]\nroot = .\n")?;
    fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
    fs::write(
        root.join("compdb.bzl"),
        "def _impl(ctx):\n    out = ctx.actions.declare_output(\"compile_commands.json\")\n    ctx.actions.run([\"sh\", \"-c\", ctx.attrs.cmd + ' > \"$1\"', \"sh\", out.as_output()], category=\"compdb\")\n    return [DefaultInfo(default_output=out)]\n\ncompilation_database = rule(impl=_impl, attrs={\"cmd\": attrs.string(), \"srcs\": attrs.list(attrs.source())})\n",
    )?;
    fs::write(
        root.join("BUCK"),
        "load(\":compdb.bzl\", \"compilation_database\")\ncompilation_database(name=\"compdb\", srcs=[\"main.c\"], cmd=\"printf '[{\\\"directory\\\":\\\"%s\\\",\\\"file\\\":\\\"main.c\\\",\\\"arguments\\\":[\\\"clang\\\",\\\"-I\\\",\\\"include\\\",\\\"-c\\\",\\\"main.c\\\"]}]' \\\"$PWD\\\"\")\n",
    )?;
    let driven = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(driven.build_system, compiler_driver::BuildSystem::Buck);
    assert_eq!(driven.translation_units.len(), 1);
    assert_eq!(driven.translation_units[0].arguments[1], "-I");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[ignore = "explicit corpus probe; requires NUDOX_CORPUS_DIR"]
fn redis_make_compound_probe_has_no_unrecognized_compile_command()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(corpus) = std::env::var_os("NUDOX_CORPUS_DIR") else {
        return Ok(());
    };
    let root = PathBuf::from(corpus).join("redis");
    if !root.is_dir() {
        return Ok(());
    }
    let driven = discover_and_drive(
        &root,
        &root.join(".nudox-test-scratch"),
        &AtomicBool::new(false),
    )?;
    println!(
        "redis observed translation units: {}",
        driven.translation_units.len()
    );
    Ok(())
}

fn real_tool(name: &str) -> bool {
    [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/Users/mileswirht/.local/bin"),
        PathBuf::from("/etc/profiles/per-user/mileswirht/bin"),
    ]
    .into_iter()
    .chain(
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()),
    )
    .any(|directory| directory.join(name).is_file())
}

#[test]
fn cmake_and_meson_redrive_are_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    if real_tool("cmake") {
        let root = directory("cmake-redrive")?;
        fs::write(
            root.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)\nproject(redrive C)\nadd_executable(redrive main.c)\n",
        )?;
        fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
        let scratch = root.join("scratch");
        let first = discover_and_drive(&root, &scratch, &AtomicBool::new(false))?;
        let second = discover_and_drive(&root, &scratch, &AtomicBool::new(false))?;
        assert_eq!(first.translation_units, second.translation_units);
        fs::remove_dir_all(root)?;
    }
    if real_tool("meson") && real_tool("ninja") {
        let root = directory("meson-redrive")?;
        fs::write(
            root.join("meson.build"),
            "project('redrive', 'c')\nexecutable('redrive', 'main.c')\n",
        )?;
        fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
        let scratch = root.join("scratch");
        let first = discover_and_drive(&root, &scratch, &AtomicBool::new(false))?;
        let second = discover_and_drive(&root, &scratch, &AtomicBool::new(false))?;
        assert_eq!(first.translation_units, second.translation_units);
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn unknown_compile_command_is_a_typed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("unknown-compiler")?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tmystery-cc -c main.c -o main.o\n",
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    fs::write(root.join("main.c"), "int main(void) { return 0; }\n")?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(BuildDriveFailure::UnrecognizedCompileCommand { .. })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cancellation_before_drive_prevents_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("cancel")?;
    fs::write(root.join("Makefile"), "all:\n\tfalse\n")?;
    let cancelled = AtomicBool::new(true);
    let scratch = root.join("scratch");
    let result = discover_and_drive(&root, &scratch, &cancelled);
    assert!(matches!(result, Err(BuildDriveFailure::Cancelled)));
    assert!(!scratch.join("build-drive/compile_commands.json").exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_failure_retains_typed_capture() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("make-failure")?;
    fs::write(root.join("Makefile"), "all:\n\t$(error drive failed)\n")
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    match discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false)) {
        Err(BuildDriveFailure::DriveFailed { tool, captured }) => {
            assert_eq!(tool, "make");
            assert!(!captured.bytes.is_empty());
        }
        other => return Err(format!("unexpected make result: {other:?}").into()),
    }
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn make_drive_compiles_and_publishes_two_generations() -> Result<(), Box<dyn std::error::Error>> {
    let root = directory("make-publish")?;
    fs::create_dir(root.join("include"))?;
    fs::write(root.join("include/required.h"), "#define REQUIRED 7\n")?;
    fs::write(
        root.join("one.c"),
        "#include <required.h>\nstruct Main { int main_field; };\nint main_value(struct Main *value) { return value->main_field + REQUIRED; }\n",
    )?;
    fs::write(
        root.join("two.c"),
        "#include <required.h>\nstruct Util { int util_field; };\nint util_value(struct Util *value) { return value->util_field + REQUIRED; }\n",
    )?;
    fs::write(
        root.join("Makefile"),
        "all: one.o two.o\none.o:\n\tclang -Iinclude -c one.c -o one.o\ntwo.o:\n\tclang -Iinclude -c two.c -o two.o\n",
    )?;
    let cancelled = AtomicBool::new(false);
    let first_drive = discover_and_drive(&root, &root.join("scratch-1"), &cancelled)?;
    assert_eq!(first_drive.translation_units.len(), 2);
    assert!(first_drive.translation_units.iter().all(|unit| {
        unit.arguments
            .windows(2)
            .any(|args| args == ["-Iinclude", "-c"])
    }));
    let one_source = fs::read(root.join("one.c"))?;
    let two_source = fs::read(root.join("two.c"))?;
    let mut one_output = vec![0; 4 << 20];
    let mut two_output = vec![0; 4 << 20];
    let one = compile_unit(
        &first_drive.database_directory,
        &first_drive.translation_units[0],
        &one_source,
        &cancelled,
        &mut one_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let two = compile_unit(
        &first_drive.database_directory,
        &first_drive.translation_units[1],
        &two_source,
        &cancelled,
        &mut two_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    assert!(!one.fragment.as_ref().is_empty());
    assert!(!two.fragment.as_ref().is_empty());
    FragmentView::validate(one.fragment.as_ref())?;
    FragmentView::validate(two.fragment.as_ref())?;
    let first_bytes = [
        one.fragment.as_ref().to_vec(),
        two.fragment.as_ref().to_vec(),
    ];
    let store = directory("make-publish-store")?;
    let artifacts = store.join("artifacts");
    let journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&store.join("journal")),
        limits()?,
    )?;
    let publish = |journal: &DurablePublisher,
                   fragments: &[compiler_driver::CompiledFragment<'_>]| {
        let mut manifest = vec![0; 4 << 20];
        let mut facts = vec![None; fragments.len()];
        let mut ordinals = vec![0; fragments.len()];
        let mut locality = vec![0; 1 << 20];
        let mut binding = vec![0; 128];
        publish_compiled(
            journal,
            &artifacts,
            fragments,
            PublishControl::Continue,
            PublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut facts,
                ordinals: &mut ordinals,
                locality_output: &mut locality,
                binding_output: &mut binding,
            },
        )
    };
    let first = publish(&journal, &[one, two])?;
    let first_generation = first.publication.generation;
    journal.shutdown()?;
    let journal_path = store.join("journal");
    // The gen-1 publication must survive a full shutdown and reopen with its
    // index sealed before any later generation is admitted.
    let first_facts = reopen_validate_seal_and_shutdown(&journal_path, &artifacts, 0, 2)?;

    fs::write(
        root.join("one.c"),
        "#include <required.h>\nstruct Main { int main_field; };\nint main_value(struct Main *value) { return value->main_field + REQUIRED; }\nint changed_helper(void) { return REQUIRED; }\n",
    )?;
    fs::write(
        root.join("three.c"),
        "#include <required.h>\nstruct Extra { int extra_field; };\nint extra_value(struct Extra *value) { return value->extra_field + REQUIRED; }\n",
    )?;
    fs::write(
        root.join("Makefile"),
        "all: one.o two.o three.o\none.o:\n\tclang -Iinclude -c one.c -o one.o\ntwo.o:\n\tclang -Iinclude -c two.c -o two.o\nthree.o:\n\tclang -Iinclude -c three.c -o three.o\n",
    )?;
    let second_drive = discover_and_drive(&root, &root.join("scratch-2"), &cancelled)?;
    assert_eq!(second_drive.translation_units.len(), 3);
    let one_source_2 = fs::read(root.join("one.c"))?;
    let three_source = fs::read(root.join("three.c"))?;
    let mut three_output = vec![0; 4 << 20];
    let mut one_output_2 = vec![0; 4 << 20];
    let mut two_output_2 = vec![0; 4 << 20];
    let one_2 = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[0],
        &one_source_2,
        &cancelled,
        &mut one_output_2,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let two_2 = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[1],
        &two_source,
        &cancelled,
        &mut two_output_2,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let three = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[2],
        &three_source,
        &cancelled,
        &mut three_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    // Changed content produces a different fragment; unchanged content does not.
    assert_ne!(
        Sha256::digest(one_2.fragment.as_ref()),
        Sha256::digest(&first_bytes[0])
    );
    assert_eq!(two_2.fragment.as_ref(), first_bytes[1].as_slice());
    let journal =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let second = publish(&journal, &[one_2, two_2, three])?;
    assert_ne!(first_generation, second.publication.generation);
    assert_ne!(
        first_generation.pinned_root,
        second.publication.generation.pinned_root
    );
    journal.shutdown()?;

    // The chained store reopens with generation two selected, every fragment
    // validating, and generation two's index sealed.
    reopen_validate_seal_and_shutdown(&journal_path, &artifacts, 1, 3)?;
    let mut old_output = vec![0; 4 << 20];
    let old = ImmutableArtifactStore::new(&artifacts)?.open(first_facts, &mut old_output)?;
    assert!(
        first_bytes
            .iter()
            .any(|bytes| Sha256::digest(bytes) == Sha256::digest(old.as_ref()))
    );
    FragmentView::validate(old.as_ref())?;
    fs::remove_dir_all(root)?;
    fs::remove_dir_all(store)?;
    Ok(())
}

#[test]
fn make_dry_run_retains_compiler_arguments_and_database_location()
-> Result<(), Box<dyn std::error::Error>> {
    let root = directory("make")?;
    fs::create_dir(root.join("include"))?;
    fs::write(root.join("include/config.h"), "#define DRIVE_FLAG 1\n")?;
    fs::write(
        root.join("main.c"),
        "#include <config.h>\nint main(void) { return DRIVE_FLAG; }\n",
    )?;
    fs::write(
        root.join("Makefile"),
        "all:\n\tclang -Iinclude -c main.c -o main.o\n",
    )?;
    let result = discover_and_drive(&root, &root.join("scratch"), &AtomicBool::new(false))?;
    assert_eq!(result.translation_units.len(), 1);
    assert!(
        result.translation_units[0]
            .arguments
            .iter()
            .any(|arg| arg == "-Iinclude")
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn meson_packaging_journey_drives_publishes_reopens_and_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    if !real_tool("meson") || !real_tool("ninja") {
        return Ok(());
    }
    let root = directory("meson-publish")?;
    fs::create_dir(root.join("inc"))?;
    fs::write(root.join("inc/config.h"), "#define CONFIGURED 3\n")?;
    fs::write(
        root.join("alpha.c"),
        "#include \"config.h\"\nstruct Alpha { int alpha_field; };\nint alpha_value(struct Alpha *value) { return value->alpha_field + CONFIGURED; }\n",
    )?;
    fs::write(
        root.join("beta.c"),
        "#include \"config.h\"\nstruct Beta { int beta_field; };\nint beta_value(struct Beta *value) { return value->beta_field + CONFIGURED; }\n",
    )?;
    fs::write(
        root.join("meson.build"),
        "project('clang-packaging', 'c')\ninc = include_directories('inc')\nexecutable('clang-packaging', 'alpha.c', 'beta.c', include_directories : inc)\n",
    )?;
    let cancelled = AtomicBool::new(false);
    let scratch = root.join("scratch");
    let first_drive = discover_and_drive(&root, &scratch, &cancelled)?;
    // Bare `ninja -t compdb` emits link, phony, and tool-runner edges as well as
    // compiles; exact admission of the two compile edges is the selection-law
    // falsifier, and every admitted command carries its include flag verbatim.
    assert_eq!(first_drive.translation_units.len(), 2);
    assert!(first_drive.translation_units.iter().all(|unit| {
        unit.arguments.iter().any(|arg| arg == "-c")
            && unit.source.to_string_lossy().ends_with(".c")
            && unit.arguments.iter().any(|arg| arg.contains("inc"))
    }));
    let alpha_source = fs::read(root.join("alpha.c"))?;
    let beta_source = fs::read(root.join("beta.c"))?;
    let mut alpha_output = vec![0; 4 << 20];
    let mut beta_output = vec![0; 4 << 20];
    let alpha = compile_unit(
        &first_drive.database_directory,
        &first_drive.translation_units[0],
        &alpha_source,
        &cancelled,
        &mut alpha_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let beta = compile_unit(
        &first_drive.database_directory,
        &first_drive.translation_units[1],
        &beta_source,
        &cancelled,
        &mut beta_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    FragmentView::validate(alpha.fragment.as_ref())?;
    FragmentView::validate(beta.fragment.as_ref())?;
    let first_bytes = [
        alpha.fragment.as_ref().to_vec(),
        beta.fragment.as_ref().to_vec(),
    ];
    let store = directory("meson-publish-store")?;
    let artifacts = store.join("artifacts");
    let journal_path = store.join("journal");
    let journal =
        DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let publish = |journal: &DurablePublisher,
                   fragments: &[compiler_driver::CompiledFragment<'_>]| {
        let mut manifest = vec![0; 4 << 20];
        let mut facts = vec![None; fragments.len()];
        let mut ordinals = vec![0; fragments.len()];
        let mut locality = vec![0; 1 << 20];
        let mut binding = vec![0; 128];
        publish_compiled(
            journal,
            &artifacts,
            fragments,
            PublishControl::Continue,
            PublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut facts,
                ordinals: &mut ordinals,
                locality_output: &mut locality,
                binding_output: &mut binding,
            },
        )
    };
    let first = publish(&journal, &[alpha, beta])?;
    let first_generation = first.publication.generation;
    journal.shutdown()?;
    let first_facts = reopen_validate_seal_and_shutdown(&journal_path, &artifacts, 0, 2)?;

    // Generation two changes alpha.c, adds gamma.c, and re-drives over the SAME
    // scratch directory (meson setup --wipe re-configuration).
    fs::write(
        root.join("alpha.c"),
        "#include \"config.h\"\nstruct Alpha { int alpha_field; };\nint alpha_value(struct Alpha *value) { return value->alpha_field + CONFIGURED; }\nint changed_helper(void) { return CONFIGURED; }\n",
    )?;
    fs::write(
        root.join("gamma.c"),
        "#include \"config.h\"\nstruct Gamma { int gamma_field; };\nint gamma_value(struct Gamma *value) { return value->gamma_field + CONFIGURED; }\n",
    )?;
    fs::write(
        root.join("meson.build"),
        "project('clang-packaging', 'c')\ninc = include_directories('inc')\nexecutable('clang-packaging', 'alpha.c', 'beta.c', 'gamma.c', include_directories : inc)\n",
    )?;
    let second_drive = discover_and_drive(&root, &scratch, &cancelled)?;
    assert_eq!(second_drive.translation_units.len(), 3);
    let alpha_source_2 = fs::read(root.join("alpha.c"))?;
    let gamma_source = fs::read(root.join("gamma.c"))?;
    let mut alpha_output_2 = vec![0; 4 << 20];
    let mut beta_output_2 = vec![0; 4 << 20];
    let mut gamma_output = vec![0; 4 << 20];
    let alpha_2 = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[0],
        &alpha_source_2,
        &cancelled,
        &mut alpha_output_2,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let beta_2 = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[1],
        &beta_source,
        &cancelled,
        &mut beta_output_2,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let gamma = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[2],
        &gamma_source,
        &cancelled,
        &mut gamma_output,
    )
    .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    assert_ne!(
        Sha256::digest(alpha_2.fragment.as_ref()),
        Sha256::digest(&first_bytes[0])
    );
    assert_eq!(beta_2.fragment.as_ref(), first_bytes[1].as_slice());
    let journal =
        DurablePublisher::reopen(&PublicationPaths::in_directory(&journal_path), limits()?)?;
    let second = publish(&journal, &[alpha_2, beta_2, gamma])?;
    assert_ne!(first_generation, second.publication.generation);
    journal.shutdown()?;
    reopen_validate_seal_and_shutdown(&journal_path, &artifacts, 1, 3)?;
    let mut old_output = vec![0; 4 << 20];
    let old = ImmutableArtifactStore::new(&artifacts)?.open(first_facts, &mut old_output)?;
    assert!(
        first_bytes
            .iter()
            .any(|bytes| Sha256::digest(bytes) == Sha256::digest(old.as_ref()))
    );
    FragmentView::validate(old.as_ref())?;
    fs::remove_dir_all(root)?;
    fs::remove_dir_all(store)?;
    Ok(())
}

// The ToolAbsent twins assert the no-false-absence side on provisioned machines
// (this host carries cmake, meson, ninja, and make); the absent branch fires on
// a tool-less machine where the child PATH genuinely lacks the binary.
#[test]
fn absent_meson_and_make_are_not_silently_skipped() -> Result<(), Box<dyn std::error::Error>> {
    let meson_root = directory("meson-absent")?;
    fs::write(meson_root.join("meson.build"), "project('absent', 'c')\n")?;
    let meson_result = discover_and_drive(&meson_root, &meson_root, &AtomicBool::new(false));
    if real_tool("meson") {
        assert!(!matches!(
            meson_result,
            Err(BuildDriveFailure::ToolAbsent { tool: "meson" })
        ));
    } else {
        assert!(matches!(
            meson_result,
            Err(BuildDriveFailure::ToolAbsent { tool: "meson" })
        ));
    }
    fs::remove_dir_all(meson_root)?;

    let make_root = directory("make-absent")?;
    fs::write(make_root.join("Makefile"), "all:\n\tcc -c main.c\n")?;
    let make_result = discover_and_drive(&make_root, &make_root, &AtomicBool::new(false));
    if real_tool("make") {
        assert!(!matches!(
            make_result,
            Err(BuildDriveFailure::ToolAbsent { tool: "make" })
        ));
    } else {
        assert!(matches!(
            make_result,
            Err(BuildDriveFailure::ToolAbsent { tool: "make" })
        ));
    }
    fs::remove_dir_all(make_root)?;
    Ok(())
}
