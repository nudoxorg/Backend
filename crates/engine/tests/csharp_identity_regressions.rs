//! Regression coverage for C# declaration identity and reference ownership.
//!
//! Each test pins one erasure that once turned a real MoreLinq source row
//! into a typed terminal. They run the real vendored Roslyn oracle over a
//! small source that reproduces the shape, and the two corpus-guarded tests
//! lower the exact package files when `NUDOX_CSHARP_CORPUS_DIR` is
//! provisioned.
//!
//! The tests are deliberately self-contained: they never fetch a package.
//! When no dotnet is available they return rather than fabricating a pass or
//! a failure, because the toolchain is an environment fact and not a claim
//! about the identity lane.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_frontend_csharp::legacy::{DeclarationKind, PartialRole};
use backend_semantic::vocabulary::{CSharpVersion, LanguageProfile, Stage};

const PROFILE: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);

/// Every callable collision class at once: a static and a parameterless
/// instance constructor share the containing-type name; `Probe<T>` and
/// `Probe<T, U>` erase to the same callable signature; and `Left`/`Right`
/// share the `errorSelector` carrier name and declared type.
const CALLABLE_SHAPES: &[u8] = br#"
namespace IdentityProbe;

public class Probe
{
    static Probe() { }
    public Probe() { }
    public void Generic<T>(int value) { _ = value; }
    public void Generic<T, U>(int value) { _ = value; }
    public int Left(int errorSelector) => errorSelector;
    public int Right(int errorSelector) => errorSelector;
}
"#;

/// The `MoreEnumerable.GroupAdjacent.cs` erasure: two same-named nested types
/// whose every structural cell matches except type-parameter arity. Both
/// nested `Grouping` declarations once folded into one
/// `DuplicateDeclarationIdentity` row; the named-type arity discriminator is
/// what keeps them distinct.
const NESTED_ARITY: &[u8] = br#"
namespace IdentityProbe;

public static class Host
{
    static class Grouping { }

    private sealed class Grouping<TKey, TElement> { }
}
"#;

/// The `MoreEnumerable.ToDelimitedString.g.cs` erasure: fifteen same-tree
/// parts of one nested static class, whose field initializers are reference
/// sites. The oracle once attributed those initializers' occurrences to the
/// enclosing partial part row; later parts are implementation rows the
/// lowerer never pushes, so the first such occurrence died as
/// `IndexCapacity { phase: Reference }`. Occurrences must own to the true
/// nearest declaration — the field declarator — and every part must still
/// lower under the definition row without truncation.
const CROSS_PART_REFERENCES: &[u8] = br#"
namespace IdentityProbe;

public static class Enumerable
{
    public static string Render(bool value) => Append.Flag(value);

    private static partial class Append
    {
        public static readonly System.Func<bool, string> Flag = v => v.ToString(System.Globalization.CultureInfo.InvariantCulture);
    }

    private static string Render(int value, string delimiter) => Append.Number(value);

    private static partial class Append
    {
        public static readonly System.Func<int, string> Number = v => v.ToString(System.Globalization.CultureInfo.InvariantCulture);
    }

    private static partial class Append { }
}
"#;

const GROUP_ADJACENT_CORPUS_RELATIVE: &str = "morelinq.source.moreenumerable.groupadjacent/1.0.1/content/net20/MoreLinq/MoreEnumerable.GroupAdjacent.cs";
const TO_DELIMITED_STRING_CORPUS_RELATIVE: &str = "morelinq.source.moreenumerable.todelimitedstring/1.1.2/content/net20/MoreLinq/MoreEnumerable.ToDelimitedString.g.cs";
const TINYIOC_CORPUS_RELATIVE: &str = "tinyioc/1.4.0-rc1/content/TinyIoc.cs";

fn dotnet() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").as_deref().and_then(|paths| {
        std::env::split_paths(paths)
            .map(|dir| dir.join("dotnet"))
            .find(|candidate| candidate.is_file())
    })?;
    let status = Command::new(&path).arg("--version").status().ok()?;
    status.success().then_some(path)
}

fn corpus_source(relative: &str) -> Option<Vec<u8>> {
    let root = std::env::var_os("NUDOX_CSHARP_CORPUS_DIR")?;
    fs::read(PathBuf::from(root).join(relative)).ok()
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "nudox-csharp-regression-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("fixture directory");
    path
}

/// Publishes the vendored helper once per test process and returns the
/// published `oracle.dll`. Every test shares one publish because the helper
/// source is fixed for the run; `None` means the publish failed, which is an
/// environment fact the caller reports as a skip.
fn published_oracle(dotnet: &Path) -> Option<&'static PathBuf> {
    static ORACLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ORACLE
        .get_or_init(|| {
            let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../frontends/csharp/src/legacy/helper");
            let root = fresh_dir("publish");
            let publish = root.join("publish");
            fs::create_dir_all(&publish).ok()?;
            let status = Command::new(dotnet)
                .args([
                    "publish",
                    "oracle.csproj",
                    "-c",
                    "Release",
                    "--nologo",
                    "--no-restore",
                    "-o",
                ])
                .arg(&publish)
                .current_dir(&helper)
                .status()
                .ok()?;
            (status.success() && publish.join("oracle.dll").is_file())
                .then_some(publish.join("oracle.dll"))
        })
        .as_ref()
}

/// Produces one source-bound authority image over a scratch binding.
fn oracle_image(dotnet: &Path, source: &[u8]) -> Option<Vec<u8>> {
    let oracle = published_oracle(dotnet)?;
    let root = fresh_dir("oracle");
    let source_root = root.join("root");
    fs::create_dir_all(&source_root).ok()?;
    let binding = source_root.join("Package.cs");
    fs::write(&binding, source).ok()?;
    let image = root.join("authority.image");
    let status = Command::new(dotnet)
        .arg("exec")
        .arg(oracle)
        .arg("--mode")
        .arg("source")
        .arg("--assembly-name")
        .arg("IdentityProbe")
        .arg("--authority-image")
        .arg("--source-binding")
        .arg(&binding)
        .arg("--out")
        .arg(&image)
        .arg("--root")
        .arg(&source_root)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    fs::read(&image).ok()
}

#[test]
fn distinct_callable_shapes_do_not_collapse() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    lower_to_completion(&dotnet, "callable-shapes", CALLABLE_SHAPES);
}

#[test]
fn same_named_nested_types_differing_only_in_generic_arity_do_not_collapse() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let image = oracle_image(&dotnet, NESTED_ARITY).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let groupings: Vec<_> = authority
        .declarations()
        .filter_map(|declared| declared.ok())
        .filter(|declared| declared.kind == DeclarationKind::Class)
        .filter(|declared| declared.name.bytes == b"Grouping")
        .collect();
    assert_eq!(
        groupings.len(),
        2,
        "the fixture must carry both arity spellings of the nested type"
    );
    let mut arities: Vec<usize> = groupings
        .iter()
        .map(|declared| declared.type_parameters.iter().count())
        .collect();
    arities.sort_unstable();
    assert_eq!(arities, vec![0, 2], "arity pair must erase-ambiguously");
    lower_to_completion(&dotnet, "nested-arity", NESTED_ARITY);
}

#[test]
fn occurrences_inside_later_partial_parts_own_to_their_true_declaration() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let image = oracle_image(&dotnet, CROSS_PART_REFERENCES).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let implementation_parts: Vec<_> = authority
        .declarations()
        .filter_map(|declared| declared.ok())
        .filter(|declared| declared.name.bytes == b"Append")
        .filter(|declared| declared.partial == PartialRole::Implementation)
        .collect();
    assert!(
        !implementation_parts.is_empty(),
        "the fixture must carry at least one same-tree implementation part"
    );
    let occurrences = authority.references().len();
    assert!(
        occurrences > 0,
        "field initializers inside the later parts must be reference sites"
    );
    lower_to_completion(&dotnet, "cross-part-references", CROSS_PART_REFERENCES);
}

#[test]
fn real_corpus_groupadjacent_lowers_with_its_arity_pair() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let Some(source) = corpus_source(GROUP_ADJACENT_CORPUS_RELATIVE) else {
        return;
    };
    let image = oracle_image(&dotnet, &source).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let groupings: Vec<_> = authority
        .declarations()
        .filter_map(|declared| declared.ok())
        .filter(|declared| declared.kind == DeclarationKind::Class)
        .filter(|declared| declared.name.bytes == b"Grouping")
        .collect();
    assert_eq!(
        groupings.len(),
        2,
        "the corpus file must hold Grouping and Grouping<TKey, TElement>"
    );
    lower_to_completion(&dotnet, "corpus-groupadjacent", &source);
}

#[test]
fn real_corpus_todelimitedstring_lowers_across_its_partial_parts() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let Some(source) = corpus_source(TO_DELIMITED_STRING_CORPUS_RELATIVE) else {
        return;
    };
    let image = oracle_image(&dotnet, &source).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let parts: Vec<_> = authority
        .declarations()
        .filter_map(|declared| declared.ok())
        .filter(|declared| declared.name.bytes == b"StringBuilderAppenders")
        .collect();
    assert!(
        parts.len() > 1
            && parts
                .iter()
                .any(|declared| declared.partial == PartialRole::Implementation),
        "the generated corpus file must hold several same-tree partial parts"
    );
    lower_to_completion(&dotnet, "corpus-todelimitedstring", &source);
}

/// The real tinyioc audit row: same-file classes (`DelegateFactory`,
/// `ResolveOptions`) whose constructors the source never declares must lower
/// to local occurrence targets, and genuinely external creations
/// (`ArgumentNullException`) must stay foreign. Fails on any lane terminal.
#[test]
fn real_corpus_tinyioc_same_file_creations_lower_to_local_targets() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let Some(source) = corpus_source(TINYIOC_CORPUS_RELATIVE) else {
        return;
    };
    let source = Box::leak(source.into_boxed_slice());
    let image = oracle_image(&dotnet, source).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let declared: Vec<Vec<u8>> = authority
        .declarations()
        .filter_map(|row| row.ok())
        .map(|row| row.name.bytes.to_vec())
        .collect();
    let creations: Vec<(Vec<u8>, Option<u32>)> = authority
        .references()
        .filter_map(|row| row.ok())
        .filter(|row| row.kind == backend_frontend_csharp::legacy::ReferenceTag::ObjectCreation)
        .map(|row| (row.spelling.bytes.to_vec(), row.target))
        .collect();
    let creation_target = |spelling: &[u8]| {
        creations
            .iter()
            .find(|(written, _)| written == spelling)
            .unwrap_or_else(|| panic!("tinyioc must create {spelling:?}"))
            .1
    };
    let local_row = |spelling: &[u8]| {
        creation_target(spelling)
            .map(|row| {
                declared
                    .get(row as usize)
                    .is_some_and(|name| name == spelling)
            })
            .unwrap_or(false)
    };
    assert!(
        local_row(b"DelegateFactory") && local_row(b"ResolveOptions"),
        "same-file creations must target their declared local rows"
    );
    assert_eq!(
        creation_target(b"ArgumentNullException"),
        None,
        "external creations must stay foreign"
    );
    lower_to_completion(&dotnet, "corpus-tinyioc", source);
}

/// Lowers one source through the fused compiler boundary and requires the
/// declared facts. A typed fault here is the exact regression each pinned
/// cause once produced.
fn lower_to_completion(dotnet: &Path, label: &str, source: &[u8]) {
    let image = oracle_image(dotnet, source).expect("Roslyn authority image");
    backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::CSharpCompiler,
        dotnet,
        b"csharp-identity-regression",
    )
    .expect("dotnet path is absolute");
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 4 * 1024 * 1024];
    let work = fresh_dir(label);
    let result = compile_semantic(
        CompileRequest {
            profile: PROFILE,
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::CSharp { image: &image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    );
    let _ = fs::remove_dir_all(&work);
    match result {
        Ok(_) => {}
        Err(error) => panic!("{label} failed to lower: {error:?}"),
    }
}

/// A same-file pair that only references each other through object
/// creations, whose constructors the source never declares. Roslyn binds
/// the creation's type site to the invoked constructor, and the implicit
/// constructor has no declaration row of its own — the erasure that sent
/// every such reference to a foreign universe key. The helper must target
/// the declared containing type instead, and an external creation must
/// stay honestly foreign.
const MUTUAL_REFERENCES: &[u8] = br#"
namespace IdentityProbe;

public class Alpha
{
    public Beta Make() => new Beta();
}

public class Beta
{
    public Alpha Back() => new Alpha();
}

public static class Maker
{
    public static Exception Boom() => new Exception("boom");
}
"#;

#[test]
fn same_file_object_creations_resolve_to_their_declared_types() {
    let Some(dotnet) = dotnet() else {
        return;
    };
    let image = oracle_image(&dotnet, MUTUAL_REFERENCES).expect("Roslyn authority image");
    let authority =
        backend_frontend_csharp::legacy::CSharpImage::open(&image).expect("valid image");
    let declarations: Vec<_> = authority
        .declarations()
        .collect::<Result<Vec<_>, _>>()
        .expect("declaration plane");
    let row_of = |name: &[u8]| {
        declarations
            .iter()
            .position(|declared| declared.name.bytes == name)
            .expect("declared row")
    };
    let references: Vec<_> = authority
        .references()
        .collect::<Result<Vec<_>, _>>()
        .expect("reference plane");
    let creations: Vec<_> = references
        .iter()
        .filter(|reference| {
            reference.kind == backend_frontend_csharp::legacy::ReferenceTag::ObjectCreation
        })
        .collect();
    let creation_target = |spelling: &[u8]| {
        creations
            .iter()
            .find(|reference| reference.spelling.bytes == spelling)
            .unwrap_or_else(|| panic!("a creation spelling {spelling:?} must exist"))
            .target
    };
    assert_eq!(
        creation_target(b"Beta"),
        Some(row_of(b"Beta") as u32),
        "the same-file creation must fold to the declared type's local row"
    );
    assert_eq!(
        creation_target(b"Alpha"),
        Some(row_of(b"Alpha") as u32),
        "the same-file creation must fold to the declared type's local row"
    );
    assert_eq!(
        creation_target(b"Exception"),
        None,
        "an external creation must stay honestly foreign"
    );
}
