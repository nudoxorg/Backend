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
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

fn directory(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!("nudox-build-drive-{name}-{nonce}"));
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
    let publish = |fragments: &[compiler_driver::CompiledFragment<'_>]| {
        let mut manifest = vec![0; 4 << 20];
        let mut facts = vec![None; fragments.len()];
        let mut ordinals = vec![0; fragments.len()];
        let mut locality = vec![0; 1 << 20];
        let mut binding = vec![0; 128];
        publish_compiled(
            &journal,
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
    let first = publish(&[one, two])?;
    let first_generation = first.publication.generation;
    let mut manifest = vec![0; 4 << 20];
    let mut facts = vec![None; 2];
    let mut fragments = vec![0; 8 << 20];
    let mut locality = vec![0; 1 << 20];
    let opened_first = open_published(
        &journal,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )?
    .ok_or("gen-1 was not published")?;
    let first_facts = opened_first
        .fragments()
        .next()
        .ok_or("gen-1 had no fragment")??
        .facts;

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
    let three_source = fs::read(root.join("three.c"))?;
    let mut three_output = vec![0; 4 << 20];
    let mut one_output_2 = vec![0; 4 << 20];
    let mut two_output_2 = vec![0; 4 << 20];
    let one_2 = compile_unit(
        &second_drive.database_directory,
        &second_drive.translation_units[0],
        &one_source,
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
    let second = publish(&[one_2, two_2, three])?;
    assert_ne!(first_generation, second.publication.generation);
    assert_ne!(
        first_generation.pinned_root,
        second.publication.generation.pinned_root
    );

    let mut manifest = vec![0; 4 << 20];
    let mut facts = vec![None; 3];
    let mut fragments = vec![0; 12 << 20];
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
    )?
    .ok_or("live publisher had no publication")?;
    let opened_fragments = opened.fragments().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(opened_fragments.len(), 3);
    for fragment in &opened_fragments {
        FragmentView::validate(fragment.view.as_ref())?;
    }
    let mut projections = vec![MaybeUninit::uninit(); 4096];
    let mut entities = vec![MaybeUninit::uninit(); 4096];
    let mut exact_rows = vec![MaybeUninit::uninit(); 4096];
    let mut lexical_rows = vec![MaybeUninit::uninit(); 4096];
    let mut atoms = vec![MaybeUninit::uninit(); 4096];
    let mut type_nodes = vec![MaybeUninit::uninit(); 4096];
    let mut projections_2 = vec![MaybeUninit::uninit(); 4096];
    let mut entities_2 = vec![MaybeUninit::uninit(); 4096];
    let mut exact_rows_2 = vec![MaybeUninit::uninit(); 4096];
    let mut lexical_rows_2 = vec![MaybeUninit::uninit(); 4096];
    let mut atoms_2 = vec![MaybeUninit::uninit(); 4096];
    let mut type_nodes_2 = vec![MaybeUninit::uninit(); 4096];
    let mut projections_3 = vec![MaybeUninit::uninit(); 4096];
    let mut entities_3 = vec![MaybeUninit::uninit(); 4096];
    let mut exact_rows_3 = vec![MaybeUninit::uninit(); 4096];
    let mut lexical_rows_3 = vec![MaybeUninit::uninit(); 4096];
    let mut atoms_3 = vec![MaybeUninit::uninit(); 4096];
    let mut type_nodes_3 = vec![MaybeUninit::uninit(); 4096];
    let prepared = [
        build(
            &opened_fragments[0],
            IndexBuildScratch {
                projections: &mut projections,
                entities: &mut entities,
                exact_rows: &mut exact_rows,
                lexical_rows: &mut lexical_rows,
                atoms: &mut atoms,
                type_nodes: &mut type_nodes,
            },
        ),
        build(
            &opened_fragments[1],
            IndexBuildScratch {
                projections: &mut projections_2,
                entities: &mut entities_2,
                exact_rows: &mut exact_rows_2,
                lexical_rows: &mut lexical_rows_2,
                atoms: &mut atoms_2,
                type_nodes: &mut type_nodes_2,
            },
        ),
        build(
            &opened_fragments[2],
            IndexBuildScratch {
                projections: &mut projections_3,
                entities: &mut entities_3,
                exact_rows: &mut exact_rows_3,
                lexical_rows: &mut lexical_rows_3,
                atoms: &mut atoms_3,
                type_nodes: &mut type_nodes_3,
            },
        ),
    ]
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| error.to_string())?;
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
    .map_err(|error| error.error.to_string())?;
    let plan = plan_index_pack(&sealed)?;
    let mut encoded = vec![0; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded)?;
    let mut old_output = vec![0; 4 << 20];
    let old = ImmutableArtifactStore::new(&artifacts)?.open(first_facts, &mut old_output)?;
    assert!(
        first_bytes
            .iter()
            .any(|bytes| Sha256::digest(bytes) == Sha256::digest(old.as_ref()))
    );
    FragmentView::validate(old.as_ref())?;
    journal.shutdown()?;
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
