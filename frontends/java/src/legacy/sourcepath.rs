use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

mod discover;

pub use discover::{
    ModuleSourceRoot, corpus_module_roots, corpus_source_roots, discover_corpus_roots,
    discover_maven_sibling_roots, discover_source_roots, discovered_corpus_roots,
    merged_source_roots, parse_module_name,
};

const CONVENTIONAL_CHILDREN: [&str; 2] = ["src/main/java", "src/main/generated"];

/// Environment variable naming the fleet Java corpus root, mirroring the Go
/// frontend's `$NUDOX_GO_CORPUS_DIR` staging contract.
pub const CORPUS_ENV: &str = "NUDOX_JAVA_CORPUS_DIR";

/// One discovered corpus version root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusSourceRoot {
    /// The canonicalized `…/<group-path>/<artifact>/<version>` directory.
    pub path: PathBuf,
    /// True when the root carries a top-level `module-info.java`, so javac
    /// treats it as a JPMS module root. Such roots are admitted to module-mode
    /// discovery but excluded from unnamed-module source paths: a
    /// `module-info.java` anywhere on an unnamed compilation's source path
    /// makes javac resolve that module's `requires` clauses and fail before
    /// any type resolution.
    pub module: bool,
}

#[cfg(test)]
mod sourcepath_tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        CorpusSourceRoot, discover_corpus_roots, discover_maven_sibling_roots,
        discover_source_roots, merged_source_roots, parse_module_name,
    };

    struct TempDir(PathBuf);

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "nudox-java-sourcepath-{label}-{}-{ordinal}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create unique temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create temporary layout");
            path
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create temporary parent");
            }
            fs::write(&path, []).expect("write temporary marker");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                if !std::thread::panicking() {
                    panic!("remove temporary directory: {error}");
                }
            }
        }
    }

    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).expect("canonicalize expected path")
    }

    #[test]
    fn discovers_flat_and_conventional_roots_in_caller_order_and_skips_missing() {
        let temp = TempDir::new("roots");
        let flat = temp.dir("flat");
        temp.file("flat/legacy-1.0-sources.jar");
        let java = temp.dir("project/src/main/java");
        let generated = temp.dir("project/src/main/generated");
        let missing = temp.path().join("absent");
        let discovered = discover_source_roots(&[&flat, &temp.path().join("project"), &missing])
            .expect("discover source roots");
        assert_eq!(
            discovered,
            vec![canonical(&flat), canonical(&java), canonical(&generated)]
        );
        let empty = discover_source_roots(&[&missing]).expect("missing roots are skipped");
        assert!(empty.is_empty());
    }

    #[test]
    fn deduplicates_repeated_and_nested_roots_preserving_first_occurrence() {
        let temp = TempDir::new("dedup");
        let java = temp.dir("project/src/main/java");
        let generated = temp.dir("project/src/main/generated");
        let project = temp.path().join("project");
        let dotted = temp.path().join(".").join("project");
        let nested_java = project.join("src").join("main").join("java");
        let discovered =
            discover_source_roots(&[&project, &nested_java, &dotted]).expect("discover roots");
        assert_eq!(discovered, vec![canonical(&java), canonical(&generated)]);
        let flat = temp.dir("flat");
        let dotted_flat = temp.path().join(".").join("flat");
        let discovered =
            discover_source_roots(&[&flat, &dotted_flat]).expect("discover flat roots twice");
        assert_eq!(discovered, vec![canonical(&flat)]);
    }

    #[test]
    fn flat_root_without_conventional_children_is_admitted_itself() {
        let temp = TempDir::new("flat");
        let flat = temp.dir("flat");
        temp.file("flat/Foo.java");
        let discovered = discover_source_roots(&[&flat]).expect("discover flat root");
        assert_eq!(discovered, vec![canonical(&flat)]);
    }

    #[test]
    fn discovers_group_siblings_only_at_exact_version() {
        let temp = TempDir::new("siblings");
        let alpha = temp.dir("com/example/alpha/1.0.0");
        temp.file("com/example/alpha/1.0.0/Alpha.java");
        let beta_java = temp.dir("com/example/beta/1.0.0/src/main/java");
        let gamma = temp.dir("com/example/gamma/1.0.0");
        temp.dir("com/example/beta/1.9.0");
        temp.dir("com/example/alpha/2.0.0");
        temp.dir("com/other/delta/1.0.0");
        temp.file("com/example/stray.txt");
        let package = temp.path().join("com/example/alpha/1.0.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(
            discovered,
            vec![canonical(&alpha), canonical(&beta_java), canonical(&gamma)]
        );
    }

    #[test]
    fn sibling_without_version_directory_or_sources_is_skipped() {
        let temp = TempDir::new("empty-sibling");
        let alpha = temp.dir("g/a/1.0");
        temp.dir("g/b/2.0");
        temp.file("g/c/notes.txt");
        let package = temp.path().join("g/a/1.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(discovered, vec![canonical(&alpha)]);
    }

    #[test]
    fn rejects_package_roots_outside_repository_boundary() {
        let temp = TempDir::new("boundary");
        let outside = TempDir::new("outside");
        let package = outside.dir("com/example/alpha/1.0.0");
        let error = discover_maven_sibling_roots(&package, temp.path())
            .expect_err("outside package root must not enter discovery");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let shallow = temp.dir("only-artifact");
        let error = discover_maven_sibling_roots(&shallow, temp.path())
            .expect_err("shallow layout is not a Maven version root");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let missing = temp.path().join("absent-repository");
        let error = discover_maven_sibling_roots(&package, &missing)
            .expect_err("missing repository propagates");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    // Creating a symlink needs developer mode or elevation on Windows.
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_package_root_that_resolves_outside_repository() {
        let temp = TempDir::new("symlink-escape");
        let outside = TempDir::new("symlink-target");
        let group = outside.dir("com/example");
        outside.dir("com/example/alpha/1.0.0");
        let link = temp.path().join("com");
        fs::create_dir_all(&link).expect("create group parent");
        symlink(&group, link.join("example")).expect("create escaping symlink");
        let package = link.join("example").join("alpha").join("1.0.0");
        let error = discover_maven_sibling_roots(&package, temp.path())
            .expect_err("symlinked ancestor escaping the repository is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    // Creating a symlink needs developer mode or elevation on Windows.
    #[cfg(unix)]
    #[test]
    fn skips_sibling_symlinks_that_escape_the_repository() {
        let temp = TempDir::new("sibling-escape");
        let alpha = temp.dir("g/a/1.0");
        let outside = TempDir::new("sibling-target");
        let target = outside.dir("b/1.0");
        fs::create_dir_all(temp.path().join("g")).expect("create group directory");
        symlink(&target, temp.path().join("g").join("b")).expect("create escaping sibling");
        let package = temp.path().join("g/a/1.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(discovered, vec![canonical(&alpha)]);
    }

    #[test]
    fn rejects_repository_equal_to_package_root() {
        let temp = TempDir::new("equal");
        let error = discover_maven_sibling_roots(temp.path(), temp.path())
            .expect_err("repository root alone is not a version root");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn corpus_discovery_admits_flat_deep_and_single_component_group_roots() {
        let temp = TempDir::new("corpus");
        temp.dir("commons-io/commons-io/2.15.1/org/example/flat");
        temp.file("commons-io/commons-io/2.15.1/org/example/flat/Flat.java");
        temp.dir("org/apache/commons/commons-text/1.11.0/org/apache/commons/text");
        temp.file("org/apache/commons/commons-text/1.11.0/org/apache/commons/text/T.java");
        temp.dir("io/vavr/vavr/0.10.4/io/vavr");
        temp.file("io/vavr/vavr/0.10.4/io/vavr/API.java");
        temp.dir("org/junit/platform/junit-platform-commons/1.10.1/org/junit/platform/commons");
        temp.file("org/junit/platform/junit-platform-commons/1.10.1/module-info.java");
        temp.file("org/junit/platform/junit-platform-commons/1.10.1/org/junit/platform/commons/Preconditions.java");
        temp.file("org/jetbrains/annotations/24.0.1/org/jetbrains/annotations/NotNull.java");
        temp.file("empty-version/1.0/notes.txt");
        temp.file("loose.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover corpus roots");
        let expected = [
            "commons-io/commons-io/2.15.1",
            "io/vavr/vavr/0.10.4",
            "org/apache/commons/commons-text/1.11.0",
            "org/jetbrains/annotations/24.0.1",
            "org/junit/platform/junit-platform-commons/1.10.1",
        ]
        .iter()
        .map(|relative| CorpusSourceRoot {
            path: canonical(&temp.path().join(relative)),
            module: relative.ends_with("junit-platform-commons/1.10.1"),
        })
        .collect::<Vec<_>>();
        assert_eq!(discovered, expected, "sorted pre-order with module flag");
    }

    #[test]
    fn corpus_discovery_prunes_below_admitted_roots_and_multiple_versions() {
        let temp = TempDir::new("corpus-versions");
        temp.dir("org/jspecify/jspecify/1.0.0/org/jspecify/annotations");
        temp.file("org/jspecify/jspecify/1.0.0/org/jspecify/annotations/NullMarked.java");
        temp.dir("org/jspecify/jspecify/2.0.10/org/jspecify/annotations");
        temp.file("org/jspecify/jspecify/2.0.10/org/jspecify/annotations/NullMarked.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover both versions");
        assert_eq!(
            discovered,
            [
                "org/jspecify/jspecify/2.0.10",
                "org/jspecify/jspecify/1.0.0"
            ]
            .iter()
            .map(|relative| CorpusSourceRoot {
                path: canonical(&temp.path().join(relative)),
                module: false,
            })
            .collect::<Vec<_>>(),
            "same-artifact versions run newest-first"
        );
    }

    #[test]
    fn duplicate_artifact_roots_order_newest_version_first() {
        let temp = TempDir::new("corpus-duplicate-versions");
        // error_prone_annotations-style pair: the oldest version's
        // `@RestrictedApi.link()` carries no default, so javac's first-match
        // source path resolution must see 2.26.1 first.
        temp.dir("com/google/errorprone/error_prone_annotations/2.18.0/com/google/errorprone/annotations");
        temp.file("com/google/errorprone/error_prone_annotations/2.18.0/com/google/errorprone/annotations/RestrictedApi.java");
        temp.dir("com/google/errorprone/error_prone_annotations/2.26.1/com/google/errorprone/annotations");
        temp.file("com/google/errorprone/error_prone_annotations/2.26.1/com/google/errorprone/annotations/RestrictedApi.java");
        // Numeric segments compare by value: 1.10.0 is newer than 1.9.0 even
        // though plain lexicographic order would put it first.
        temp.dir("org/example/one/1.10.0/org/example/one");
        temp.file("org/example/one/1.10.0/org/example/one/Newer.java");
        temp.dir("org/example/one/1.9.0/org/example/one");
        temp.file("org/example/one/1.9.0/org/example/one/Older.java");
        // A single-version artifact between the duplicates keeps its
        // lexicographic pre-order position.
        temp.dir("commons-io/commons-io/2.16.1/org/example/flat");
        temp.file("commons-io/commons-io/2.16.1/org/example/flat/Flat.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover duplicates");
        let corpus = canonical(temp.path());
        let relatives = discovered
            .iter()
            .map(|root| {
                root.path
                    .strip_prefix(&corpus)
                    .expect("corpus-relative root")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            relatives,
            vec![
                "com/google/errorprone/error_prone_annotations/2.26.1",
                "com/google/errorprone/error_prone_annotations/2.18.0",
                "commons-io/commons-io/2.16.1",
                "org/example/one/1.10.0",
                "org/example/one/1.9.0",
            ],
            "same-artifact duplicates run newest-first; everything else keeps sorted pre-order"
        );
    }

    #[test]
    fn corpus_discovery_descends_through_dotted_artifact_directories() {
        let temp = TempDir::new("corpus-dotted-artifacts");
        // A dotted Maven artifact directory carries only version children;
        // the version directories beneath it are the real roots.
        temp.dir("org/osgi/org.osgi.framework/1.10.0/org/osgi/framework");
        temp.file("org/osgi/org.osgi.framework/1.10.0/org/osgi/framework/Bundle.java");
        temp.dir("biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1/aQute/bnd/annotation/spi");
        temp.file("biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1/aQute/bnd/annotation/spi/ServiceProvider.java");
        // A dotted artifact whose version child carries a top-level
        // module-info.java is still discovered as a module root.
        temp.dir("javax/xml/bind/jaxb-api/2.3.1/javax/xml/bind");
        temp.file("javax/xml/bind/jaxb-api/2.3.1/module-info.java");
        temp.file("javax/xml/bind/jaxb-api/2.3.1/javax/xml/bind/JAXB.java");
        // A dotted artifact with no version children and no Java stays out.
        temp.dir("net/example/example.empty");
        let discovered = discover_corpus_roots(temp.path()).expect("discover through dots");
        assert_eq!(
            discovered,
            [
                "biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1",
                "javax/xml/bind/jaxb-api/2.3.1",
                "org/osgi/org.osgi.framework/1.10.0",
            ]
            .iter()
            .map(|relative| CorpusSourceRoot {
                path: canonical(&temp.path().join(relative)),
                module: relative.ends_with("jaxb-api/2.3.1"),
            })
            .collect::<Vec<_>>(),
            "dotted artifact directories never shadow their version roots"
        );
    }

    #[test]
    fn corpus_discovery_never_admits_a_version_looking_corpus_root() {
        let temp = TempDir::new("corpus-root-guard");
        // A corpus root whose final path component starts with a digit (a
        // content-addressed store path) must never be admitted as a version
        // root: doing so prunes every real root beneath it.
        let corpus = temp.dir("12345-fleet-corpus-java");
        temp.dir("12345-fleet-corpus-java/org/example/lib/1.0/org/example");
        temp.file("12345-fleet-corpus-java/org/example/lib/1.0/org/example/Lib.java");
        let discovered = discover_corpus_roots(&corpus).expect("discover under version-like root");
        assert_eq!(
            discovered,
            vec![CorpusSourceRoot {
                path: canonical(&corpus.join("org/example/lib/1.0")),
                module: false,
            }]
        );
    }

    #[test]
    fn merged_roots_keep_caller_precedence_and_exclude_module_roots() {
        let corpus = TempDir::new("corpus-merge");
        corpus.dir("org/testlib/testlib/1.0/org/testlib");
        corpus.file("org/testlib/testlib/1.0/org/testlib/Helper.java");
        corpus.dir("org/modlib/modlib/1.0/org/modlib");
        corpus.file("org/modlib/modlib/1.0/module-info.java");
        corpus.file("org/modlib/modlib/1.0/org/modlib/ModDep.java");
        let app = TempDir::new("app-root");
        app.file("demo/App.java");
        let app_root = app.path().to_owned();
        let merged = merged_source_roots(&[&app_root], Some(corpus.path()))
            .expect("merge caller and corpus roots");
        assert_eq!(
            merged,
            vec![
                canonical(&app_root),
                canonical(&corpus.path().join("org/testlib/testlib/1.0")),
            ]
        );
        assert!(
            !merged
                .iter()
                .any(|root| root.starts_with(corpus.path().join("org/modlib"))),
            "module roots must never enter an unnamed source path: {merged:?}"
        );
        let empty = merged_source_roots(&[], Some(app.path())).expect("corpus without versions");
        assert!(empty.is_empty());
    }

    #[test]
    fn parses_module_names_from_plain_annotated_and_open_declarations() {
        assert_eq!(
            parse_module_name(b"module org.example.lib {\n\texports org.example.lib;\n}\n"),
            Some(String::from("org.example.lib"))
        );
        assert_eq!(
            parse_module_name(b"open module org.example.open {\n}\n"),
            Some(String::from("org.example.open"))
        );
        assert_eq!(
            parse_module_name(
                b"// a comment mentioning module fake.decoy\n/* block module also.fake */\n@Deprecated\nmodule org.example.doc {\n}\n"
            ),
            Some(String::from("org.example.doc"))
        );
        assert_eq!(
            parse_module_name(
                b"@Deprecated(\"module not.a.decoy\")\nmodule org.example.strings {\n}\n"
            ),
            Some(String::from("org.example.strings"))
        );
    }

    #[test]
    fn module_name_parsing_refuses_declarationless_files() {
        assert_eq!(
            parse_module_name(b"package demo;\n\nfinal class X {}\n"),
            None
        );
        assert_eq!(parse_module_name(b""), None);
        assert_eq!(parse_module_name(b"module {\n}\n"), None);
        assert_eq!(
            parse_module_name(
                b"module org.example.lib {\n\trequires transitive org.example.dep;\n}\n"
            ),
            Some(String::from("org.example.lib")),
            "directives after the body brace never leak into the name"
        );
    }

    #[test]
    fn corpus_module_roots_deduplicate_by_module_name_keeping_newest_version() {
        let temp = TempDir::new("corpus-modules");
        temp.file("org/testlib/testlib.api/1.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/testlib/testlib.api/1.0.0/module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("write first module declaration");
        temp.dir("org/testlib/testlib.api/1.0.0/org/testlib/api");
        temp.file("org/testlib/testlib.api/2.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/testlib/testlib.api/2.0.0/module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("write second module declaration");
        temp.dir("org/testlib/testlib.api/2.0.0/org/testlib/api");
        temp.file("org/plain/plain/1.0.0/org/plain/Plain.java");
        temp.file("org/unnamed/unnamed/1.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/unnamed/unnamed/1.0.0/module-info.java"),
            b"// no declaration here\nfinal class Junk {}\n",
        )
        .expect("write unparseable declaration");
        let modules = super::corpus_module_roots(Some(temp.path())).expect("module corpus roots");
        assert_eq!(
            modules,
            vec![super::ModuleSourceRoot {
                name: String::from("org.testlib.api"),
                path: canonical(&temp.path().join("org/testlib/testlib.api/2.0.0")),
            }],
            "one entry per module name, newest version wins, nameless roots skipped"
        );
    }
}
