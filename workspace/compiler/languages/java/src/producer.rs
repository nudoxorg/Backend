//! [`Producer`] implementation for the Java `javadoc`-doclet oracle.
//!
//! # Invocation contract
//!
//! Java is one of the three external-toolchain subprocess producers (Go, C#,
//! Java — see `nudox_producer::oracle`'s module doc), so [`invoke`] uses the
//! shared [`nudox_producer::oracle::run_json`] helper: spawn a command,
//! capture stdout/stderr, treat a non-zero exit as a typed error, and
//! deserialize stdout as the oracle's JSON document ([`schema::Extraction`]).
//!
//! The command is:
//!
//! ```text
//! javadoc -quiet -doclet nudox.oracle.Extractor -docletpath <classes> \
//!     [-sourcepath <root>:<sibling dirs>] [--release <N>] \
//!     <every .java file under PackageSource::root, recursively>
//! ```
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
//! under `.real-crates/`, hash-verified — see `corpus/manifest.toml`). The
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
//! now a corpus entry (see `corpus/manifest.toml`), and [`invoke`] passes
//! `-sourcepath` so `javadoc` can find it (and anything else already
//! fetched) as a sibling checkout, instead of hand-wiring one classpath
//! entry per package.
//!
//! [`sourcepath_entries`] builds that `-sourcepath` value from whatever
//! directories happen to sit beside `PackageSource::root()` — it does not
//! know or care that they came from `corpus/manifest.toml`; any caller that
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
//! unrelated `requires` directives. The target package's *own* root is always
//! included even if it carries a `module-info.java` (`gson` and
//! `jakarta.validation-api` both do, and both must keep working) — only
//! *other* packages' module roots are excluded.
//!
//! # Where the compiled doclet classes come from
//!
//! `build.rs` compiles `oracle/Extractor.java` + `oracle/Json.java` with
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

use nudox_ir::body::Language;
use nudox_ir::lower::Lowering;
use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId, oracle};

use crate::{
    lower::{JavaId, LoweringCtx, lower_extraction},
    schema::Extraction,
};

/// The Java producer: `javadoc` + the vendored `nudox.oracle.Extractor`
/// doclet, lowered by [`crate::lower`].
///
/// Stateless by design (see `Producer`'s trait doc) — all per-run
/// configuration (release level, doclet classes location, `javadoc` binary)
/// is read from the environment inside [`invoke`], not carried on `self`,
/// exactly like `GoProducer`'s equivalent knobs.
#[derive(Debug, Default, Clone, Copy)]
pub struct JavaProducer;

impl JavaProducer {
    /// Construct a new Java producer.
    pub fn new() -> Self {
        JavaProducer
    }
}

impl Producer for JavaProducer {
    type Id = JavaId;
    type Oracle = Extraction;

    const ID: ProducerId = ProducerId("java-javadoc/1");
    const LANGUAGE: Language = Language::Java;

    fn invoke(&self, src: &PackageSource) -> Result<Extraction, ProducerError> {
        let sources = discover_java_sources(src.root())?;

        let classes_dir = oracle_classes_dir();
        let mut args: Vec<String> = vec![
            "-quiet".to_owned(),
            "-doclet".to_owned(),
            "nudox.oracle.Extractor".to_owned(),
            "-docletpath".to_owned(),
            classes_dir,
        ];
        if let Some(sourcepath) = sourcepath_entries(src.root()) {
            args.push("-sourcepath".to_owned());
            args.push(sourcepath);
        }
        if let Ok(release) = std::env::var("NUDOX_JAVA_RELEASE") {
            args.push("--release".to_owned());
            args.push(release);
        }
        args.extend(sources.iter().map(|p| p.to_string_lossy().into_owned()));

        let javadoc_bin = std::env::var("NUDOX_JAVADOC").unwrap_or_else(|_| "javadoc".to_owned());

        oracle::run_json(Self::ID.0, javadoc_bin, args)
    }

    fn lower(&self, oracle: &Extraction, out: &mut Lowering<JavaId>) -> Result<(), ProducerError> {
        let mut ctx = LoweringCtx::new(out, &oracle.types);
        lower_extraction(&mut ctx, oracle);
        Ok(())
    }
}

/// The doclet's compiled `.class` directory.
///
/// Defaults to the path `build.rs` compiled into (baked in at *this crate's*
/// compile time via `env!("OUT_DIR")`); `NUDOX_JAVA_ORACLE_CLASSES` overrides
/// it at runtime. See this module's doc comment for why the override exists.
fn oracle_classes_dir() -> String {
    std::env::var("NUDOX_JAVA_ORACLE_CLASSES")
        .unwrap_or_else(|_| concat!(env!("OUT_DIR"), "/classes").to_owned())
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
        if path == root || !path.is_dir() {
            continue;
        }
        if path.join("module-info.java").is_file() {
            continue;
        }
        entries.push(path);
    }

    std::env::join_paths(&entries)
        .ok()
        .map(|joined| joined.to_string_lossy().into_owned())
}

/// Recursively collect every `.java` file under `root`, sorted for
/// deterministic `javadoc` invocations (source order otherwise depends on
/// directory-read order, which is filesystem-dependent).
///
/// An empty result is a typed [`ProducerError::OracleSpawn`], not a silent
/// empty [`Extraction`] — a package root with zero Java sources is a
/// misconfiguration (wrong root, wrong ecosystem), and letting it through as
/// "zero declarations" would make that indistinguishable from a package that
/// genuinely has no public API (see `nudox-producer-python`'s analogous
/// discussion, referenced from `ProducerRegistry::with_all_available`'s doc
/// comment, for the general shape of this trap).
fn discover_java_sources(root: &Path) -> Result<Vec<PathBuf>, ProducerError> {
    let mut out = Vec::new();
    walk_java_sources(root, &mut out)?;
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

fn walk_java_sources(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ProducerError> {
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
            if !is_hidden {
                walk_java_sources(&path, out)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
            out.push(path);
        }
    }
    Ok(())
}
