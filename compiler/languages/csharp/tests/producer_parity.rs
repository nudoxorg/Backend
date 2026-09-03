//! Source-truth parity and structural falsifiers for the Roslyn authority image.
use compiler_languages_csharp::{CSharpImage, DeclarationKind, ImageError, PartialRole, ReferenceTag, Section};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SOURCE: &[u8] = include_bytes!("fixtures/producer/fidelity.cs");
const IMAGE: &[u8] = include_bytes!("fixtures/producer/fidelity.ncaimg");
const UNICODE_SOURCE: &[u8] = include_bytes!("fixtures/producer/unicode.cs");
const UNICODE_IMAGE: &[u8] = include_bytes!("fixtures/producer/unicode.ncaimg");
const DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";

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
    for kind in [DeclarationKind::Namespace, DeclarationKind::Interface, DeclarationKind::Class,
        DeclarationKind::Struct, DeclarationKind::Record, DeclarationKind::RecordStruct,
        DeclarationKind::Enum, DeclarationKind::Delegate, DeclarationKind::Field,
        DeclarationKind::Property, DeclarationKind::Indexer, DeclarationKind::Event,
        DeclarationKind::Constructor, DeclarationKind::Operator, DeclarationKind::Conversion,
        DeclarationKind::Method] {
        assert!(declarations.iter().any(|declaration| declaration.kind == kind), "missing {kind:?}");
    }
    assert!(declarations.iter().any(|declaration| declaration.flags.is_const));
    assert!(declarations.iter().any(|declaration| declaration.flags.is_explicit_interface));
    assert!(declarations.iter().any(|declaration| declaration.partial == PartialRole::Definition));
    assert!(declarations.iter().any(|declaration| declaration.partial == PartialRole::Implementation));
    assert!(declarations.iter().any(|declaration| declaration.parameters.len() >= 3));
    assert!(authority.references().flatten().any(|reference| reference.kind == ReferenceTag::Invocation));
    assert!(authority.references().flatten().any(|reference| reference.kind == ReferenceTag::InterfaceImplementation && reference.start < reference.end));
    assert!(authority.docs().flatten().any(|doc| doc.start < doc.end));
    for declaration in &declarations {
        let start = usize::try_from(declaration.name_start)?;
        let end = usize::try_from(declaration.name_end)?;
        assert_eq!(&SOURCE[start..end], declaration.name.bytes);
    }
    assert!(declarations.iter().any(|declaration| declaration.kind == DeclarationKind::Operator && declaration.name.bytes == b"+"));
    assert!(declarations.iter().any(|declaration| declaration.kind == DeclarationKind::Constructor && declaration.name.bytes == b"Widget"));
    assert!(declarations.iter().filter(|declaration| declaration.kind == DeclarationKind::Property).count() >= 4);
    assert!(declarations.iter().filter(|declaration| declaration.kind == DeclarationKind::Property && declaration.name.bytes == b"Value").count() >= 2);
    let delegate = declarations.iter().find(|declaration| declaration.kind == DeclarationKind::Delegate).ok_or("delegate row")?;
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
    let dotnet = Command::new("dotnet").arg("--version").output();
    let Ok(version) = dotnet else { return Ok(()) };
    if !version.status.success() { return Ok(()) }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/producer");
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("helper/bin/Release/net10.0/oracle.dll");
    let first = std::env::temp_dir().join("nudox-csharp-fidelity-first.ncaimg");
    let second = std::env::temp_dir().join("nudox-csharp-fidelity-second.ncaimg");
    for output in [&first, &second] {
        let status = Command::new("dotnet").args([helper.to_str().ok_or("helper path")?, "--mode", "source", "--root", root.to_str().ok_or("root path")?, "--authority-image", "--source-binding", root.join("fidelity.cs").to_str().ok_or("source path")?, "--out", output.to_str().ok_or("output path")?]).status()?;
        assert!(status.success(), "oracle regeneration failed: {status}");
    }
    let first_bytes = fs::read(&first)?;
    let second_bytes = fs::read(&second)?;
    assert_eq!(first_bytes, IMAGE);
    assert_eq!(first_bytes, second_bytes);
    let unicode_output = std::env::temp_dir().join("nudox-csharp-fidelity-unicode.ncaimg");
    let status = Command::new("dotnet").args([helper.to_str().ok_or("helper path")?, "--mode", "source", "--root", root.to_str().ok_or("root path")?, "--authority-image", "--source-binding", root.join("unicode.cs").to_str().ok_or("source path")?, "--out", unicode_output.to_str().ok_or("output path")?]).status()?;
    assert!(status.success(), "unicode oracle regeneration failed: {status}");
    assert_eq!(fs::read(unicode_output)?, UNICODE_IMAGE);
    Ok(())
}

#[test]
fn structural_mutations_report_their_exact_fault_class() -> Result<(), Box<dyn Error>> {
    let cases = [
        (Section::Declarations, 0, 0, "kind"), (Section::Declarations, 1, 0xff, "flags"),
        (Section::Declarations, 2, 0xff, "partial"), (Section::Declarations, 3, 0xff, "refkind"),
        (Section::TypeParameters, 10, 0xff, "variance"), (Section::Types, 0, 0xff, "typekind"),
        (Section::Types, 1, 0xff, "nullability"), (Section::Types, 2, 1, "reserved"),
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
        assert!(matches!(error, ImageError::DeclarationKind { .. } | ImageError::DeclarationReserved { .. }), "{name}: {error:?}");
    }
    let mut bytes = IMAGE.to_vec();
    let atom_offset = section_offset(IMAGE, Section::Declarations);
    bytes[atom_offset + 4..atom_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::NameRange { .. })));
    let mut bytes = IMAGE.to_vec();
    bytes[atom_offset + 24..atom_offset + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::Span { .. })));
    let mut bytes = IMAGE.to_vec();
    let atom_offset = section_offset(IMAGE, Section::Atoms);
    bytes[atom_offset + 4..atom_offset + 8].copy_from_slice(&0u32.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::NameRange { .. })));
    let mut bytes = IMAGE.to_vec();
    let parameter_offset = section_offset(IMAGE, Section::Parameters);
    bytes[parameter_offset..parameter_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::Span { .. })));
    let mut bytes = IMAGE.to_vec();
    let reference_offset = section_offset(IMAGE, Section::References);
    bytes[reference_offset..reference_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::Span { .. })));
    let mut bytes = IMAGE.to_vec();
    let docs_offset = section_offset(IMAGE, Section::Docs);
    let start = u32::from_le_bytes(bytes[docs_offset + 8..docs_offset + 12].try_into()?);
    bytes[docs_offset + 12..docs_offset + 16].copy_from_slice(&start.saturating_sub(1).to_le_bytes());
    rewrite_digest(&mut bytes);
    assert!(matches!(CSharpImage::open(&bytes), Err(ImageError::Span { .. })));
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
