#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Focused whole-package reproduction for the real Java corpus terminals.
//!
//! The real audit compiles every `.java` file of a Maven package and lowers
//! the resulting image against the *selected* file's bytes. This probe mirrors
//! that shape so the twelve audited packages can be driven in seconds instead
//! of the full fleet audit. It stages the package with the harness (the same
//! `image_with_sourcepath` path as the flow corpus provider), compiles through
//! the full semantic build (`compile_semantic`, so duplicate declaration
//! identities are detected), and asserts every package lowers.
//!
//! It also carries the root-cause regression: one method with two parameters
//! of the same declared type must lower because image v3 names each carrier by
//! its declared parameter name.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_java::legacy::{
    JavaRelease as HarnessRelease,
    harness::{Harness, HarnessRequest, JavaSource, JdkToolchain},
};
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, Stage};

const TARGETS: &[&str] = &[
    "maven:commons-io:commons-io@2.15.1",
    "maven:org.apache.commons:commons-math3@3.6.1",
    "maven:org.checkerframework:checker-qual@3.36.0",
    "maven:com.google.code.gson:gson@2.10.1",
    "maven:com.fasterxml.jackson.core:jackson-annotations@2.16.1",
    "maven:org.apache.commons:commons-lang3@3.14.0",
    "maven:org.slf4j:slf4j-api@2.0.10",
    "maven:commons-codec:commons-codec@1.16.0",
    "maven:org.apache.commons:commons-csv@1.10.0",
    "maven:org.opentest4j:opentest4j@1.3.0",
    "maven:org.ow2.asm:asm@9.6",
    "maven:org.tukaani:xz@1.9",
];

fn coordinate_parts(coordinate: &str) -> Option<(&str, &str, &str)> {
    let raw = coordinate.strip_prefix("maven:")?;
    let (name, version) = raw.rsplit_once('@')?;
    let (group, artifact) = name.split_once(':')?;
    Some((group, artifact, version))
}

fn package_root(root: &Path, coordinate: &str) -> Option<PathBuf> {
    let (group, artifact, version) = coordinate_parts(coordinate)?;
    let mut path = root.to_owned();
    for component in group.split('.') {
        path.push(component);
    }
    path.push(artifact);
    path.push(version);
    path.is_dir().then_some(path)
}

fn collect_java_sources(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'));
            if !hidden {
                collect_java_sources(&path, out)?;
            }
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("java")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// The largest source file is the audit's selected binding file.
fn selected_file(files: &[PathBuf]) -> Option<PathBuf> {
    files
        .iter()
        .max_by_key(|path| {
            (
                fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                (*path).clone(),
            )
        })
        .cloned()
}

fn sourcepath_entries(package_root: &Path) -> Vec<PathBuf> {
    let mut entries = vec![package_root.to_owned()];
    let Some(group_dir) = package_root.parent().and_then(Path::parent) else {
        return entries;
    };
    let Ok(artifacts) = fs::read_dir(group_dir) else {
        return entries;
    };
    for artifact in artifacts.flatten() {
        let artifact_path = artifact.path();
        if !artifact_path.is_dir() {
            continue;
        }
        let Ok(versions) = fs::read_dir(&artifact_path) else {
            continue;
        };
        for version in versions.flatten() {
            let candidate = version.path();
            if candidate.as_path() == package_root || !candidate.is_dir() {
                continue;
            }
            if candidate.join("module-info.java").is_file() {
                continue;
            }
            entries.push(candidate);
        }
    }
    entries.sort();
    entries.dedup();
    entries
}

fn build_image(
    jdk: &JdkToolchain<'static>,
    harness: &Harness,
    coordinate: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let root = std::env::var_os("NUDOX_JAVA_CORPUS_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "NUDOX_JAVA_CORPUS_DIR is unset".to_owned())?;
    let package_root =
        package_root(&root, coordinate).ok_or_else(|| format!("{coordinate}: package missing"))?;
    let mut files = Vec::new();
    collect_java_sources(&package_root, &mut files).map_err(|error| error.to_string())?;
    files.sort();
    let selected = selected_file(&files).ok_or_else(|| format!("{coordinate}: no .java files"))?;
    let selected_relative = selected
        .strip_prefix(&package_root)
        .map(Path::to_owned)
        .map_err(|error| error.to_string())?;
    let source = fs::read(&selected).map_err(|error| error.to_string())?;

    let mut staged: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for file in &files {
        if file == &selected {
            continue;
        }
        let relative = file
            .strip_prefix(&package_root)
            .map(Path::to_owned)
            .map_err(|error| error.to_string())?;
        let bytes = fs::read(file).map_err(|error| error.to_string())?;
        staged.push((relative, bytes));
    }
    let mut names: Vec<PathBuf> = Vec::with_capacity(staged.len() + 1);
    names.push(selected_relative);
    names.extend(staged.iter().map(|(name, _)| name.clone()));
    let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(staged.len() + 1);
    buffers.push(source.clone());
    buffers.extend(staged.into_iter().map(|(_, bytes)| bytes));
    let sources: Vec<JavaSource<'_>> = names
        .iter()
        .zip(buffers.iter())
        .map(|(name, bytes)| JavaSource {
            name: name.as_path(),
            bytes: bytes.as_slice(),
        })
        .collect();
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let entries = sourcepath_entries(&package_root);
    let roots: Vec<&Path> = entries.iter().map(PathBuf::as_path).collect();
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image_with_sourcepath(jdk, request, &roots, &mut image)
        .map_err(|error| format!("{coordinate}: {error}"))?;
    Ok((source, image))
}

/// Lowers one already-staged image through the full semantic build so the
/// duplicate-declaration-identity lane is exercised, and returns the written
/// fragment length.
fn lower(source: &[u8], image: &[u8], work: &Path) -> Result<usize, String> {
    let tool = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/usr/bin/true"),
        b"fixture",
    )
    .map_err(|error| error.to_string())?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0_u8; 1 << 20];
    let mut output = vec![0_u8; 64 << 20];
    let compiled = compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(tool),
            authority: SemanticAuthorityInput::Java { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(300),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| format!("{failure:?}"))?;
    Ok(compiled.artifact.fragment.as_ref().len())
}

/// The root-cause regression: one method with two parameters of the same
/// declared type must lower, because image v3 names each carrier by its
/// declared parameter name rather than the shared type spelling. Before the
/// fix both carriers were named `java.lang.String`, shared one parent, and
/// collided as `DuplicateDeclarationIdentity`.
#[test]
fn same_typed_parameters_carry_distinct_names_and_lower() -> Result<(), String> {
    use backend_frontend_java::legacy::{DeclarationKind, JavaAuthorityImage};
    let Some(jdk_root) = std::env::var_os("NUDOX_JDK") else {
        eprintln!(
            "same_typed_parameters_carry_distinct_names_and_lower: NUDOX_JDK unset; skipping"
        );
        return Ok(());
    };
    let jdk = JdkToolchain::from_owned_root(PathBuf::from(jdk_root))
        .map_err(|error| error.to_string())?;
    let mut harness = Harness::new().map_err(|error| error.to_string())?;
    harness.prepare(&jdk).map_err(|error| error.to_string())?;

    let source: &[u8] = b"package p; public class P { public void m(String a, String b) {} }";
    let sources = [JavaSource {
        name: Path::new("p/P.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image(&jdk, request, &mut image)
        .map_err(|error| error.to_string())?;

    let authority = JavaAuthorityImage::open(&image).map_err(|error| error.to_string())?;
    let mut names: Vec<String> = Vec::new();
    for declaration in authority.image.declarations() {
        let declaration = declaration.map_err(|error| error.to_string())?;
        if declaration.kind != DeclarationKind::Method {
            continue;
        }
        let Some(symbol_reference) = declaration.symbol else {
            continue;
        };
        let symbol = authority
            .image
            .symbol(symbol_reference)
            .map_err(|error| error.to_string())?;
        for name in symbol.parameter_names {
            let name = name.map_err(|error| error.to_string())?;
            names.push(name.map_or_else(String::new, |atom| {
                String::from_utf8_lossy(atom.bytes).into_owned()
            }));
        }
    }
    if names != ["a", "b"] {
        return Err(format!(
            "expected declared parameter names [a, b], found {names:?}"
        ));
    }

    let work = std::env::temp_dir().join(format!("java-param-{}", std::process::id()));
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    lower(source, &image, &work)
        .map_err(|error| format!("same-typed parameters must lower: {error}"))?;
    Ok(())
}

/// Two overloads that differ only by a type-variable bound (`<T>` vs
/// `<T extends Throwable>`) are distinct Java declarations with distinct
/// erasures. The authority image must keep them distinct, which it does by
/// rendering the bound into the type variable's spelling; otherwise both
/// collapse to `T` and the whole-package semantic build reports a duplicate.
#[test]
fn type_variable_bounds_keep_overloads_distinct() -> Result<(), String> {
    use backend_frontend_java::legacy::JavaAuthorityImage;
    let Some(jdk_root) = std::env::var_os("NUDOX_JDK") else {
        eprintln!("type_variable_bounds_keep_overloads_distinct: NUDOX_JDK unset; skipping");
        return Ok(());
    };
    let jdk = JdkToolchain::from_owned_root(PathBuf::from(jdk_root))
        .map_err(|error| error.to_string())?;
    let mut harness = Harness::new().map_err(|error| error.to_string())?;
    harness.prepare(&jdk).map_err(|error| error.to_string())?;

    let source: &[u8] = b"package p; public class Q { public static <T> T f(T x) { return x; } public static <T extends Throwable> T f(T x) { return x; } }";
    let sources = [JavaSource {
        name: Path::new("p/Q.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image(&jdk, request, &mut image)
        .map_err(|error| error.to_string())?;

    let authority = JavaAuthorityImage::open(&image).map_err(|error| error.to_string())?;
    let mut spellings: Vec<String> = Vec::new();
    for declaration in authority.image.declarations() {
        let declaration = declaration.map_err(|error| error.to_string())?;
        if declaration.name.bytes != b"f".as_slice() {
            continue;
        }
        let Some(symbol_reference) = declaration.symbol else {
            continue;
        };
        let symbol = authority
            .image
            .symbol(symbol_reference)
            .map_err(|error| error.to_string())?;
        let Some(parameter) = symbol.parameters.into_iter().next() else {
            continue;
        };
        let fact = authority
            .image
            .type_fact(parameter)
            .map_err(|error| error.to_string())?;
        spellings.push(
            fact.spelling
                .map(|atom| String::from_utf8_lossy(atom.bytes).into_owned())
                .unwrap_or_default(),
        );
    }
    if spellings.len() != 2 || spellings[0] == spellings[1] {
        return Err(format!(
            "expected two distinct bounded type-variable spellings, found {spellings:?}"
        ));
    }

    let work = std::env::temp_dir().join(format!("java-bound-{}", std::process::id()));
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    lower(source, &image, &work)
        .map_err(|error| format!("bound-distinguished overloads must lower: {error}"))?;
    Ok(())
}

/// Stages and lowers each real package in the audit's shape. `JAVA_REPRO` may
/// name a comma-separated subset of coordinates to drive one package.
#[test]
fn dump_real_package_outcomes() {
    let Some(jdk_root) = std::env::var_os("NUDOX_JDK") else {
        eprintln!("dump_real_package_outcomes: NUDOX_JDK unset; skipping");
        return;
    };
    if std::env::var_os("NUDOX_JAVA_CORPUS_DIR").is_none() {
        eprintln!("dump_real_package_outcomes: NUDOX_JAVA_CORPUS_DIR unset; skipping");
        return;
    }
    let Ok(jdk) = JdkToolchain::from_owned_root(PathBuf::from(jdk_root)) else {
        eprintln!("dump_real_package_outcomes: invalid JDK root; skipping");
        return;
    };
    let Ok(mut harness) = Harness::new() else {
        eprintln!("dump_real_package_outcomes: harness unavailable; skipping");
        return;
    };
    if harness.prepare(&jdk).is_err() {
        eprintln!("dump_real_package_outcomes: doclet prepare failed; skipping");
        return;
    }
    let selected: Vec<String> = std::env::var("JAVA_REPRO")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| TARGETS.iter().map(|target| (*target).to_owned()).collect());

    let work = std::env::temp_dir().join(format!("java-repro-{}", std::process::id()));
    let _ = fs::create_dir_all(&work);
    let mut failures: Vec<String> = Vec::new();
    for coordinate in selected {
        let coordinate = coordinate.as_str();
        match build_image(&jdk, &harness, coordinate) {
            Err(error) => {
                eprintln!("REPRO|{coordinate}|IMAGE|{error}");
                failures.push(format!("{coordinate}: image: {error}"));
            }
            Ok((source, image)) => match lower(&source, &image, &work) {
                Ok(fragment_bytes) => eprintln!(
                    "REPRO|{coordinate}|OK|image={}|fragment={}|source={}",
                    image.len(),
                    fragment_bytes,
                    source.len()
                ),
                Err(error) => {
                    eprintln!("REPRO|{coordinate}|FAIL|{error}");
                    failures.push(format!("{coordinate}: {error}"));
                }
            },
        }
    }
    assert!(
        failures.is_empty(),
        "real Java package terminals remain:\n{}",
        failures.join("\n")
    );
}

/// A method and a constructor with the same name on one type are distinct Java
/// declarations. Naming parameter carriers by their declared names must not
/// merge them, and the semantic build proves it: both lower.
#[test]
fn same_named_method_and_constructor_stay_distinct() -> Result<(), String> {
    let Some(jdk_root) = std::env::var_os("NUDOX_JDK") else {
        return Ok(());
    };
    let jdk = JdkToolchain::from_owned_root(PathBuf::from(jdk_root))
        .map_err(|error| error.to_string())?;
    let mut harness = Harness::new().map_err(|error| error.to_string())?;
    harness.prepare(&jdk).map_err(|error| error.to_string())?;
    let source: &[u8] = b"package p; public class R { public R() {} public void R() {} }";
    let sources = [JavaSource {
        name: Path::new("p/R.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: HarnessRelease::Java21,
    };
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image(&jdk, request, &mut image)
        .map_err(|error| error.to_string())?;
    let work = std::env::temp_dir().join(format!("java-adv-{}", std::process::id()));
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    lower(source, &image, &work)
        .map_err(|error| format!("method+constructor same name: {error}"))?;
    Ok(())
}
