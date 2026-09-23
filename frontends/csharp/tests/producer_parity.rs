//! Source-truth parity and structural falsifiers for the Roslyn authority image.
use backend_frontend_csharp::legacy::{
    CSharpImage, DeclarationKind, ImageError, PartialRole, ReferenceTag, Section, probe_dotnet,
    probe_dotnet_path,
};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SOURCE: &[u8] = include_bytes!("fixtures/producer/fidelity.cs");
/// Regenerated 2026-09 through the locked-restore flow with the net8.0-pinned
/// helper (SDK 8.0.422, Roslyn 5.6.0). The prior golden was produced by a
/// net10-era helper; the only delta is the References plane, 15 → 19 rows:
/// the current extractor now records the four bare-identifier const-field
/// reads of `Answer` (`ReferenceTag::FieldRead`) that the older build never
/// emitted. Every declaration, parameter, type, attribute, and doc row is
/// byte-identical. The fixture itself is unchanged: its `allows ref struct`
/// constraint (fidelity.cs:62) still fails to bind under net8.0 with
/// recovered error CS9240 ("Target runtime doesn't support by-ref-like
/// generics"), but that constraint is unobservable in the image — the
/// TypeConstraints plane is empty in both goldens — so the fixture still
/// round-trips faithfully without modification.
const IMAGE: &[u8] = include_bytes!("fixtures/producer/fidelity.ncaimg");
const UNICODE_SOURCE: &[u8] = include_bytes!("fixtures/producer/unicode.cs");
const UNICODE_IMAGE: &[u8] = include_bytes!("fixtures/producer/unicode.ncaimg");
const DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v4\0";

/// Strips the machine-specific absolute-path prefix the Roslyn oracle bakes
/// into every atom naming the bound source file, leaving only the portion
/// from `marker` onward.
///
/// `SourceLoader.CollectSourceFiles` resolves every root/source file through
/// `Path.GetFullPath` before Roslyn parses it, and `AuthorityImage` embeds
/// that exact `tree.FilePath` verbatim (`Atom(tree.FilePath)`). That is
/// correct for the real production pipeline
/// (`CSharpAuthorityProducer::authority_image` canonicalizes and passes real
/// absolute paths too), but it means a byte-exact checked-in golden can only
/// ever match a regeneration performed from the *exact same absolute
/// checkout path* it was captured from. This repository runs many
/// concurrent git worktrees at different absolute paths, and the committed
/// `fidelity.ncaimg`/`unicode.ncaimg` fixtures were captured from a
/// `backend-fix-csharp` worktree, so a raw byte compare mismatches from any
/// other checkout — including the one the gate itself builds from.
/// Normalizing both sides to the checkout-independent relative form restores
/// a comparison that verifies the extraction is genuinely byte-reproducible
/// (every declaration, span, type, reference and doc row still compares
/// byte-for-byte) without depending on where the repository happens to live
/// on disk.
fn normalize_authority_image_paths(bytes: &[u8], marker: &str) -> Vec<u8> {
    const HEADER_BYTES: usize = 256;
    const DIRECTORY_OFFSET: usize = 48;
    const DIRECTORY_ENTRY_BYTES: usize = 16;
    const SECTION_COUNT: usize = 11;
    const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v4\0";

    struct DirEntry {
        tag: u16,
        row_bytes: u16,
        count: u32,
        offset: u32,
        byte_count: u32,
    }

    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);

    let entries: Vec<DirEntry> = (0..SECTION_COUNT)
        .map(|index| {
            let at = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
            DirEntry {
                tag: u16_at(bytes, at),
                row_bytes: u16_at(bytes, at + 2),
                count: u32_at(bytes, at + 4),
                offset: u32_at(bytes, at + 8),
                byte_count: u32_at(bytes, at + 12),
            }
        })
        .collect();

    // Directory index 0 is `Section::Atoms` (offset/length pairs), index 1
    // is `Section::AtomBytes` (the concatenated UTF-8 backing bytes). Every
    // other section references an atom by table *index*, never by raw byte
    // offset, so rewriting only these first two sections' content keeps
    // every later section's bytes valid unchanged.
    let atoms = &entries[0];
    let atom_bytes_section = &entries[1];
    let atoms_off = atoms.offset as usize;
    let atom_bytes_off = atom_bytes_section.offset as usize;

    let mut new_atom_bytes: Vec<u8> = Vec::new();
    let mut new_atom_rows: Vec<u8> = Vec::with_capacity(atoms.count as usize * 8);
    for index in 0..atoms.count as usize {
        let row_at = atoms_off + index * 8;
        let off = u32_at(bytes, row_at) as usize;
        let len = u32_at(bytes, row_at + 4) as usize;
        let raw = &bytes[atom_bytes_off + off..atom_bytes_off + off + len];
        let rewritten = match std::str::from_utf8(raw).ok().and_then(|s| s.find(marker)) {
            Some(at) => raw[at..].to_vec(),
            None => raw.to_vec(),
        };
        let new_off = u32::try_from(new_atom_bytes.len()).unwrap_or(u32::MAX);
        let new_len = u32::try_from(rewritten.len()).unwrap_or(u32::MAX);
        new_atom_bytes.extend_from_slice(&rewritten);
        new_atom_rows.extend_from_slice(&new_off.to_le_bytes());
        new_atom_rows.extend_from_slice(&new_len.to_le_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(&new_atom_rows);
    body.extend_from_slice(&new_atom_bytes);
    for entry in &entries[2..] {
        let start = entry.offset as usize;
        let len = entry.byte_count as usize;
        body.extend_from_slice(&bytes[start..start + len]);
    }

    let mut out = vec![0u8; HEADER_BYTES];
    out.copy_from_slice(&bytes[..HEADER_BYTES]);
    let total_len = u32::try_from(HEADER_BYTES + body.len()).unwrap_or(u32::MAX);
    out[8..12].copy_from_slice(&total_len.to_le_bytes());

    let mut cursor = u32::try_from(HEADER_BYTES).unwrap_or(u32::MAX);
    for (index, entry) in entries.iter().enumerate() {
        let at = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        let byte_count = if index == 1 {
            u32::try_from(new_atom_bytes.len()).unwrap_or(u32::MAX)
        } else {
            entry.byte_count
        };
        out[at..at + 2].copy_from_slice(&entry.tag.to_le_bytes());
        out[at + 2..at + 4].copy_from_slice(&entry.row_bytes.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&entry.count.to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&cursor.to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&byte_count.to_le_bytes());
        cursor += byte_count;
    }
    out.extend_from_slice(&body);

    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(&out[..224]);
    hasher.update(&out[256..]);
    let digest = hasher.finalize();
    out[224..256].copy_from_slice(&digest);
    out
}

fn image(bytes: &[u8]) -> Result<CSharpImage<'_>, Box<dyn Error>> {
    CSharpImage::open(bytes).map_err(|error| format!("image rejected: {error:?}").into())
}

fn section_offset(image: &[u8], section: Section) -> usize {
    let at = 48 + (section as usize - 1) * 16;
    u32::from_le_bytes(image[at + 8..at + 12].try_into().unwrap_or([0; 4])) as usize
}

fn rewrite_digest(bytes: &mut [u8]) {
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&bytes[..224]);
    digest.update(&bytes[256..]);
    bytes[224..256].copy_from_slice(&digest.finalize());
}

#[test]
fn committed_fixtures_retain_deep_source_truth() -> Result<(), Box<dyn Error>> {
    let authority = image(IMAGE)?;
    let expected_source: [u8; 32] = Sha256::digest(SOURCE).into();
    assert_eq!(authority.source_digest(), expected_source);
    let declarations: Vec<_> = authority.declarations().collect::<Result<_, _>>()?;
    for kind in [
        DeclarationKind::Namespace,
        DeclarationKind::Interface,
        DeclarationKind::Class,
        DeclarationKind::Struct,
        DeclarationKind::Record,
        DeclarationKind::RecordStruct,
        DeclarationKind::Enum,
        DeclarationKind::Delegate,
        DeclarationKind::Field,
        DeclarationKind::Property,
        DeclarationKind::Indexer,
        DeclarationKind::Event,
        DeclarationKind::Constructor,
        DeclarationKind::Operator,
        DeclarationKind::Conversion,
        DeclarationKind::Method,
    ] {
        assert!(
            declarations
                .iter()
                .any(|declaration| declaration.kind == kind),
            "missing {kind:?}"
        );
    }
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.flags.is_const)
    );
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.flags.is_explicit_interface)
    );
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.partial == PartialRole::Definition)
    );
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.partial == PartialRole::Implementation)
    );
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.parameters.len() >= 3)
    );
    assert!(
        authority
            .references()
            .flatten()
            .any(|reference| reference.kind == ReferenceTag::Invocation)
    );
    assert!(
        authority
            .references()
            .flatten()
            .any(
                |reference| reference.kind == ReferenceTag::InterfaceImplementation
                    && reference.start < reference.end
            )
    );
    assert!(authority.docs().flatten().any(|doc| doc.start < doc.end));
    for declaration in &declarations {
        let start = usize::try_from(declaration.name_start)?;
        let end = usize::try_from(declaration.name_end)?;
        assert_eq!(&SOURCE[start..end], declaration.name.bytes);
    }
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.kind == DeclarationKind::Operator
                && declaration.name.bytes == b"+")
    );
    assert!(declarations.iter().any(|declaration| declaration.kind
        == DeclarationKind::Constructor
        && declaration.name.bytes == b"Widget"));
    assert!(
        declarations
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::Property)
            .count()
            >= 4
    );
    assert!(
        declarations
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::Property
                && declaration.name.bytes == b"Value")
            .count()
            >= 2
    );
    let delegate = declarations
        .iter()
        .find(|declaration| declaration.kind == DeclarationKind::Delegate)
        .ok_or("delegate row")?;
    assert_eq!(delegate.parameters.len(), 3);
    let unicode = image(UNICODE_IMAGE)?;
    let expected_unicode: [u8; 32] = Sha256::digest(UNICODE_SOURCE).into();
    assert_eq!(unicode.source_digest(), expected_unicode);
    for declaration in unicode.declarations().collect::<Result<Vec<_>, _>>()? {
        let start = usize::try_from(declaration.name_start)?;
        let end = usize::try_from(declaration.name_end)?;
        assert_eq!(&UNICODE_SOURCE[start..end], declaration.name.bytes);
    }
    Ok(())
}

#[test]
fn dotnet_regeneration_is_byte_exact_and_deterministic() -> Result<(), Box<dyn Error>> {
    // The real Roslyn authority requires dotnet. `probe_dotnet` returns a
    // typed `ToolingUnavailable` terminal when the configured executable is
    // absent or unusable, so this test can never silently replay fixtures.
    let dotnet = probe_dotnet()?;
    // The oracle is built from source, exactly as the flow harness does: a
    // locked restore pins the Roslyn closure from packages.lock.json, and the
    // no-restore publish reuses exactly those assets. No committed build
    // output is ever trusted.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/producer");
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/legacy/helper");
    let publish = std::env::temp_dir().join(format!(
        "nudox-csharp-oracle-publish-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    fs::create_dir_all(&publish)?;
    // Every concurrently running C# test builds the same checked-in helper
    // project. Restore and publish both write intermediate build state
    // (`obj/`, `BaseIntermediateOutputPath`) under the project directory by
    // default, and publish also builds into the default `bin/`
    // (`BaseOutputPath`) before copying to `-o` — `-o` alone does not
    // redirect that intermediate build step. Without explicit, per-process
    // overrides for both, every concurrent process races on the same
    // files, which is what actually produced the "byte-exact" failure under
    // full-suite load: a manual, isolated reproduction of these exact steps
    // is byte-identical to the committed fixture (see the fix commit for
    // this test file).
    let intermediate = publish.join("obj");
    let mut intermediate_arg = std::ffi::OsString::from("-p:BaseIntermediateOutputPath=");
    intermediate_arg.push(&intermediate);
    intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let output_base = publish.join("bin");
    let mut output_base_arg = std::ffi::OsString::from("-p:BaseOutputPath=");
    output_base_arg.push(&output_base);
    output_base_arg.push(std::path::MAIN_SEPARATOR.to_string());
    let restored = Command::new(&dotnet)
        .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
        .arg(&intermediate_arg)
        .current_dir(&helper_dir)
        .status()?;
    assert!(restored.success(), "locked oracle restore failed: {restored}");
    // `UseSharedCompilation=false`: see this file's doc comment above — the
    // isolated `obj/`/`bin/` directories rule out a file-system race, but
    // MSBuild's ambient VBCSCompiler node is shared across every concurrent
    // `dotnet build`/`publish` on this machine regardless of directory
    // isolation, so force this build to spawn its own isolated `csc`
    // process instead.
    let published = Command::new(&dotnet)
        .args([
            "publish",
            "oracle.csproj",
            "-c",
            "Release",
            "--nologo",
            "--no-restore",
            "-p:UseSharedCompilation=false",
        ])
        .arg(&intermediate_arg)
        .arg(&output_base_arg)
        .arg("-o")
        .arg(&publish)
        .current_dir(&helper_dir)
        .status()?;
    assert!(published.success(), "oracle publish failed: {published}");
    let helper = publish.join("oracle.dll");
    let first = std::env::temp_dir().join("nudox-csharp-fidelity-first.ncaimg");
    let second = std::env::temp_dir().join("nudox-csharp-fidelity-second.ncaimg");
    for output in [&first, &second] {
        let status = Command::new(&dotnet)
            .args([
                helper.to_str().ok_or("helper path")?,
                "--mode",
                "source",
                "--root",
                root.to_str().ok_or("root path")?,
                "--authority-image",
                "--source-binding",
                root.join("fidelity.cs").to_str().ok_or("source path")?,
                "--out",
                output.to_str().ok_or("output path")?,
            ])
            .status()?;
        assert!(status.success(), "oracle regeneration failed: {status}");
    }
    let first_bytes = fs::read(&first)?;
    let second_bytes = fs::read(&second)?;
    // `IMAGE` was captured from a different worktree's absolute checkout
    // path than this one (see `normalize_authority_image_paths`'s doc
    // comment); compare the checkout-independent form so this assertion
    // verifies real Roslyn-extraction parity instead of an absolute-path
    // coincidence. `first_bytes == second_bytes` below still compares raw,
    // unnormalized bytes: both regenerations ran in this same process, so a
    // raw mismatch there would be genuine non-determinism.
    const MARKER: &str = "frontends/csharp/tests/fixtures/producer/fidelity.cs";
    assert_eq!(
        normalize_authority_image_paths(&first_bytes, MARKER),
        normalize_authority_image_paths(IMAGE, MARKER)
    );
    assert_eq!(first_bytes, second_bytes);
    let unicode_output = std::env::temp_dir().join("nudox-csharp-fidelity-unicode.ncaimg");
    let status = Command::new(&dotnet)
        .args([
            helper.to_str().ok_or("helper path")?,
            "--mode",
            "source",
            "--root",
            root.to_str().ok_or("root path")?,
            "--authority-image",
            "--source-binding",
            root.join("unicode.cs").to_str().ok_or("source path")?,
            "--out",
            unicode_output.to_str().ok_or("output path")?,
        ])
        .status()?;
    assert!(
        status.success(),
        "unicode oracle regeneration failed: {status}"
    );
    const UNICODE_MARKER: &str = "frontends/csharp/tests/fixtures/producer/unicode.cs";
    assert_eq!(
        normalize_authority_image_paths(&fs::read(unicode_output)?, UNICODE_MARKER),
        normalize_authority_image_paths(UNICODE_IMAGE, UNICODE_MARKER)
    );
    Ok(())
}

/// The absent-toolchain terminal is itself observable: an explicit bogus
/// dotnet path must return a typed `ToolingUnavailable` rather than let the
/// producer test replay committed fixtures.
#[test]
fn absent_dotnet_is_a_typed_tooling_unavailable_terminal() {
    let error = probe_dotnet_path(PathBuf::from("/definitely/not-a-dotnet"))
        .expect_err("a bogus dotnet path must not resolve");
    assert_eq!(error.language, "csharp");
    assert_eq!(error.tool, PathBuf::from("/definitely/not-a-dotnet"));
}

#[test]
fn structural_mutations_report_their_exact_fault_class() -> Result<(), Box<dyn Error>> {
    let cases = [
        (Section::Declarations, 0, 0, "kind"),
        (Section::Declarations, 1, 0xff, "flags"),
        (Section::Declarations, 2, 0xff, "partial"),
        (Section::Declarations, 3, 0xff, "refkind"),
        (Section::TypeParameters, 10, 0xff, "variance"),
        (Section::Types, 0, 0xff, "typekind"),
        (Section::Types, 1, 0xff, "nullability"),
        (Section::Types, 2, 1, "reserved"),
    ];
    for (section, cell, value, name) in cases {
        let mut bytes = IMAGE.to_vec();
        let offset = section_offset(IMAGE, section) + cell;
        bytes[offset] = value;
        rewrite_digest(&mut bytes);
        let error = match CSharpImage::open(&bytes) {
            Ok(_) => return Err(format!("{name}: mutation was accepted").into()),
            Err(error) => error,
        };
        assert!(
            matches!(
                error,
                ImageError::DeclarationKind { .. } | ImageError::DeclarationReserved { .. }
            ),
            "{name}: {error:?}"
        );
    }
    let mut bytes = IMAGE.to_vec();
    let atom_offset = section_offset(IMAGE, Section::Declarations);
    bytes[atom_offset + 4..atom_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::NameRange { .. })
    ));
    let mut bytes = IMAGE.to_vec();
    bytes[atom_offset + 24..atom_offset + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::Span { .. })
    ));
    let mut bytes = IMAGE.to_vec();
    let atom_offset = section_offset(IMAGE, Section::Atoms);
    bytes[atom_offset + 4..atom_offset + 8].copy_from_slice(&0u32.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::NameRange { .. })
    ));
    let mut bytes = IMAGE.to_vec();
    let parameter_offset = section_offset(IMAGE, Section::Parameters);
    bytes[parameter_offset..parameter_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::Span { .. })
    ));
    let mut bytes = IMAGE.to_vec();
    let reference_offset = section_offset(IMAGE, Section::References);
    bytes[reference_offset..reference_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::Span { .. })
    ));
    let mut bytes = IMAGE.to_vec();
    let docs_offset = section_offset(IMAGE, Section::Docs);
    let start = u32::from_le_bytes(bytes[docs_offset + 8..docs_offset + 12].try_into()?);
    bytes[docs_offset + 12..docs_offset + 16]
        .copy_from_slice(&start.saturating_sub(1).to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(
        CSharpImage::open(&bytes),
        Err(ImageError::Span { .. })
    ));
    Ok(())
}

#[test]
fn image_digest_is_reproducible_and_sections_are_canonical() -> Result<(), Box<dyn Error>> {
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&IMAGE[..224]);
    digest.update(&IMAGE[256..]);
    assert_eq!(&digest.finalize()[..], &IMAGE[224..256]);
    image(IMAGE)?;
    assert_eq!(IMAGE, include_bytes!("fixtures/producer/fidelity.ncaimg"));
    Ok(())
}
