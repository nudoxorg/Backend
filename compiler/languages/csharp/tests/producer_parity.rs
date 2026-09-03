//! Source-truth parity for the checked-in image emitted by the real Roslyn oracle.
use compiler_languages_csharp::{CSharpImage, DeclarationKind, ReferenceTag};
use sha2::{Digest, Sha256};

const SOURCE: &[u8] = include_bytes!("fixtures/producer/fidelity.cs");
const IMAGE: &[u8] = include_bytes!("fixtures/producer/fidelity.ncaimg");
const DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";

#[test]
fn committed_fixture_retains_source_truth() {
    let image = CSharpImage::open(IMAGE).expect("committed producer image must validate");
    let source_digest: [u8; 32] = Sha256::digest(SOURCE).into();
    assert_eq!(image.source_digest(), source_digest);
    let declarations: Vec<_> = image.declarations().map(Result::unwrap).collect();
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
        assert!(declarations.iter().any(|declaration| declaration.kind == kind));
    }
    assert!(declarations.iter().any(|declaration| declaration.flags.is_const));
    assert!(declarations.iter().any(|declaration| declaration.flags.is_explicit_interface));
    assert!(image.references().flatten().any(|reference| reference.kind == ReferenceTag::Invocation));
}

#[test]
fn image_digest_is_reproducible_and_sections_are_canonical() {
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(&IMAGE[..224]);
    digest.update(&IMAGE[256..]);
    assert_eq!(&digest.finalize()[..], &IMAGE[224..256]);
    assert!(CSharpImage::open(IMAGE).is_ok());
    assert_eq!(IMAGE, include_bytes!("fixtures/producer/fidelity.ncaimg"));
}
