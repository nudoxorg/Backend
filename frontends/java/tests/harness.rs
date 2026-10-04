use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use backend_frontend_java::legacy::sourcepath::{
    corpus_source_roots, discover_corpus_roots, discover_maven_sibling_roots, merged_source_roots,
};
use backend_frontend_java::legacy::{
    JavaAuthorityImage, JavaRelease,
    harness::{
        Harness, HarnessError, HarnessOutcome, HarnessRequest, JavaSource, JdkToolchain,
        UnavailableCause,
    },
};
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct TestAuthority {
    toolchain: JdkToolchain<'static>,
    harness: Harness,
}

fn fixture(name: &str) -> io::Result<Vec<u8>> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/authority/src")
            .join(name),
    )
}

fn toolchain() -> Result<JdkToolchain<'static>, HarnessError> {
    JdkToolchain::from_env()
}

fn authority() -> io::Result<(&'static JdkToolchain<'static>, &'static Harness)> {
    static AUTHORITY: OnceLock<Result<TestAuthority, String>> = OnceLock::new();
    let authority = AUTHORITY.get_or_init(|| {
        let toolchain = toolchain().map_err(|error| error.to_string())?;
        let mut harness = Harness::new().map_err(|error| error.to_string())?;
        harness
            .prepare(&toolchain)
            .map_err(|error| error.to_string())?;
        Ok(TestAuthority { toolchain, harness })
    });
    let authority = authority
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))?;
    Ok((&authority.toolchain, &authority.harness))
}

fn scratch_root(harness: &Harness, label: &str) -> io::Result<PathBuf> {
    let root = harness
        .classes_dir()
        .parent()
        .ok_or_else(|| io::Error::other("harness classes directory has no parent"))?
        .join("fixtures")
        .join(label);
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn write_source(root: &Path, relative: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = root.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("fixture source path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::write(&path, bytes)?;
    Ok(path)
}

fn write_source_root(root: &Path, relative: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    write_source(root, relative, bytes)?
        .parent()
        .map(Path::to_owned)
        .ok_or_else(|| io::Error::other("fixture source path has no parent"))
}

#[test]
fn prepare_reuses_digest_marker() -> TestResult {
    let mut harness = Harness::new()?;
    let jdk = toolchain()?;
    harness.prepare(&jdk)?;
    let marker = harness.marker_path();
    let first = fs::metadata(&marker)?.modified()?;
    harness.prepare(&jdk)?;
    assert_eq!(first, fs::metadata(marker)?.modified()?);
    Ok(())
}

#[test]
fn image_binds_the_first_source_bytes() -> TestResult {
    let mut harness = Harness::new()?;
    let jdk = toolchain()?;
    harness.prepare(&jdk)?;
    let cafe = fixture("demo/Cafe.java")?;
    let module = fixture("module-info.java")?;
    let helper = fixture("demo/Helper.java")?;
    let sources = [
        JavaSource {
            name: Path::new("demo/Cafe.java"),
            bytes: &cafe,
        },
        JavaSource {
            name: Path::new("module-info.java"),
            bytes: &module,
        },
        JavaSource {
            name: Path::new("demo/Helper.java"),
            bytes: &helper,
        },
    ];
    let mut output = Vec::new();
    harness.image(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java21,
        },
        &mut output,
    )?;
    let image = JavaAuthorityImage::open(&output)?;
    let mut digest = Sha256::new();
    digest.update(&cafe);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

#[test]
fn syntax_failure_retains_bounded_stderr() -> TestResult {
    let mut harness = Harness::new()?;
    let jdk = toolchain()?;
    harness.prepare(&jdk)?;
    let broken = b"package demo; class Broken {";
    let sources = [JavaSource {
        name: Path::new("demo/Broken.java"),
        bytes: broken,
    }];
    let mut output = Vec::new();
    let error = harness
        .image(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &mut output,
        )
        .err()
        .ok_or_else(|| io::Error::other("invalid Java source unexpectedly compiled"))?;
    match error {
        HarnessError::Command {
            command, stderr, ..
        } => {
            assert!(command.contains("java"));
            assert!(stderr.len() <= 64 * 1024);
        }
        other => panic!("unexpected error: {other}"),
    }
    Ok(())
}

#[test]
fn drop_removes_owned_scratch() -> TestResult {
    let path;
    {
        let harness = Harness::new()?;
        path = harness
            .classes_dir()
            .parent()
            .ok_or_else(|| io::Error::other("harness classes directory has no parent"))?
            .to_owned();
    }
    assert!(!path.exists());
    Ok(())
}

#[test]
fn missing_environment_is_typed() -> TestResult {
    if env::var_os("NUDOX_JDK").is_some() {
        return Ok(());
    }
    assert!(matches!(
        JdkToolchain::from_env(),
        Err(HarnessError::MissingEnvironment {
            variable: "NUDOX_JDK"
        })
    ));
    Ok(())
}

#[test]
fn legacy_yield_fails_release21_then_java8_alternate_succeeds_with_exact_image() -> TestResult {
    let (jdk, harness) = authority()?;
    let source =
        b"package legacy;\n\npublic class yield {\n\tpublic int value() {\n\t\treturn 1;\n\t}\n}\n";
    let sources = [JavaSource {
        name: Path::new("legacy/yield.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: JavaRelease::Java21,
    };
    let mut output = Vec::new();
    let error = harness
        .image(&jdk, request, &mut output)
        .err()
        .ok_or_else(|| io::Error::other("Java 21 yield source unexpectedly compiled"))?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("yield"),
        "diagnostic must name yield: {stderr}"
    );
    assert!(output.is_empty());
    let outcome =
        harness.image_with_releases(&jdk, request, &[], &[JavaRelease::Java8], &mut output)?;
    let HarnessOutcome::Available {
        release,
        prior_failures,
    } = outcome
    else {
        panic!("the Java8 alternate must succeed: {outcome:?}")
    };
    assert_eq!(release, JavaRelease::Java8);
    assert_eq!(prior_failures.len(), 1);
    assert_eq!(prior_failures[0].release, JavaRelease::Java21);
    assert_eq!(prior_failures[0].cause, UnavailableCause::Compilation);
    let HarnessError::Command { stderr, .. } = &prior_failures[0].error else {
        panic!("unexpected retained error: {:?}", prior_failures[0].error)
    };
    assert!(stderr.contains("yield"));
    let image = JavaAuthorityImage::open(&output)?;
    assert_eq!(image.image.release, JavaRelease::Java8);
    let mut digest = Sha256::new();
    digest.update(source);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

/// A source file that imports a package not on the source or class path must
/// fail as an unresolved dependency graph. `javac` still runs at full
/// strictness — the package is not sealed with the import erased.
#[test]
fn a_missing_import_is_dependencies_unresolved_not_a_sealed_package() -> TestResult {
    let (jdk, harness) = authority()?;
    let root = std::env::temp_dir().join(format!(
        "nudox-java-missing-dep-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let src_dir = root.join("com/example/app");
    fs::create_dir_all(&src_dir)?;
    fs::write(
        src_dir.join("App.java"),
        r#"package com.example.app;
import com.google.common.base.Preconditions;
public final class App {
    public String greet(String name) { return Preconditions.checkNotNull(name); }
}
"#,
    )?;
    let source = b"package com.example.app;\nimport com.google.common.base.Preconditions;\npublic final class App {\n    public String greet(String name) { return Preconditions.checkNotNull(name); }\n}\n";
    let sources = [JavaSource {
        name: Path::new("com/example/app/App.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: JavaRelease::Java21,
    };
    let mut output = Vec::new();
    let error = harness
        .image(&jdk, request, &mut output)
        .err()
        .ok_or_else(|| io::Error::other("missing dependency unexpectedly compiled"))?;
    let _ = fs::remove_dir_all(&root);
    let HarnessError::UnresolvedDependencies {
        packages, stderr, ..
    } = error
    else {
        panic!(
            "missing dependency packages must be UnresolvedDependencies, not a weakened compile"
        );
    };
    assert!(
        packages.contains("com.google.common.base"),
        "the missing package must be named: {packages}"
    );
    assert!(
        stderr.contains("not weakened"),
        "the error must say the compiler was not relaxed: {stderr}"
    );
    Ok(())
}

#[test]
fn missing_dependency_with_yield_alternates_retains_both_compilation_attempts() -> TestResult {
    let (jdk, harness) = authority()?;
    let source = b"package legacy;\n\nimport io.vavr.match.annotation.Generate;\n\n@Generate\npublic final class Depends {\n\tpublic yield label() {\n\t\treturn null;\n\t}\n}\n";
    let sources = [JavaSource {
        name: Path::new("legacy/Depends.java"),
        bytes: source,
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: JavaRelease::Java21,
    };
    let mut output = Vec::new();
    let outcome =
        harness.image_with_releases(&jdk, request, &[], &[JavaRelease::Java8], &mut output)?;
    let HarnessOutcome::Unavailable { attempts } = outcome else {
        panic!("a missing dependency must never yield an image")
    };
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].release, JavaRelease::Java21);
    assert_eq!(attempts[0].cause, UnavailableCause::Compilation);
    assert_eq!(attempts[1].release, JavaRelease::Java8);
    assert_eq!(attempts[1].cause, UnavailableCause::Compilation);
    let HarnessError::Command { stderr, .. } = &attempts[1].error else {
        panic!(
            "the final attempt must retain a diagnostic: {:?}",
            attempts[1].error
        )
    };
    assert!(
        stderr.contains("io.vavr.match.annotation"),
        "the final diagnostic must retain the missing dependency: {stderr}"
    );
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn sibling_resolution_uses_discovered_maven_roots() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "sibling-repo")?;
    let app = write_source(
        &repo,
        "com/example/app/1.0.0/src/main/java/demo/App.java",
        b"package demo;\n\npublic class App {\n\tpublic String brew(int cups) {\n\t\treturn Helper.render(cups);\n\t}\n}\n",
    )?;
    write_source(
        &repo,
        "com/example/lib/1.0.0/src/main/java/demo/Helper.java",
        b"package demo;\n\npublic final class Helper {\n\tprivate Helper() {}\n\n\tpublic static String render(int value) {\n\t\treturn \"v:\" + value;\n\t}\n}\n",
    )?;
    let package = repo.join("com/example/app/1.0.0");
    let discovered = discover_maven_sibling_roots(&package, &repo)?;
    assert_eq!(discovered.len(), 2);
    assert!(discovered[0].ends_with("com/example/app/1.0.0/src/main/java"));
    assert!(discovered[1].ends_with("com/example/lib/1.0.0/src/main/java"));
    let roots: Vec<&Path> = discovered.iter().map(PathBuf::as_path).collect();
    let app_bytes = fs::read(&app)?;
    let sources = [JavaSource {
        name: Path::new("demo/App.java"),
        bytes: app_bytes.as_slice(),
    }];
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: JavaRelease::Java21,
    };
    let mut output = Vec::new();
    harness.image_with_sourcepath(&jdk, request, &roots, &mut output)?;
    let image = JavaAuthorityImage::open(&output)?;
    assert_eq!(image.image.release, JavaRelease::Java21);
    let mut digest = Sha256::new();
    digest.update(sources[0].bytes);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    let mut without = Vec::new();
    let error = harness
        .image_with_sourcepath(&jdk, request, &[], &mut without)
        .err()
        .ok_or_else(|| {
            io::Error::other("sibling source unexpectedly compiled without source roots")
        })?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("Helper"),
        "the sibling must be missing without roots: {stderr}"
    );
    assert!(without.is_empty());
    Ok(())
}

#[test]
fn newer_record_fails_java8_explicitly_without_upgrade() -> TestResult {
    let (jdk, harness) = authority()?;
    let source = b"package legacy;\n\npublic record Point(int x, int y) {}\n";
    let sources = [JavaSource {
        name: Path::new("legacy/Point.java"),
        bytes: source,
    }];
    let mut output = Vec::new();
    let error = harness
        .image(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java8,
            },
            &mut output,
        )
        .err()
        .ok_or_else(|| io::Error::other("Java 8 record source unexpectedly compiled"))?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("record"),
        "the diagnostic must name the record: {stderr}"
    );
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn unsupported_release25_reports_compiler_once_without_retry() -> TestResult {
    let (jdk, harness) = authority()?;
    let version = fs::read_to_string(jdk.root().join("release"))?;
    if version.contains("JAVA_VERSION=\"25") {
        return Ok(());
    }
    let source = b"package legacy;\n\npublic final class Ahead {}\n";
    let sources = [JavaSource {
        name: Path::new("legacy/Ahead.java"),
        bytes: source,
    }];
    let mut output = Vec::new();
    let outcome = harness.image_with_releases(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java25,
        },
        &[],
        &[JavaRelease::Java21, JavaRelease::Java8],
        &mut output,
    )?;
    let HarnessOutcome::Unavailable { attempts } = outcome else {
        panic!("an unsupported release must never yield an image")
    };
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].release, JavaRelease::Java25);
    assert_eq!(attempts[0].cause, UnavailableCause::Compiler);
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn malformed_encoding_retains_diagnostic_without_rewrite() -> TestResult {
    let (jdk, harness) = authority()?;
    let mut source =
        b"package legacy;\n\npublic final class Mangled {\n\tpublic String text() {\n\t\treturn \""
            .to_vec();
    source.extend_from_slice(&[0xFF, 0xFE, 0xC3, 0x28]);
    source.extend_from_slice(b"\";\n\t}\n}\n");
    let sources = [JavaSource {
        name: Path::new("legacy/Mangled.java"),
        bytes: &source,
    }];
    let mut output = Vec::new();
    let error = harness
        .image(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &mut output,
        )
        .err()
        .ok_or_else(|| io::Error::other("malformed encoded source unexpectedly compiled"))?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("unmappable character"),
        "the decoder diagnostic must be retained: {stderr}"
    );
    assert!(
        stderr.contains("0xFF"),
        "the diagnostic must cite the exact staged byte: {stderr}"
    );
    assert!(stderr.contains("Mangled"));
    assert!(output.is_empty());
    Ok(())
}

/// A corpus directory laid out exactly like `$NUDOX_JAVA_CORPUS_DIR`:
/// `<group-path>/<artifact>/<version>` with the package root directly under
/// the version directory, plus one `module-info.java`-carrying root.
fn staged_corpus(root: &Path) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    let dependency = root.join("org/testlib/testlib/1.0");
    write_source(
        root,
        "org/testlib/testlib/1.0/org/testlib/Helper.java",
        b"package org.testlib;\n\npublic final class Helper {\n\tprivate Helper() {}\n\n\tpublic static String render(int value) {\n\t\treturn \"v:\" + value;\n\t}\n}\n",
    )?;
    write_source(
        root,
        "org/modlib/modlib/1.0/module-info.java",
        b"module org.modlib {\n\texports org.modlib;\n}\n",
    )?;
    write_source(
        root,
        "org/modlib/modlib/1.0/org/modlib/ModDep.java",
        b"package org.modlib;\n\npublic final class ModDep {\n\tpublic int id() {\n\t\treturn 7;\n\t}\n}\n",
    )?;
    Ok((
        dependency,
        root.join("org/modlib/modlib/1.0"),
        root.to_owned(),
    ))
}

/// Cross-group fleet roots are appended after caller roots, so an artifact
/// depending on a sibling group resolves exactly like the audited rows
/// (jsoup→jspecify, metrics→slf4j). Caller roots keep precedence and the
/// extracted image still binds to the selected bytes.
#[test]
fn cross_group_corpus_root_resolves_fleet_dependency() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "corpus-cross-group")?;
    let corpus_root = repo.join("corpus");
    let (dependency, _module_root, corpus) = staged_corpus(&corpus_root)?;
    let app_root = write_source_root(
        &repo,
        "app/1.0/demo/App.java",
        b"package demo;\n\nimport org.testlib.Helper;\n\npublic class App {\n\tpublic String brew(int cups) {\n\t\treturn Helper.render(cups);\n\t}\n}\n",
    )?;
    let merged = merged_source_roots(&[&app_root], Some(&corpus))?;
    assert_eq!(
        merged,
        vec![fs::canonicalize(&app_root)?, fs::canonicalize(&dependency)?]
    );
    let merged_refs: Vec<&Path> = merged.iter().map(PathBuf::as_path).collect();
    let source = fs::read(repo.join("app/1.0/demo/App.java"))?;
    let sources = [JavaSource {
        name: Path::new("demo/App.java"),
        bytes: source.as_slice(),
    }];
    let mut output = Vec::new();
    harness.image_with_sourcepath(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java21,
        },
        &merged_refs,
        &mut output,
    )?;
    let image = JavaAuthorityImage::open(&output)?;
    let mut digest = Sha256::new();
    digest.update(&source);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

/// A `module-info.java`-carrying corpus root must never enter an unnamed
/// compilation's source path: javac turns it into a required-module lookup
/// that fails unrelated rows before any type resolution. The excluded root's
/// types therefore stay unresolved — a typed compilation refusal naming the
/// missing type, never a module-system failure.
#[test]
fn module_root_is_excluded_and_its_absence_stays_a_typed_compilation_refusal() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "corpus-module-root")?;
    let (_dependency, module_root, corpus) = staged_corpus(&repo.join("corpus"))?;
    let discovered = discover_corpus_roots(&corpus)?;
    assert!(discovered.contains(
        &backend_frontend_java::legacy::sourcepath::CorpusSourceRoot {
            path: fs::canonicalize(&module_root)?,
            module: true,
        }
    ));
    let app_root = write_source_root(
        &repo,
        "app/1.0/demo/ModApp.java",
        b"package demo;\n\nimport org.modlib.ModDep;\n\npublic class ModApp {\n\tpublic int id() {\n\t\treturn ModDep.id();\n\t}\n}\n",
    )?;
    let merged = merged_source_roots(&[&app_root], Some(&corpus))?;
    assert!(
        !merged.contains(&fs::canonicalize(&module_root)?),
        "module roots must not enter the merged source path: {merged:?}"
    );
    let merged_refs: Vec<&Path> = merged.iter().map(PathBuf::as_path).collect();
    let source = fs::read(repo.join("app/1.0/demo/ModApp.java"))?;
    let sources = [JavaSource {
        name: Path::new("demo/ModApp.java"),
        bytes: source.as_slice(),
    }];
    let mut output = Vec::new();
    let error = harness
        .image_with_sourcepath(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &merged_refs,
            &mut output,
        )
        .err()
        .ok_or_else(|| {
            io::Error::other("module-excluded source unexpectedly compiled without its root")
        })?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("ModDep"),
        "the missing type must be named: {stderr}"
    );
    assert!(
        !stderr.contains("module not found"),
        "an excluded module root must not poison unnamed compilation: {stderr}"
    );
    assert!(output.is_empty());
    Ok(())
}

/// End-to-end proof against the real fleet corpus when it is present: a
/// cross-group annotation import (`org.jspecify.annotations`), the exact
/// class of dependency that broke the audited jsoup row, resolves through
/// the appended corpus roots.
#[test]
fn real_corpus_annotation_sibling_resolves_cross_group() -> TestResult {
    let Ok(corpus) = env::var("NUDOX_JAVA_CORPUS_DIR") else {
        eprintln!("NUDOX_JAVA_CORPUS_DIR unset; skipping real-corpus sibling proof");
        return Ok(());
    };
    let discovered = corpus_source_roots(Some(Path::new(&corpus)))?;
    assert!(
        discovered
            .iter()
            .any(|root| root.ends_with("org/jspecify/jspecify/1.0.0")),
        "the real corpus must expose its non-module version roots: {:?}",
        discovered.iter().take(4).collect::<Vec<_>>()
    );
    let (jdk, harness) = authority()?;
    let sources = [JavaSource {
        name: Path::new("demo/Annotated.java"),
        bytes: b"package demo;\n\nimport org.jspecify.annotations.NullMarked;\n\n@NullMarked\npublic final class Annotated {\n\tpublic String text() {\n\t\treturn \"bound\";\n\t}\n}\n",
    }];
    let mut output = Vec::new();
    harness.image_with_sourcepath(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java21,
        },
        &[],
        &mut output,
    )?;
    let image = JavaAuthorityImage::open(&output)?;
    let mut digest = Sha256::new();
    digest.update(sources[0].bytes);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

/// A module-walled target module whose `requires` names a sibling corpus
/// module compiles through `--module-source-path`: the target module maps to
/// its staged run directory, the sibling maps to its corpus root, and javac
/// compiles the required module from source without any rewrite.
#[test]
fn module_walled_target_compiles_through_module_source_path() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "module-two-modules")?;
    let corpus = repo.join("corpus");
    write_source(
        &corpus,
        "org/testlib/testlib.api/1.0.0/module-info.java",
        b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
    )?;
    write_source(
        &corpus,
        "org/testlib/testlib.api/1.0.0/org/testlib/api/Tag.java",
        b"package org.testlib.api;\n\npublic final class Tag {\n\tprivate Tag() {}\n\n\tpublic static String name() {\n\t\treturn \"tag\";\n\t}\n}\n",
    )?;
    let target_root = write_source_root(
        &repo,
        "org/testmod/testmod/1.0.0/module-info.java",
        b"module org.testmod {\n\trequires org.testlib.api;\n\texports org.testmod;\n}\n",
    )?;
    let binding = write_source(
        &repo,
        "org/testmod/testmod/1.0.0/org/testmod/ModApp.java",
        b"package org.testmod;\n\nimport org.testlib.api.Tag;\n\npublic final class ModApp {\n\tpublic String label() {\n\t\treturn Tag.name();\n\t}\n}\n",
    )?;
    let bytes = fs::read(&binding)?;
    let sources = [
        JavaSource {
            name: Path::new("org/testmod/ModApp.java"),
            bytes: bytes.as_slice(),
        },
        JavaSource {
            name: Path::new("module-info.java"),
            bytes:
                b"module org.testmod {\n\trequires org.testlib.api;\n\texports org.testmod;\n}\n",
        },
    ];
    let mut output = Vec::new();
    harness.image_with_corpus(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java21,
        },
        &[&target_root],
        Some(corpus.as_path()),
        &mut output,
    )?;
    let image = JavaAuthorityImage::open(&output)?;
    assert_eq!(image.image.release, JavaRelease::Java21);
    let mut digest = Sha256::new();
    digest.update(bytes.as_slice());
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

/// A non-modular package keeps the default unnamed `-sourcepath` extraction
/// even when the corpus it is compiled against carries module-walled roots:
/// the module-aware feature engages only on module detection, so the
/// default path stays byte-identical and the module roots never poison the
/// unnamed compilation.
#[test]
fn non_modular_default_path_stays_unaffected_by_module_corpus() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "module-default-path")?;
    let (dependency, _module_root, corpus) = staged_corpus(&repo.join("corpus"))?;
    let app_root = write_source_root(
        &repo,
        "app/1.0/demo/PlainApp.java",
        b"package demo;\n\nimport org.testlib.Helper;\n\npublic class PlainApp {\n\tpublic String brew(int cups) {\n\t\treturn Helper.render(cups);\n\t}\n}\n",
    )?;
    let source = fs::read(repo.join("app/1.0/demo/PlainApp.java"))?;
    let sources = [JavaSource {
        name: Path::new("demo/PlainApp.java"),
        bytes: source.as_slice(),
    }];
    let mut output = Vec::new();
    harness.image_with_corpus(
        &jdk,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: JavaRelease::Java21,
        },
        &[&app_root, &dependency],
        Some(corpus.as_path()),
        &mut output,
    )?;
    let image = JavaAuthorityImage::open(&output)?;
    let mut digest = Sha256::new();
    digest.update(&source);
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(image.source_digest(), expected);
    Ok(())
}

/// A module whose `module-info.java` `requires` a module absent from the
/// corpus stays a typed compilation refusal whose preserved diagnostic names
/// exactly that module — nothing is faked or silently dropped.
#[test]
fn missing_module_requirement_stays_typed_naming_the_module() -> TestResult {
    let (jdk, harness) = authority()?;
    let repo = scratch_root(harness, "module-missing-requirement")?;
    let corpus = repo.join("corpus");
    fs::create_dir_all(&corpus)?;
    let target_root = write_source_root(
        &repo,
        "org/testmod/testmod/1.0.0/module-info.java",
        b"module org.testmod {\n\trequires org.testlib.api;\n}\n",
    )?;
    let sources = [
        JavaSource {
            name: Path::new("org/testmod/ModApp.java"),
            bytes: b"package org.testmod;\n\npublic final class ModApp {\n\tpublic String label() {\n\t\treturn \"plain\";\n\t}\n}\n",
        },
        JavaSource {
            name: Path::new("module-info.java"),
            bytes: b"module org.testmod {\n\trequires org.testlib.api;\n}\n",
        },
    ];
    let mut output = Vec::new();
    let error = harness
        .image_with_corpus(
            &jdk,
            HarnessRequest {
                sources: &sources,
                classpath: &[],
                release: JavaRelease::Java21,
            },
            &[&target_root],
            Some(corpus.as_path()),
            &mut output,
        )
        .err()
        .ok_or_else(|| io::Error::other("missing module requirement unexpectedly resolved"))?;
    assert_eq!(
        error.unavailable_cause(),
        Some(UnavailableCause::Compilation)
    );
    let HarnessError::Command { stderr, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert!(
        stderr.contains("module not found: org.testlib.api"),
        "the refusal must name the missing module: {stderr}"
    );
    assert!(output.is_empty());
    Ok(())
}
