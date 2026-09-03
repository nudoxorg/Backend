# Card J9 — java lifecycle integration (P15/P17/P18)

## Registered role

`nudox_luna_implementer`. You implement one frozen proof card. You do not choose
product architecture, do not widen paths, do not change any proof-matrix row.

## Baseline and workspace

- Worktree: `/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-lane`,
  detached HEAD at commit `0cc4d167` (verify with `git rev-parse HEAD` before
  the first edit; if it differs, STOP and report).
- Build environment for every gate:
  `CARGO_TARGET_DIR=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-gate-target`
  and `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  (`"$NUDOX_JDK/bin/javac" -version` must print 21.0.12.1).
- The worktree carries quarantine modifications to FOREIGN files (clang, csharp,
  typescript, rust, python, Cargo.lock, node_modules, evidence). Never stage,
  commit, revert, or reformat them. Stage only your two owned paths.

## Owned paths (exclusive write custody)

1. `compiler/driver/tests/java_lifecycle.rs` (new)
2. `compiler/driver/tests/java_lifecycle/mod.rs` (new; import from the test
   file via `#[path = "java_lifecycle/mod.rs"] mod support;`)

Nothing else. `Cargo.toml` needs no change (dev-dependencies already include
flate2, ureq, publication, journal, and index crates).

## Public terminal

`cargo test -p compiler-driver --test java_lifecycle` runs three tests, all
green in the recorded environment:

**T1 `offline_local_repo_purl_to_published_index`** — no network. The test
builds its own fixture: two tiny real Java sources (inline string constants,
≤ 40 lines each) that together contain a checked `throws` clause, one runtime
annotation applied to a class and a method, one record with two components and
a javadoc comment containing a `{@link}`, and one overload pair. The support
module packages them into a real ZIP (`{artifact}-{version}-sources.jar`,
stored entries, one entry deflated via the dev-dependency `flate2`, correct
CRC-32) and also places a minimal binary jar file at
`{root}/{group/path}/{artifact}/{version}/{artifact}-{version}.jar` (content
unused; layout proof only). Journey, in this order, each stage through public
APIs: `MavenCoordinates::parse` (read `purl.rs` for the accepted input
spelling) → `Repository::{jar, sources_jar}` asserting both resolve to existing
files → `Jar::parse` on the sources jar bytes → extract `.java` entries with
`is_safe_relative_path` → `JdkToolchain::from_env` (or explicit root) +
`Harness::image` with `classpath: &[]` → `compile` with
`SemanticAuthorityInput::Java { image }`, profile
`LanguageProfile::Java(JavaRelease::Java21)`, stage `Stage::LowerIr` →
`FragmentView::validate` on the fragment → `publish_compiled` → journal
shutdown → `DurablePublisher::reopen` → `open_published` → assertions on the
reopened fragment view: class entity, record entity, ≥ 1 Parameter carrier,
non-empty throws list, annotation atoms, record-component facts (study the
`java_facts`/extension access patterns in the `compiler/driver/lower/java.rs`
test module and `compiler/driver/tests/java_support/mod.rs`) →
`server_index_build::build` + `seal_compilation_index` + `plan_index_pack` +
`encode_index_pack`, asserting the class name appears in the built rows. Then
P18: compile one more source (append a third tiny source, recompile, publish
with `PublishControl::Continue`), reopen again, assert the newest generation
contains the new entity and the generation numbers differ; then re-open the
FIRST generation via `ImmutableManifestStore` + `ImmutableArtifactStore` and
assert SHA-256 equality with the first fragment bytes plus
`FragmentView::validate` success. Remove every temp dir on success AND failure;
surface `Harness::take_cleanup_error`.

**T2 `central_commons_lang3_slice_journey`** — live network.
`Central::default()` fetch of `org.apache.commons:commons-lang3:3.14.0`:
sources jar + binary jar via `sources_jar_url` / `jar_url` / `fetch` into
caller buffers. `Jar::parse` the sources jar and extract EXACTLY this frozen
slice (fail typed naming any missing path; never substitute):
`org/apache/commons/lang3/StringUtils.java`, `CharUtils.java`, `ArchUtils.java`,
`Arch.java`, `SystemUtils.java`, `RandomStringUtils.java`,
`builder/ToStringBuilder.java`, `builder/EqualsBuilder.java`,
`builder/HashCodeBuilder.java`, `tuple/Pair.java`, `mutable/MutableInt.java`,
`function/Suppliers.java` (all under `org/apache/commons/lang3/`). Write the
binary jar to the temp dir and run `Harness::image` with `sources` = ALL 12
extracted files, each as a `JavaSource { name, bytes }` in the frozen order
above, and `classpath: [binary jar path]`. Feeding javac fewer than all 12
extracted sources fails P17 even if every extraction happened. Then the same
compile → validate → publish → reopen → open → index sequence as T1. Assertions on the reopened fragment:
entity `StringUtils` exists; ≥ 2 distinct overload signatures among
`StringUtils` methods; ≥ 1 atom whose bytes are exactly `maven` (ecosystem
foreign keys); occurrences lane non-empty; ≥ 1 structural compound type row
(generic application) exists in the fragment's type facts. Bounded: slice
≤ 12 files; image output buffer 16 MiB; fragment output buffer 16 MiB; whole
test < 120 s.

**T3 `central_missing_artifact_is_typed`** — `Central::new("https://repo1.maven.org/maven2")`
with coordinates `demo:missing@9.9.9`; `fetch` of the resulting jar URL must
return a typed `FetchError` (read `central.rs` for the exact variant; the HTTP
404 maps to the typed error — no panic, no `Ok`).

## Proof-matrix rows and falsifiers

- P17: killed if any stage is bypassed (hand-built fragment, faked publication,
  skipped index). Comment the exact stage sequence at the top of the test file.
- P18: killed if the first-generation re-read fails validation or digest
  equality after a second publication.
- P15 live evidence: T2/T3 exercise live Central; correctness comes from typed
  assertions only.

## Semantic and resource bounds

- No production edits; no new dependencies; no POM/XML parsing; no
  `unwrap`/`expect`/`panic!` (match `python_purl_lifecycle.rs`:
  `#![forbid(unsafe_code)] #![deny(clippy::expect_used, clippy::panic,
  clippy::unwrap_used)]`).
- Never return `Ok(())` on missing environment: absent `NUDOX_JDK` → typed
  error naming the variable; network failure in T2/T3 → typed error retaining
  the cause. Silent skips are forbidden.
- Temp dirs under `std::env::temp_dir()` with process-unique names, removed on
  success and failure.
- Tests pass twice consecutively (`--test-threads=1` and default parallel).
- LOC budget: `java_lifecycle.rs` ≤ 450; `java_lifecycle/mod.rs` ≤ 550. If a
  stage cannot fit, STOP and report; do not silently exceed.

## Forbidden adjacent surface

Production code of any crate; other test files; shared driver lanes;
`compiler/languages/java/**`; doclet; evidence files (Terra owns evidence).

## Exact gates (all green at your checkpoint)

1. `cargo check -p compiler-driver -p compiler-languages-java --tests`
2. `cargo test -p compiler-driver --test java_lifecycle`
3. `cargo test -p compiler-driver --test java_image` and
   `cargo test -p compiler-driver --test java_render`
4. `cargo test -p compiler-driver --lib lower::java`
5. `cargo test -p compiler-languages-java`
6. `rustfmt --check --edition <workspace edition>` on your two files only
   (foreign quarantine files are not yours to fix).

## API inventory (verified facts — read each source before coding)

- purl: `compiler/languages/java/purl.rs` — `MavenCoordinates::parse`;
  borrowed `group`/`artifact`/`version`.
- repo: `compiler/languages/java/repo.rs` — `Repository::new(&Path)`,
  `.jar(coords)`, `.sources_jar(coords)`, `LocateError`.
- central: `compiler/languages/java/central.rs` — `Central::new(base)` /
  `Central::default()`, `.jar_url`, `.sources_jar_url`, `.fetch(url, &mut Vec<u8>)`,
  `FetchError`.
- jar: `compiler/languages/java/jar.rs` — `Jar::parse(&bytes)`, `.entries()` →
  `JarEntry` (`.name()`, `.is_safe_relative_path()`, `.data(&mut Vec<u8>)`).
- harness: `compiler/languages/java/harness.rs` — `JdkToolchain::from_env()` /
  `new(&Path)`; `JavaSource { name: &Path, bytes: &[u8] }`; `HarnessRequest {
  sources, classpath: &[&Path], release: JavaRelease }`; `Harness::new()`,
  `.image(&toolchain, request, &mut Vec<u8>)`, `.take_cleanup_error()`.
- driver admission: mirror `compiler/driver/tests/java_image.rs` —
  `compile(CompileRequest { profile, stage, source, toolchain:
  ToolchainSelection::ResolvedNative(ResolvedToolchain::from_version(
  NativeTool::JavaCompiler, Path::new("/usr/bin/true"), b"…")?), authority:
  SemanticAuthorityInput::Java { image }, control: CompileControl { deadline,
  cancelled } }, CompileScratch { diagnostic_output, native_work },
  CompileOutput { fragment_output })`.
- publication/index tail: mirror `compiler/driver/tests/python_purl_lifecycle.rs`
  from `DurablePublisher::create` through `encode_index_pack`, including
  `ImmutableManifestStore`/`ImmutableArtifactStore` first-generation re-read.
- JavaFacts extension access from a validated `FragmentView`: study
  `compiler/driver/lower/java.rs` test module (`java_extension`, `record`,
  `throws_entries_create_parameter_carriers_and_an_ordered_type_list`,
  `annotations_decode_the_exact_interned_atom_bytes_for_class_and_method`,
  `record_components_attach_after_fields_and_retain_record_annotations`) and
  `compiler/driver/tests/java_support/mod.rs`.

## Stop decisions (report back; do not improvise)

- A frozen slice file missing from commons-lang3 3.14.0 → STOP, report the
  missing list; Terra refreezes the slice.
- javac requires classpath entries beyond the artifact binary jar for the slice
  → STOP, report the exact javac diagnostics; do NOT fetch extra artifacts or
  implement POM resolution.
- An extension-plane fact cannot be reached through public APIs from a
  validated `FragmentView` → STOP with the exact API gap.
- Image output exceeds 16 MiB or fragment output exceeds 16 MiB → STOP with
  observed sizes.
- `git rev-parse HEAD` ≠ `0cc4d167` → STOP.

## Commit and return

One commit on top of `0cc4d167` in the worktree, message:
`test(java): purl lifecycle from local repo and central to sealed index`.
Stage ONLY your two paths. Return: commit hash; one-line output per gate; LOC
count per file; the smallest remaining red row; any stop decision hit.

Plan closure: the next manager decision after this card is Terra reproduction
of the three tests plus corpus decomposition (J10).
