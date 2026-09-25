//! `javadoc`-doclet oracle invocation: command construction and subprocess run.
//!
//! # Invocation contract
//!
//! Java is one of the three external-toolchain subprocess producers (Go, C#,
//! Java — see `crate::oracle`'s module doc), so [`invoke`] uses the
//! shared [`crate::oracle::run_json`] helper: spawn a command,
//! capture stdout/stderr, treat a non-zero exit as a typed error, and
//! deserialize stdout as the oracle's JSON document ([`Extraction`]).
//!
//! The command is:
//!
//! ```text
//! javadoc -quiet -doclet nudox.oracle.Extractor -docletpath <classes> \
//!     [-sourcepath <root>:<sibling dirs>                  # non-modular target
//!      | --module-source-path <mod>=<dir> ...]            # modular target
//!     [--module-path <corpus>/.module-path] [--add-modules <spec>] \
//!     [--release <N>] [--add-exports <module>/<pkg>=ALL-UNNAMED ...] \
//!     <every .java file under PackageSource::root, recursively>
//! ```
//!
//! The `-sourcepath` / `--module-source-path` choice is forced by `javac`,
//! which rejects both in one invocation, and is made on one fact: whether
//! `PackageSource::root` holds a `module-info.java`. See
//! [`module_source_path_args`].
//!
//! No `-outfile` side channel is used: the doclet prints its one JSON document
//! to stdout only when `-outfile` is absent (see `Extractor.run`), and
//! `javadoc`'s own progress notices (`"Loading source file ...`,
//! `"Constructing Javadoc information..."`) go to **stderr**, not stdout, on
//! this JDK — verified empirically (JDK 21.0.11, Zulu) by capturing the two
//! streams separately: stdout was byte-identical whether or not `-quiet` was
//! passed. `-quiet` is passed anyway to keep stderr clean for the error path.
//!
//! # `-sourcepath`: best-effort cross-artifact resolution, not a dependency resolver
//!
//! Real Maven libraries routinely import types from *other* Maven artifacts
//! (Guava's error-prone annotations, JUnit 5's platform-commons, Jackson's
//! split core/annotations/databind, …), and `PackageSource` hands `invoke`
//! exactly one artifact's own sources with no classpath at all. Two prior
//! diagnoses of the resulting "5 of 22 corpus packages lower" failure were
//! each wrong in a specific, checkable way: one assumed adding a classpath
//! flag alone would fix most of them (it fixes none — no flag conjures
//! dependency *content* that was never fetched); the other assumed the maven
//! corpus packages were not provisioned at all (all 20/22 *were* on disk
//! under `result/`, hash-verified — see `nix/corpus.nix`). The
//! real, sweep-verified shape (`tests/corpus_sweep.rs`) is per-package and
//! non-uniform: 16 of the 17 non-lowering entries need one or more Maven
//! artifacts that are not part of this corpus at all (Guava needs
//! `com.google.errorprone`/`checkerframework`/`javax.annotation`; JUnit 4
//! needs `org.hamcrest`; Jackson-databind needs `jackson-core` +
//! `jackson-annotations`; …) — no local flag fixes those; they need new
//! corpus entries this crate is not scoped to add unilaterally. Exactly one
//! (`io.reactivex.rxjava3:rxjava`) needed precisely one artifact
//! (`org.reactivestreams:reactive-streams`, 4 interfaces) and nothing else —
//! confirmed by rerunning `javadoc -Xmaxerrs 100000` and observing every one
//! of its ~2200 errors trace to `org.reactivestreams`. That one artifact is
//! now a corpus entry (see `nix/corpus.nix`), and [`invoke`] passes
//! `-sourcepath` so `javadoc` can find it (and anything else already
//! fetched) as a sibling checkout, instead of hand-wiring one classpath
//! entry per package.
//!
//! [`sourcepath_entries`] builds that `-sourcepath` value from whatever
//! directories happen to sit beside `PackageSource::root()` — it does not
//! know or care that they came from `nix/corpus.nix`; any caller that
//! extracts several package checkouts as sibling directories gets the same
//! benefit. Two things keep this from being a real dependency resolver: (1)
//! it is peer-directory discovery, not `groupId:artifactId` resolution — a
//! sibling only helps if its Java package names happen to match what the
//! target imports, exactly as with reactive-streams here; (2) any sibling
//! whose own root holds a `module-info.java` is excluded. `javac` treats
//! every `-sourcepath` directory that contains one as a potential JPMS module
//! root, and with more than one such directory on the path at once it fails
//! *pre-resolution* module lookups (`module not found: …`) for modules that
//! have nothing to do with the package being compiled — verified empirically
//! by adding `ch.qos.logback:logback-classic`'s and
//! `org.junit.jupiter:junit-jupiter-api`'s module-bearing checkouts to
//! rxjava's sourcepath and watching rxjava's compile fail on logback's own
//! unrelated `requires` directives.
//!
//! # Modular targets take the other branch entirely
//!
//! A target that carries its own `module-info.java` (`gson`,
//! `jakarta.validation-api`, `logback-classic`, `junit-jupiter-api`) is
//! compiled by `javac` as a *named module*, and a named module does not read
//! `-sourcepath` packages at all — it reads the modules in its `requires`
//! closure, and fails before touching ordinary source if any of them is
//! missing. Those targets therefore get [`module_source_path_args`] instead:
//! `--module-source-path <module-name>=<dir>` for the target and every
//! module-bearing sibling, plus [`module_path_args`] for the handful of
//! modules that exist only as compiled descriptors. `javac` refuses to accept
//! `-sourcepath` and `--module-source-path` together, so this is genuinely
//! either/or rather than a union — which also means a modular target sees no
//! non-modular sibling, and is fine precisely because a named module could
//! not have read one anyway.
//!
//! # Where the compiled doclet classes come from
//!
//! `build.rs` compiles `oracle/java/Extractor.java` + `oracle/java/Json.java` with
//! `javac` into `$OUT_DIR/classes` at *this crate's* build time (see that
//! file's module doc for why a build step lives here instead of a separate
//! manual toolchain build, which is the Go producer's — and the only other
//! precedent's — approach). [`oracle_classes_dir`] reads that compile-time
//! path back via `env!("OUT_DIR")`, with a runtime escape hatch
//! (`NUDOX_JAVA_ORACLE_CLASSES`) for pointing at a hand-built classes
//! directory during doclet development — the same override shape
//! `GoProducer::invoke_oracle`'s `NUDOX_GO_ORACLE_BIN` uses for the same
//! purpose, extended here rather than invented fresh.
//!
//! # Version support (honest, not aspirational)
//!
//! * **`--release`**: unset by default, so `javadoc` uses this JDK's native
//!   level (`SourceVersion.latestSupported()` — 21 on the toolchain this crate
//!   ships against). Set `NUDOX_JAVA_RELEASE=8` (or 11, 17, …) to pin an older
//!   source level; verified empirically that `--release 8` genuinely rejects
//!   Java 16+ syntax (`record` declarations fail with `error: records are not
//!   supported in -source 8`) rather than silently accepting it, and that
//!   ordinary Java-8-era source (raw generics, no `var`, anonymous classes)
//!   processes cleanly at every level from 8 through 21.
//! * **`--add-exports`**: unset by default. `NUDOX_JAVA_ADD_EXPORTS` takes a
//!   comma-separated list of `<module>/<package>` specs, each emitted as
//!   `--add-exports <module>/<package>=ALL-UNNAMED`. See
//!   [`add_exports_args`] for why a per-invocation escape hatch rather than
//!   an always-on flag, and which real library forces the question.
//! * **JEP 467 Markdown javadoc (`///`)**: `javadoc.rs` in this crate parses
//!   both flavors, but the *oracle* can only ever hand it Markdown-flavored
//!   text on a JDK that recognizes `///` as a doc comment at all — that
//!   requires JDK 23+. Verified empirically on the JDK 21 toolchain this crate
//!   builds against: `Elements.getDocCommentKind` does not exist (probed
//!   reflectively in `Extractor.run`, degrading `docKindMethod` to `null`),
//!   and — more fundamentally — javac itself does not treat `///` as a doc
//!   comment before JDK 23, so `Elements.getDocComment` returns `null` for a
//!   `///`-documented element outright. A `///`-documented class oracled on
//!   this JDK loses its documentation entirely rather than degrading to the
//!   traditional flavor. `javadoc.rs`'s Markdown path is real, unit-tested
//!   code, but it is currently *unreachable in practice* on this toolchain —
//!   it activates automatically the day the crate is rebuilt against a JDK
//!   23+ `javadoc` binary, with no code change required here.

use std::path::{Path, PathBuf};

use crate::java::{producer::PRODUCER_ID, schema::Extraction};
use crate::{PackageSource, ProducerError, oracle};

/// Run `javadoc` with the vendored doclet over `src`'s sources and
/// deserialize its JSON document.
pub(crate) fn invoke(src: &PackageSource) -> Result<Extraction, ProducerError> {
    // `build.rs` emits this cfg when no `javac` could compile the doclet (see
    // that file's module doc). The crate still builds; only this producer is
    // degraded, and it says so as a typed spawn failure rather than dying on a
    // missing classes directory at runtime.
    if cfg!(nudox_java_oracle_unavailable) {
        return Err(ProducerError::OracleSpawn {
            command: "javadoc".to_owned(),
            reason: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the Java oracle doclet was not compiled at build time (no JDK 17+ javac); \
                 install a JDK or set NUDOX_JAVAC and rebuild",
            ),
        });
    }

    let legacy = std::env::var("NUDOX_JAVA_LEGACY").ok().as_deref() == Some("8");
    let sources = discover_java_sources(src.root(), legacy)?;

    let classes_dir = if legacy {
        legacy_oracle_classes_dir()
    } else {
        oracle_classes_dir()
    };
    let mut args: Vec<String> = vec![
        "-quiet".to_owned(),
        "-doclet".to_owned(),
        if legacy {
            "nudox.oracle.LegacyExtractor".to_owned()
        } else {
            "nudox.oracle.Extractor".to_owned()
        },
        "-docletpath".to_owned(),
        classes_dir,
    ];
    // A target that carries its own `module-info.java` is compiled as a
    // named JPMS module, and a named module can only read other *modules* —
    // `-sourcepath` package lookup does not apply to it. The two options
    // are mutually exclusive (`javac`: "cannot specify both --source-path
    // and --module-source-path"), so this is an either/or, not a union.
    // See [`module_source_path_args`].
    if !legacy && is_module_root(src.root()) {
        args.extend(module_source_path_args(src.root()));
    } else if let Some(sourcepath) = sourcepath_entries(src.root()) {
        args.push("-sourcepath".to_owned());
        args.push(sourcepath);
    }
    if !legacy {
        args.extend(module_path_args(src.root()));
        args.extend(add_modules_args());
    }
    args.extend(class_path_args(legacy)?);
    if legacy {
        args.push("-source".to_owned());
        args.push("8".to_owned());
    } else if let Ok(release) = std::env::var("NUDOX_JAVA_RELEASE") {
        args.push("--release".to_owned());
        args.push(release);
    } else if let Ok(source) = std::env::var("NUDOX_JAVA_SOURCE") {
        args.push("-source".to_owned());
        args.push(source);
    }
    if !legacy {
        args.extend(add_exports_args());
    }
    args.extend(sources.iter().map(|p| p.to_string_lossy().into_owned()));

    let javadoc_bin = if legacy {
        std::env::var("NUDOX_JAVA8_JAVADOC").unwrap_or_else(|_| "javadoc8".to_owned())
    } else {
        std::env::var("NUDOX_JAVADOC").unwrap_or_else(|_| "javadoc".to_owned())
    };

    match oracle::run_json(PRODUCER_ID, javadoc_bin, args) {
        Err(ProducerError::OracleExit {
            command,
            code,
            stderr,
        }) if let Some(missing) = unresolved_dependency_packages(&stderr) => {
            Err(ProducerError::DependenciesUnresolved {
                package: src.name.as_str().to_owned(),
                source: Box::new(UnresolvedDependencies {
                    packages: missing,
                    exit_code: code,
                    command,
                    stderr,
                }),
            })
        }
        other => other,
    }
}

/// `javadoc` exited because named packages are not on the source or class
/// path. The compiler was not relaxed: the same invocation still fails.
///
/// A language error (`records are not supported`, a syntax error) stays
/// [`ProducerError::OracleExit`] even if a missing package is also present,
/// so a real compile failure is not relabeled as an environment problem.
#[derive(Debug, thiserror::Error)]
#[error(
    "javadoc could not resolve dependency package(s) {packages}; the compiler was \
     not weakened (exit {exit_code}, {command}). Put the missing jars on \
     NUDOX_JAVA_CLASS_PATH. {stderr}"
)]
pub struct UnresolvedDependencies {
    /// Packages named by `error: package … does not exist`, sorted and joined.
    packages: String,
    /// `javadoc`'s exit status.
    exit_code: String,
    /// The command label `run_json` reported.
    command: String,
    /// The full diagnostic, so the missing types stay readable.
    stderr: String,
}

/// `Some` when every `error:` line is a missing package or a symbol that
/// follows from one. `None` when there is no missing package, or when any
/// other `error:` is present.
fn unresolved_dependency_packages(stderr: &str) -> Option<String> {
    let mut packages = Vec::new();
    for line in stderr.lines() {
        let Some((_, rest)) = line.split_once("error:") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(name) = rest
            .strip_prefix("package ")
            .and_then(|s| s.strip_suffix(" does not exist"))
        {
            packages.push(name.trim().to_owned());
            continue;
        }
        if rest.starts_with("cannot find symbol") {
            continue;
        }
        return None;
    }
    if packages.is_empty() {
        return None;
    }
    packages.sort();
    packages.dedup();
    Some(packages.join(", "))
}

/// Overrides the compiled doclet-classes directory (`-docletpath`).
///
/// Named once and used both to *read* the override and to *report* it, matching
/// [`crate::go::producer::ORACLE_BIN_ENV`]. A packaged `lindsey.app` does not
/// keep Cargo's `$OUT_DIR/classes`, so this is the variable the Nix cargo-bundle
/// wrapper sets.
pub const ORACLE_CLASSES_ENV: &str = "NUDOX_JAVA_ORACLE_CLASSES";

fn legacy_oracle_classes_dir() -> String {
    std::env::var("NUDOX_JAVA8_ORACLE_CLASSES")
        .unwrap_or_else(|_| concat!(env!("OUT_DIR"), "/java8-classes").to_owned())
}

/// The doclet's compiled `.class` directory.
///
/// Defaults to the path `build.rs` compiled into (baked in at *this crate's*
/// compile time via `env!("OUT_DIR")`); [`ORACLE_CLASSES_ENV`] overrides
/// it at runtime. See this module's doc comment for why the override exists.
fn oracle_classes_dir() -> String {
    std::env::var(ORACLE_CLASSES_ENV)
        .unwrap_or_else(|_| concat!(env!("OUT_DIR"), "/classes").to_owned())
}

/// `--add-exports <module>/<package>=ALL-UNNAMED` pairs, read from
/// `NUDOX_JAVA_ADD_EXPORTS` (comma-separated `<module>/<package>` specs;
/// unset — the default — contributes nothing).
///
/// This exists because a small number of real libraries compile *only* with
/// a JDK-internal package exported, and say so in their own build rather
/// than their POM. The corpus's live case is `org.conscrypt`, whose
/// `Platform.java` imports `sun.security.x509.AlgorithmId`; conscrypt's own
/// Gradle build gets away with that by compiling at `sourceCompatibility
/// 1.7`, i.e. before the module system enforced exports at all. Nothing
/// short of this flag makes that import resolve on a modern JDK: `--release
/// 8` does *not* work (it swaps in the `ct.sym` cross-compilation view,
/// under which `sun.security.x509` does not exist at all rather than merely
/// being unexported), and no Maven artifact can supply a package that lives
/// inside `java.base`.
///
/// Deliberately off by default and per-invocation, exactly like
/// `NUDOX_JAVA_RELEASE`: an always-on `--add-exports` would quietly make
/// this producer's compile environment more permissive than a stock
/// `javadoc`, which is the kind of difference that turns "the corpus lowers"
/// into a claim about our flags rather than about the code. Callers that
/// need it scope it to the one package that does (see
/// `tests/corpus_sweep.rs`'s `Entry::add_exports`).
///
/// The value is `ALL-UNNAMED` rather than a named module because the
/// producer never compiles the *dependency* as a module — sibling checkouts
/// resolve through `-sourcepath` into the unnamed module.
fn add_exports_args() -> Vec<String> {
    let Ok(raw) = std::env::var("NUDOX_JAVA_ADD_EXPORTS") else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .flat_map(|spec| ["--add-exports".to_owned(), format!("{spec}=ALL-UNNAMED")])
        .collect()
}

/// Builds the `-sourcepath` value: `root` itself, plus every sibling
/// directory of `root` (i.e. every other entry directly under `root`'s
/// parent) that does **not** carry its own `module-info.java` — see this
/// module's doc comment for why module-bearing siblings are excluded and why
/// `root` itself is always kept even when *it* has one.
///
/// Returns `None` when `root` has no parent or that parent cannot be read
/// (e.g. a package extracted directly at a filesystem root, or a transient
/// permission error) — `-sourcepath` is a best-effort resolution aid, not a
/// requirement, so `invoke` falls back to the pre-existing no-sourcepath
/// invocation rather than failing the whole run over it.
fn sourcepath_entries(root: &Path) -> Option<String> {
    let parent = root.parent()?;
    let siblings = std::fs::read_dir(parent).ok()?;

    let mut entries: Vec<PathBuf> = vec![root.to_path_buf()];
    for entry in siblings.flatten() {
        let path = entry.path();
        if path == root || !path.is_dir() || is_hidden(&path) {
            continue;
        }
        if is_module_root(&path) {
            continue;
        }
        entries.push(path);
    }
    entries[1..].sort();

    std::env::join_paths(&entries)
        .ok()
        .map(|joined| joined.to_string_lossy().into_owned())
}

/// A dot-prefixed directory next to the package checkouts is never a package
/// checkout — it is corpus machinery. Today the only one is
/// `result/.module-path`, the compiled-JPMS-descriptor directory (see
/// [`module_path_args`]); skipping the whole class rather than that one name
/// keeps `-sourcepath` from picking up anything a future corpus mechanism
/// puts beside the checkouts.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// Whether `dir` is a JPMS module root — i.e. holds a `module-info.java`
/// directly at its top level, which is the only place `javac` looks.
fn is_module_root(dir: &Path) -> bool {
    dir.join("module-info.java").is_file()
}

/// `--module-source-path <module>=<dir>` for the target and for every
/// module-bearing sibling checkout.
///
/// This is the invocation shape for a target that carries its own
/// `module-info.java`. Such a target is compiled as a named module, and a
/// named module reads *modules*, not `-sourcepath` packages: every name in
/// its `requires` list — including `requires static`, which is optional at
/// runtime but mandatory at compile time — has to resolve to a real module or
/// `javac` stops before it looks at a single ordinary source file
/// (`module not found: org.slf4j`, and so on).
///
/// The module-specific `<module>=<path>` form (JDK 12+) is used rather than
/// the pattern form, because this corpus's checkout directories are named
/// `groupId__artifactId-version` and a module's name is unrelated to that
/// (`ch.qos.logback__logback-classic-1.4.14` declares `ch.qos.logback.classic`).
/// The name is read out of each `module-info.java` by [`declared_module_name`].
///
/// Registering *every* module-bearing sibling, not just the ones a given
/// target happens to require, is safe: `javac` resolves lazily from the root
/// module outward, so an unrequired entry is scanned for its name and then
/// ignored. It is also what makes this a general mechanism rather than a
/// per-package table.
///
/// Modules that only exist as compiled jars come in through
/// [`module_path_args`] instead; the two are complementary and both are
/// passed.
fn module_source_path_args(root: &Path) -> Vec<String> {
    let mut roots: Vec<PathBuf> = vec![root.to_path_buf()];
    if let Some(parent) = root.parent()
        && let Ok(siblings) = std::fs::read_dir(parent)
    {
        let mut others: Vec<PathBuf> = siblings
            .flatten()
            .map(|e| e.path())
            .filter(|p| p != root && p.is_dir() && !is_hidden(p) && is_module_root(p))
            .collect();
        others.sort();
        roots.extend(others);
    }

    roots
        .iter()
        .filter_map(|dir| {
            let name = declared_module_name(&dir.join("module-info.java"))?;
            Some([
                "--module-source-path".to_owned(),
                format!("{name}={}", dir.display()),
            ])
        })
        .flatten()
        .collect()
}

/// The module name declared by a `module-info.java`, or `None` if the file
/// cannot be read or holds nothing that looks like a module declaration.
///
/// Deliberately a small scanner rather than a real parser: strip comments
/// (both flavors — every real `module-info.java` in this corpus opens with a
/// block-comment license header, and several document individual `requires`
/// with `//`), then take the identifier after the first `module` keyword. The
/// optional `open` modifier is handled by simply not caring about it: `open
/// module foo` still has `module` immediately before the name. Annotations
/// (`@Deprecated module foo`) likewise fall out for free.
fn declared_module_name(module_info: &Path) -> Option<String> {
    let text = std::fs::read_to_string(module_info).ok()?;
    let stripped = strip_java_comments(&text);
    let mut tokens = stripped.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "module" {
            let name = tokens.next()?.trim_end_matches('{');
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Removes `/* … */` and `// …` from Java source. Only used on
/// `module-info.java`, which by construction has no string or character
/// literals for a comment delimiter to hide inside — the reason this can be a
/// character scan instead of a lexer.
fn strip_java_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                // Comments separate tokens; without this, `*/module` would
                // read as one word.
                out.push(' ');
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// `--module-path <corpus>/.module-path`, when that directory exists.
///
/// This is where `nix build .#checks.corpus` puts the corpus's only compiled artifacts:
/// jars fetched *solely* so `javac` can resolve a module descriptor that has
/// no source form (see `nix/corpus.nix`'s `[[jpms_modules]]` header for
/// the three situations that arise, and why a sources jar cannot cover them).
/// They are never extracted and never on `-sourcepath` — [`is_hidden`] keeps
/// the dot-directory off it — so nothing here can be lowered.
///
/// Passing the flag whenever the directory exists is deliberate and safe: a
/// module on `--module-path` is inert unless something *resolves* it. For a
/// modular target that means appearing in its `requires` closure; for a
/// non-modular one it means being named in `--add-modules` (see
/// [`add_modules_args`]), which is off by default.
///
/// The one exception is a pre-module-system `--release`: `javac` rejects the
/// combination outright (`option --module-path not allowed with target 8`),
/// so an entry pinned below Java 9 gets no module path at all. This is not
/// hypothetical tidiness — `io.vavr:vavr` runs at `--release 8` and regressed
/// out of the corpus sweep the moment the module path was added
/// unconditionally.
fn module_path_args(root: &Path) -> Vec<String> {
    if !module_system_available() {
        return Vec::new();
    }
    let dir = root.parent().map(|p| p.join(".module-path"));
    match dir {
        Some(dir) if dir.is_dir() => {
            vec!["--module-path".to_owned(), dir.display().to_string()]
        }
        _ => Vec::new(),
    }
}

/// Whether the current `NUDOX_JAVA_RELEASE` (if any) admits the module system
/// at all. JPMS arrived in Java 9, and `javac` refuses `--module-path` /
/// `--add-modules` when targeting 8 or lower rather than ignoring them.
///
/// An unset or unparseable value means "this JDK's native level", which is
/// always ≥ 9 on any toolchain this crate can run on (the doclet API it uses
/// is itself JDK 9+).
fn module_system_available() -> bool {
    std::env::var("NUDOX_JAVA_RELEASE")
        .or_else(|_| std::env::var("NUDOX_JAVA_SOURCE"))
        .map_or(true, |level| {
            level.trim().parse::<u32>().ok().is_none_or(|n| n >= 9)
        })
}

/// `--add-modules <value>` from `NUDOX_JAVA_ADD_MODULES`; unset contributes
/// nothing.
///
/// Needed only by a **non**-modular target that has to see types from a
/// compiled module: without an explicit `--add-modules`, nothing on
/// `--module-path` enters the root module set, so the unnamed module cannot
/// read any of it. `assertj-core` is the corpus's one such case
/// (`ALL-MODULE-PATH`) — see its entry in `tests/corpus_sweep.rs`.
///
/// Off by default, per-invocation, for the same reason as
/// [`add_exports_args`]: silently widening every package's module graph would
/// make "the corpus lowers" a statement about our flags.
fn add_modules_args() -> Vec<String> {
    if !module_system_available() {
        return Vec::new();
    }
    match std::env::var("NUDOX_JAVA_ADD_MODULES") {
        Ok(value) if !value.trim().is_empty() => {
            vec!["--add-modules".to_owned(), value.trim().to_owned()]
        }
        _ => Vec::new(),
    }
}

/// `--class-path <paths>` supplied by a caller that has already decided a
/// package needs compiled *dependency* declarations to type-check.
///
/// The producer never discovers this implicitly. A caller must name every
/// jar via `NUDOX_JAVA_CLASS_PATH`, using the platform path separator. This
/// keeps binary dependencies out of the source walk and makes the exceptional
/// compile environment auditable per package. Each entry must exist and be a
/// regular file; accepting a missing path would turn a typo into a misleading
/// "unresolved type" diagnosis from `javadoc`.
fn class_path_args(legacy: bool) -> Result<Vec<String>, ProducerError> {
    let Some(raw) = std::env::var_os("NUDOX_JAVA_CLASS_PATH") else {
        if !legacy {
            return Ok(Vec::new());
        }
        let Some(tools) = std::env::var_os("NUDOX_JAVA8_TOOLS_JAR") else {
            return Ok(Vec::new());
        };
        let path = PathBuf::from(tools);
        if !path.is_file() {
            return Err(ProducerError::OracleSpawn {
                command: format!("javadoc classpath {}", path.display()),
                reason: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Java 8 tools jar is not a regular file: {}", path.display()),
                ),
            });
        }
        return Ok(vec!["-classpath".to_owned(), path.display().to_string()]);
    };
    let paths: Vec<PathBuf> = std::env::split_paths(&raw).collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    for path in &paths {
        if !path.is_file() {
            return Err(ProducerError::OracleSpawn {
                command: format!("javadoc classpath {}", path.display()),
                reason: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "NUDOX_JAVA_CLASS_PATH contains a path that is not a regular file: {}",
                        path.display()
                    ),
                ),
            });
        }
    }
    let mut paths = paths;
    if legacy
        && let Some(tools) = std::env::var_os("NUDOX_JAVA8_TOOLS_JAR") {
            let path = PathBuf::from(tools);
            if !path.is_file() {
                return Err(ProducerError::OracleSpawn {
                    command: format!("javadoc classpath {}", path.display()),
                    reason: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Java 8 tools jar is not a regular file: {}", path.display()),
                    ),
                });
            }
            paths.push(path);
        }
    let joined = std::env::join_paths(paths).map_err(|e| ProducerError::OracleSpawn {
        command: "javadoc classpath".to_owned(),
        reason: std::io::Error::new(std::io::ErrorKind::InvalidInput, e),
    })?;
    Ok(vec![
        if legacy {
            "-classpath".to_owned()
        } else {
            "--class-path".to_owned()
        },
        joined.to_string_lossy().into_owned(),
    ])
}

/// Recursively collect every `.java` file under `root`, sorted for
/// deterministic `javadoc` invocations (source order otherwise depends on
/// directory-read order, which is filesystem-dependent).
///
/// An empty result is a typed [`ProducerError::OracleSpawn`], not a silent
/// empty [`Extraction`] — a package root with zero Java sources is a
/// misconfiguration (wrong root, wrong ecosystem), and letting it through as
/// "zero declarations" would make that indistinguishable from a package that
/// genuinely has no public API (see `crate::python`'s analogous
/// discussion, referenced from `ProducerRegistry::with_all_available`'s doc
/// comment, for the general shape of this trap).
fn discover_java_sources(root: &Path, legacy: bool) -> Result<Vec<PathBuf>, ProducerError> {
    let mut out = Vec::new();
    walk_java_sources(root, &mut out, legacy)?;
    out.sort();
    if out.is_empty() {
        return Err(ProducerError::OracleSpawn {
            command: format!("javadoc (no .java sources found under {})", root.display()),
            reason: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no .java source files found under the package root",
            ),
        });
    }
    Ok(out)
}

fn walk_java_sources(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    legacy: bool,
) -> Result<(), ProducerError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ProducerError::OracleSpawn {
        command: format!("read_dir {}", dir.display()),
        reason: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ProducerError::OracleSpawn {
            command: format!("read_dir entry under {}", dir.display()),
            reason: e,
        })?;
        let path = entry.path();
        if path.is_dir() {
            // Skip dotfiles/dotdirs (`.git`, …) — never part of a source set.
            let is_hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if !(is_hidden || legacy && is_legacy_excluded_dir(&path)) {
                walk_java_sources(&path, out, legacy)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("java")
            && !(legacy && is_legacy_excluded_file(&path)) {
                out.push(path);
            }
    }
    Ok(())
}

fn is_legacy_excluded_dir(path: &Path) -> bool {
    path.ends_with("lombok/eclipse")
        || path.ends_with("lombok/bytecode")
        || path.ends_with("lombok/core/configuration")
        || path.ends_with("lombok/core/debug")
        || path.ends_with("lombok/javac/java6")
        || path.ends_with("lombok/javac/java7")
        || path.ends_with("lombok/javac/java8")
        || path.ends_with("lombok/javac/java9")
}

#[cfg(test)]
mod tests {
    use super::unresolved_dependency_packages;

    #[test]
    fn a_missing_package_is_an_unresolved_dependency() {
        let stderr = "\
App.java:1: error: package com.google.common.base does not exist
import com.google.common.base.Preconditions;
App.java:4: error: cannot find symbol
        Preconditions.checkNotNull(name);
  symbol:   variable Preconditions
1 error
";
        assert_eq!(
            unresolved_dependency_packages(stderr).as_deref(),
            Some("com.google.common.base")
        );
    }

    #[test]
    fn a_language_error_stays_a_compiler_failure_even_beside_a_missing_package() {
        let stderr = "\
App.java:3: error: records are not supported in -source 8
App.java:1: error: package com.google.common.base does not exist
";
        assert!(unresolved_dependency_packages(stderr).is_none());
    }

    #[test]
    fn a_bare_cannot_find_symbol_is_not_relabeled_as_a_missing_dependency() {
        let stderr = "App.java:2: error: cannot find symbol\n  symbol: class Typo\n";
        assert!(unresolved_dependency_packages(stderr).is_none());
    }
}

fn is_legacy_excluded_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            name.starts_with("Test")
                || name.starts_with("AbstractTest")
                || (name.starts_with("Run") && name.contains("Test"))
                || name == "PatchFixesHider.java"
        })
}
