//! Exercises the borrowed Roslyn authority image with independent byte-level
//! attacks. The fixture is constructed as an image, never by serializing
//! production DTOs. Each mutation recomputes the domain-separated SHA-256 so
//! structural checks cannot be bypassed by a checksum error.

use compiler_languages_csharp::{
    CSharpImage, DeclarationKind, HeaderError, ImageError, NullabilityCell, PartialRole, RefKind,
    ReferenceTag, Section, TypeNodeKind,
};
use sha2::{Digest, Sha256};

const HEADER_BYTES: usize = 256;
const DIRECTORY_OFFSET: usize = 48;
const DIRECTORY_ENTRY_BYTES: usize = 16;
const IMAGE_DIGEST_OFFSET: usize = 224;
const ABSENT: u32 = u32::MAX;
const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";

#[derive(Debug, thiserror::Error)]
enum ImageTestError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("expected one {fact} record, found none")]
    Missing { fact: &'static str },
    #[error("expected `{expected}`, found `{actual}`")]
    Text {
        expected: &'static str,
        actual: String,
    },
    #[error("expected a semantic invariant that the image did not retain: {fact}")]
    MissingFact { fact: &'static str },
    #[error("the test image exceeded its fixed wire scalar: {fact}")]
    FixtureOverflow { fact: &'static str },
}

#[derive(Clone)]
struct TypeRow {
    kind: u8,
    nullable: u8,
    spelling: Option<usize>,
    children: Vec<(Option<usize>, u32)>,
}

#[derive(Clone)]
struct ParamRow {
    ty: u32,
    name: usize,
    ref_kind: u8,
    flags: u8,
    default: Option<usize>,
    name_start: u32,
    name_end: u32,
}

#[derive(Clone)]
struct GenericRow {
    name: usize,
    constraints: Vec<u32>,
    variance: u8,
    flags: u8,
}

#[derive(Clone)]
struct DeclarationRow {
    kind: u8,
    flags: u8,
    partial: u8,
    ref_kind: u8,
    name: usize,
    qualified: Option<usize>,
    owner: Option<u32>,
    declared_type: Option<u32>,
    decl_start: u32,
    name_start: u32,
    name_end: u32,
    params: Vec<ParamRow>,
    generics: Vec<GenericRow>,
    doc: Option<usize>,
}

#[derive(Clone)]
struct AttributeRow {
    declaration: u32,
    spelling: usize,
}

#[derive(Clone)]
struct DocRow {
    declaration: u32,
    file: usize,
    start: u32,
    end: u32,
    xml: usize,
}

#[derive(Clone)]
struct ReferenceRow {
    owner: u32,
    target: Option<u32>,
    spelling: usize,
    file: usize,
    start: u32,
    end: u32,
    kind: u8,
}

/// One Roslyn authority image under construction, encoded exactly like the
/// vendored producer: canonical sections, fixed directory, domain-separated
/// checksum, and the bound source digest.
#[derive(Clone, Default)]
struct Fixture {
    atoms: Vec<Vec<u8>>,
    declarations: Vec<DeclarationRow>,
    types: Vec<TypeRow>,
    attributes: Vec<AttributeRow>,
    docs: Vec<DocRow>,
    references: Vec<ReferenceRow>,
}

impl Fixture {
    fn atom(&mut self, text: &[u8]) -> usize {
        self.atoms.push(text.to_vec());
        self.atoms.len() - 1
    }

    /// One empty class declaration whose spans name the given source range.
    fn class(&mut self, qualified: &[u8], name_start: u32, name_end: u32) -> u32 {
        let (_namespace, simple) = match qualified.iter().rposition(|byte| *byte == b'.') {
            Some(at) => (&qualified[..at], &qualified[at + 1..]),
            None => (&[][..], qualified),
        };
        let qualified_atom = self.atom(qualified);
        let name_atom = self.atom(simple);
        let spelling = self.atom(qualified);
        let ty = u32::try_from(self.types.len()).unwrap_or(u32::MAX);
        self.types.push(TypeRow {
            kind: TypeNodeKind::Named as u8,
            nullable: NullabilityCell::NotAnnotated as u8,
            spelling: Some(spelling),
            children: Vec::new(),
        });
        let decl_start = name_start;
        self.declarations.push(DeclarationRow {
            kind: DeclarationKind::Class as u8,
            flags: 0,
            partial: PartialRole::None as u8,
            ref_kind: RefKind::Value as u8,
            name: name_atom,
            qualified: Some(qualified_atom),
            owner: None,
            declared_type: Some(ty),
            decl_start,
            name_start,
            name_end,
            params: Vec::new(),
            generics: Vec::new(),
            doc: None,
        });
        u32::try_from(self.declarations.len() - 1).unwrap_or(u32::MAX)
    }

    fn encode(&self, source: &[u8]) -> Result<Vec<u8>, ImageTestError> {
        let cell = |value: usize| {
            u32::try_from(value).map_err(|_| ImageTestError::FixtureOverflow {
                fact: "plane scalar",
            })
        };
        let mut atoms = Vec::new();
        let mut atom_bytes = Vec::new();
        for text in &self.atoms {
            atoms.extend_from_slice(&cell(atom_bytes.len())?.to_le_bytes());
            atoms.extend_from_slice(&cell(text.len())?.to_le_bytes());
            atom_bytes.extend_from_slice(text);
        }
        let mut params = Vec::new();
        let mut generic_rows = Vec::new();
        let mut constraints = Vec::new();
        let mut declarations = Vec::new();
        for row in &self.declarations {
            let param_start = cell(params.len() / 24)?;
            for param in &row.params {
                params.extend_from_slice(&param.ty.to_le_bytes());
                params.extend_from_slice(&cell(param.name)?.to_le_bytes());
                params.push(param.ref_kind);
                params.push(param.flags);
                params.extend_from_slice(&0_u16.to_le_bytes());
                params.extend_from_slice(
                    &param
                        .default
                        .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                        .to_le_bytes(),
                );
                params.extend_from_slice(&param.name_start.to_le_bytes());
                params.extend_from_slice(&param.name_end.to_le_bytes());
            }
            let generic_start = cell(generic_rows.len() / 12)?;
            for generic in &row.generics {
                let constraint_start = cell(constraints.len() / 4)?;
                for constraint in &generic.constraints {
                    constraints.extend_from_slice(&constraint.to_le_bytes());
                }
                generic_rows.extend_from_slice(&cell(generic.name)?.to_le_bytes());
                generic_rows.extend_from_slice(&constraint_start.to_le_bytes());
                generic_rows.extend_from_slice(
                    &u16::try_from(generic.constraints.len())
                        .map_err(|_| ImageTestError::FixtureOverflow {
                            fact: "constraint count",
                        })?
                        .to_le_bytes(),
                );
                generic_rows.push(generic.variance);
                generic_rows.push(generic.flags);
            }
            let param_count =
                u16::try_from(row.params.len()).map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "param count",
                })?;
            let generic_count =
                u16::try_from(row.generics.len()).map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "generic count",
                })?;
            declarations.push(row.kind);
            declarations.push(row.flags);
            declarations.push(row.partial);
            declarations.push(row.ref_kind);
            declarations.extend_from_slice(&cell(row.name)?.to_le_bytes());
            declarations.extend_from_slice(
                &row.qualified
                    .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                    .to_le_bytes(),
            );
            declarations.extend_from_slice(&row.owner.unwrap_or(ABSENT).to_le_bytes());
            declarations.extend_from_slice(&row.declared_type.unwrap_or(ABSENT).to_le_bytes());
            declarations.extend_from_slice(&row.decl_start.to_le_bytes());
            declarations.extend_from_slice(&row.name_start.to_le_bytes());
            declarations.extend_from_slice(&row.name_end.to_le_bytes());
            declarations.extend_from_slice(&param_start.to_le_bytes());
            declarations.extend_from_slice(&param_count.to_le_bytes());
            declarations.extend_from_slice(&generic_start.to_le_bytes());
            declarations.extend_from_slice(&generic_count.to_le_bytes());
            declarations.extend_from_slice(
                &row.doc
                    .map_or(ABSENT, |d| cell(d).unwrap_or(ABSENT))
                    .to_le_bytes(),
            );
        }
        let mut types = Vec::new();
        let mut type_children = Vec::new();
        for row in &self.types {
            let child_start = cell(type_children.len() / 4)?;
            for (name, ty) in &row.children {
                type_children.extend_from_slice(
                    &name
                        .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                        .to_le_bytes(),
                );
                type_children.extend_from_slice(&ty.to_le_bytes());
            }
            types.push(row.kind);
            types.push(row.nullable);
            types.extend_from_slice(&0_u16.to_le_bytes());
            types.extend_from_slice(
                &row.spelling
                    .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                    .to_le_bytes(),
            );
            types.extend_from_slice(&child_start.to_le_bytes());
            types.extend_from_slice(&cell(row.children.len())?.to_le_bytes());
        }
        let mut attribute_rows = Vec::new();
        for row in &self.attributes {
            attribute_rows.extend_from_slice(&row.declaration.to_le_bytes());
            attribute_rows.extend_from_slice(&cell(row.spelling)?.to_le_bytes());
        }
        let mut doc_rows = Vec::new();
        for row in &self.docs {
            doc_rows.extend_from_slice(&row.declaration.to_le_bytes());
            doc_rows.extend_from_slice(&cell(row.file)?.to_le_bytes());
            doc_rows.extend_from_slice(&row.start.to_le_bytes());
            doc_rows.extend_from_slice(&row.end.to_le_bytes());
            doc_rows.extend_from_slice(&cell(row.xml)?.to_le_bytes());
        }
        let mut reference_rows = Vec::new();
        for row in &self.references {
            reference_rows.extend_from_slice(&row.owner.to_le_bytes());
            reference_rows.extend_from_slice(&row.target.unwrap_or(ABSENT).to_le_bytes());
            reference_rows.extend_from_slice(&cell(row.spelling)?.to_le_bytes());
            reference_rows.extend_from_slice(&cell(row.file)?.to_le_bytes());
            reference_rows.extend_from_slice(&row.start.to_le_bytes());
            reference_rows.extend_from_slice(&row.end.to_le_bytes());
            reference_rows.push(row.kind);
            reference_rows.extend_from_slice(&[0; 3]);
        }
        let sections = [
            atoms,
            atom_bytes,
            declarations,
            params,
            generic_rows,
            constraints,
            types,
            type_children,
            attribute_rows,
            doc_rows,
            reference_rows,
        ];
        let row_bytes = [8_u16, 1, 48, 24, 12, 4, 16, 8, 8, 20, 28];
        let mut image = vec![0_u8; HEADER_BYTES];
        image[..4].copy_from_slice(b"NCAI");
        image[4..6].copy_from_slice(&3_u16.to_le_bytes());
        image[6..8].copy_from_slice(
            &u16::try_from(HEADER_BYTES)
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "header length",
                })?
                .to_le_bytes(),
        );
        let body = sections.iter().map(Vec::len).sum::<usize>();
        image[8..12].copy_from_slice(
            &u32::try_from(body)
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "body length",
                })?
                .to_le_bytes(),
        );
        image[12..44].copy_from_slice(Sha256::digest(source).as_slice());
        let _ = IMAGE_DIGEST_OFFSET;
        image[44..46].copy_from_slice(
            &u16::try_from(sections.len())
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "section count",
                })?
                .to_le_bytes(),
        );
        let mut offset = HEADER_BYTES;
        for (index, section) in sections.iter().enumerate() {
            let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
            image[entry..entry + 2].copy_from_slice(
                &u16::try_from(index + 1)
                    .map_err(|_| ImageTestError::FixtureOverflow {
                        fact: "section tag",
                    })?
                    .to_le_bytes(),
            );
            image[entry + 2..entry + 4].copy_from_slice(&row_bytes[index].to_le_bytes());
            image[entry + 4..entry + 8].copy_from_slice(
                &u32::try_from(section.len() / usize::from(row_bytes[index]))
                    .map_err(|_| ImageTestError::FixtureOverflow {
                        fact: "section count",
                    })?
                    .to_le_bytes(),
            );
            image[entry + 8..entry + 12].copy_from_slice(
                &u32::try_from(offset)
                    .map_err(|_| ImageTestError::FixtureOverflow {
                        fact: "section offset",
                    })?
                    .to_le_bytes(),
            );
            image[entry + 12..entry + 16].copy_from_slice(
                &u32::try_from(section.len())
                    .map_err(|_| ImageTestError::FixtureOverflow {
                        fact: "section bytes",
                    })?
                    .to_le_bytes(),
            );
            offset += section.len();
        }
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&image[..IMAGE_DIGEST_OFFSET]);
        for section in &sections {
            digest.update(section);
        }
        image.resize(HEADER_BYTES, 0);
        image[IMAGE_DIGEST_OFFSET..HEADER_BYTES].copy_from_slice(&digest.finalize());
        let mut complete = image;
        for section in &sections {
            complete.extend_from_slice(section);
        }
        Ok(complete)
    }
}

fn checksum(image: &mut [u8]) {
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(&image[..IMAGE_DIGEST_OFFSET]);
    digest.update(&image[HEADER_BYTES..]);
    image[IMAGE_DIGEST_OFFSET..HEADER_BYTES].copy_from_slice(&digest.finalize());
}

#[test]
fn image_borrows_declaration_types_and_doc_provenance_without_dto_reconstruction()
-> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let file_text = b"Widget.cs";
    let xml_text = b"<member name=\"T:Demo.Widget\"><summary>Serves.</summary></member>";
    let attribute_text = b"Obsolete(\"use New\")";
    let file = fix.atom(file_text);
    let xml = fix.atom(xml_text);
    let attribute = fix.atom(attribute_text);
    let widget = fix.class(b"Demo.Widget", 6, 12);
    fix.docs.push(DocRow {
        declaration: widget,
        file,
        start: 0,
        end: 5,
        xml,
    });
    fix.attributes.push(AttributeRow {
        declaration: widget,
        spelling: attribute,
    });
    let source = b"class Widget {}";
    let bytes = fix.encode(source)?;
    let image = CSharpImage::open(&bytes)?;
    if image.source_digest() != Sha256::digest(source).as_slice() {
        return Err(ImageTestError::MissingFact {
            fact: "source digest",
        });
    }
    let declaration = image
        .declarations()
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "declaration",
        })?;
    if declaration.kind != DeclarationKind::Class
        || declaration.partial != PartialRole::None
        || declaration.ref_kind != RefKind::Value
    {
        return Err(ImageTestError::MissingFact {
            fact: "closed cells",
        });
    }
    if declaration.name.bytes != b"Widget" {
        return Err(ImageTestError::Text {
            expected: "Widget",
            actual: String::from_utf8_lossy(declaration.name.bytes).into_owned(),
        });
    }
    let qualified = declaration.qualified.ok_or(ImageTestError::Missing {
        fact: "qualified name",
    })?;
    if qualified.bytes != b"Demo.Widget" {
        return Err(ImageTestError::MissingFact {
            fact: "qualified name",
        });
    }
    let declared = declaration.declared_type.ok_or(ImageTestError::Missing {
        fact: "declared type",
    })?;
    let node = image.type_node(declared)?;
    if node.kind != TypeNodeKind::Named
        || node.nullable != NullabilityCell::NotAnnotated
        || node.spelling.map(|atom| atom.bytes) != Some(b"Demo.Widget".as_slice())
    {
        return Err(ImageTestError::MissingFact {
            fact: "named type node",
        });
    }
    let doc = image
        .docs()
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing { fact: "doc row" })?;
    if doc.declaration != widget || doc.file.bytes != file_text || doc.xml.bytes != xml_text {
        return Err(ImageTestError::MissingFact {
            fact: "doc provenance",
        });
    }
    let attribute = image
        .attributes()
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "attribute row",
        })?;
    if attribute.declaration != widget || attribute.spelling.bytes != attribute_text {
        return Err(ImageTestError::MissingFact {
            fact: "attribute spelling",
        });
    }
    Ok(())
}

#[test]
fn image_rejects_checksum_and_post_checksum_structural_mutations() -> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let _ = fix.class(b"Demo.Widget", 6, 12);
    let source = b"class Widget {}";
    let valid = fix.encode(source)?;

    let mut checksum_mutant = valid.clone();
    checksum_mutant[IMAGE_DIGEST_OFFSET] ^= 1;
    if !matches!(CSharpImage::open(&checksum_mutant), Err(ImageError::Digest)) {
        return Err(ImageTestError::MissingFact {
            fact: "checksum mutation rejection",
        });
    }

    // A declaration kind outside the closed vocabulary, with the checksum
    // recomputed, is the typed closed-tag rejection retaining the row.
    let declarations_offset = plane_offset(&valid, Section::Declarations)?;
    let mut tag_mutant = valid.clone();
    tag_mutant[declarations_offset] = 200;
    checksum(&mut tag_mutant);
    if !matches!(
        CSharpImage::open(&tag_mutant),
        Err(ImageError::DeclarationKind {
            index: 0,
            found: 200,
            plane: Section::Declarations
        })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "closed tag rejection",
        });
    }

    // A type coordinate outside the type section is the typed span rejection.
    let mut coordinate_mutant = valid.clone();
    coordinate_mutant[declarations_offset + 16..declarations_offset + 20]
        .copy_from_slice(&9_u32.to_le_bytes());
    checksum(&mut coordinate_mutant);
    match CSharpImage::open(&coordinate_mutant) {
        Err(ImageError::Span {
            start: 9, end: 1, ..
        }) => {}
        other => {
            eprintln!("coordinate mutant produced {other:?}");
            return Err(ImageTestError::MissingFact {
                fact: "coordinate rejection",
            });
        }
    }

    // A truncated envelope is the typed header rejection.
    if !matches!(
        CSharpImage::open(&valid[..80]),
        Err(ImageError::Header(HeaderError::Truncated { .. }))
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "truncation rejection",
        });
    }

    // A non-UTF-8 atom byte is the typed UTF-8 rejection.
    let atoms_offset = plane_offset(&valid, Section::AtomBytes)?;
    let mut utf8_mutant = valid.clone();
    utf8_mutant[atoms_offset] = 0xff;
    checksum(&mut utf8_mutant);
    if !matches!(
        CSharpImage::open(&utf8_mutant),
        Err(ImageError::NameUtf8 { index: 0 })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "utf8 rejection",
        });
    }
    Ok(())
}

#[test]
fn image_rejects_every_signature_and_type_vocabulary_mutation() -> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let class = fix.class(b"Demo.Widget", 6, 12);
    let method_name = fix.atom(b"M");
    let parameter_name = fix.atom(b"value");
    let type_name = fix.atom(b"System.Int32");
    let type_row = u32::try_from(fix.types.len()).unwrap_or(u32::MAX);
    fix.types.push(TypeRow {
        kind: TypeNodeKind::Named as u8,
        nullable: NullabilityCell::None as u8,
        spelling: Some(type_name),
        children: Vec::new(),
    });
    let generic_name = fix.atom(b"T");
    fix.declarations[0].generics.push(GenericRow {
        name: generic_name,
        constraints: Vec::new(),
        variance: 0,
        flags: 0,
    });
    fix.declarations.push(DeclarationRow {
        kind: DeclarationKind::Method as u8,
        flags: 0,
        partial: PartialRole::None as u8,
        ref_kind: RefKind::Value as u8,
        name: method_name,
        qualified: None,
        owner: Some(class),
        declared_type: Some(type_row),
        decl_start: 15,
        name_start: 15,
        name_end: 16,
        params: vec![ParamRow {
            ty: type_row,
            name: parameter_name,
            ref_kind: RefKind::Value as u8,
            flags: 0,
            default: None,
            name_start: 17,
            name_end: 22,
        }],
        generics: Vec::new(),
        doc: None,
    });
    let source = b"class Widget { void M(int value) {} }";
    let valid = fix.encode(source)?;

    let parameters = plane_offset(&valid, Section::Parameters)?;
    let mut parameter_mutant = valid.clone();
    parameter_mutant[parameters + 8] = 200;
    checksum(&mut parameter_mutant);
    assert_eq!(
        CSharpImage::open(&parameter_mutant),
        Err(ImageError::DeclarationKind {
            index: 0,
            found: 200,
            plane: Section::Parameters,
        })
    );

    let type_parameters = plane_offset(&valid, Section::TypeParameters)?;
    let mut generic_mutant = valid.clone();
    generic_mutant[type_parameters + 10] = 200;
    checksum(&mut generic_mutant);
    assert_eq!(
        CSharpImage::open(&generic_mutant),
        Err(ImageError::DeclarationKind {
            index: 0,
            found: 200,
            plane: Section::TypeParameters,
        })
    );

    let types = plane_offset(&valid, Section::Types)?;
    let mut kind_mutant = valid.clone();
    kind_mutant[types] = 200;
    checksum(&mut kind_mutant);
    assert_eq!(
        CSharpImage::open(&kind_mutant),
        Err(ImageError::DeclarationKind {
            index: 0,
            found: 200,
            plane: Section::Types,
        })
    );

    let mut nullability_mutant = valid;
    nullability_mutant[types + 1] = 200;
    checksum(&mut nullability_mutant);
    assert_eq!(
        CSharpImage::open(&nullability_mutant),
        Err(ImageError::DeclarationKind {
            index: 0,
            found: 200,
            plane: Section::Types,
        })
    );
    Ok(())
}

#[test]
fn image_carries_signatures_partial_roles_and_flags() -> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let widget = fix.class(b"Demo.Widget", 6, 12);
    let method_name = fix.atom(b"brew");
    let param_name = fix.atom(b"count");
    let int_spelling = fix.atom(b"System.Int32");
    let int_row = u32::try_from(fix.types.len()).unwrap_or(u32::MAX);
    fix.types.push(TypeRow {
        kind: TypeNodeKind::Named as u8,
        nullable: NullabilityCell::None as u8,
        spelling: Some(int_spelling),
        children: Vec::new(),
    });
    let partial_byte = PartialRole::Definition as u8;
    let async_flag = 0x2;
    let explicit_interface_flag = 0x10;
    fix.declarations.push(DeclarationRow {
        kind: DeclarationKind::Method as u8,
        flags: async_flag | explicit_interface_flag,
        partial: partial_byte,
        ref_kind: RefKind::Value as u8,
        name: method_name,
        qualified: None,
        owner: Some(widget),
        declared_type: None,
        decl_start: 15,
        name_start: 21,
        name_end: 25,
        params: vec![ParamRow {
            ty: int_row,
            name: param_name,
            ref_kind: RefKind::Ref as u8,
            flags: 0x1,
            default: None,
            name_start: 27,
            name_end: 34,
        }],
        generics: Vec::new(),
        doc: None,
    });
    let source = b"class Widget { async partial void brew(ref params int count) {}";
    let bytes = fix.encode(source)?;
    let image = CSharpImage::open(&bytes)?;
    let declaration = image.declaration(1).map_err(ImageTestError::Image)?;
    if declaration.kind != DeclarationKind::Method
        || !declaration.flags.is_async
        || !declaration.flags.is_explicit_interface
        || declaration.partial != PartialRole::Definition
        || declaration.owner != Some(widget)
    {
        return Err(ImageTestError::MissingFact {
            fact: "method cells",
        });
    }
    let parameter = declaration
        .parameters
        .iter()
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing { fact: "parameter" })?;
    if parameter.name.bytes != b"count"
        || parameter.ref_kind != RefKind::Ref
        || !parameter.is_params
        || parameter.has_default
    {
        return Err(ImageTestError::MissingFact {
            fact: "parameter cells",
        });
    }
    Ok(())
}

#[test]
fn image_carries_generic_constraints_and_tuple_children() -> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let _ = fix.class(b"Demo.Widget", 6, 12);
    let generic_name = fix.atom(b"T");
    let variance = 0;
    let flag_bits = 0x1 | 0x10;
    let constraint_spelling = fix.atom(b"Demo.IPart");
    let constraint_row = u32::try_from(fix.types.len()).unwrap_or(u32::MAX);
    fix.types.push(TypeRow {
        kind: TypeNodeKind::Named as u8,
        nullable: NullabilityCell::None as u8,
        spelling: Some(constraint_spelling),
        children: Vec::new(),
    });
    let tuple_spelling = fix.atom(b"(int, System.String)");
    let label = fix.atom(b"Name");
    let tuple_row = u32::try_from(fix.types.len()).unwrap_or(u32::MAX);
    fix.types.push(TypeRow {
        kind: TypeNodeKind::Tuple as u8,
        nullable: NullabilityCell::None as u8,
        spelling: Some(tuple_spelling),
        children: vec![(None, constraint_row), (Some(label), constraint_row)],
    });
    let field_name = fix.atom(b"pair");
    fix.declarations.push(DeclarationRow {
        kind: DeclarationKind::Field as u8,
        flags: 0,
        partial: 0,
        ref_kind: 0,
        name: field_name,
        qualified: None,
        owner: Some(0),
        declared_type: Some(tuple_row),
        decl_start: 15,
        name_start: 45,
        name_end: 49,
        params: Vec::new(),
        generics: Vec::new(),
        doc: None,
    });
    // Attach one generic parameter row to the class declaration.
    fix.declarations[0].generics = vec![GenericRow {
        name: generic_name,
        constraints: vec![constraint_row],
        variance,
        flags: flag_bits,
    }];
    let source = b"class Widget<T> { (int, string Name) pair; }";
    let bytes = fix.encode(source)?;
    let image = match CSharpImage::open(&bytes) {
        Ok(image) => image,
        Err(fault) => {
            eprintln!("debug open fault {fault:?}");
            let mut hexdump = String::new();
            for (at, byte) in bytes.iter().enumerate().skip(340).take(140) {
                if at % 16 == 0 {
                    hexdump.push_str(&format!("\n{at:4}:"));
                }
                hexdump.push_str(&format!(" {byte:02x}"));
            }
            eprintln!("{hexdump}");
            eprintln!(
                "atoms={} decls={} params={} tparams={} tcons={} types={} tchildren={} attrs={} docs={} refs={}",
                fix.atoms.len(),
                fix.declarations.len(),
                fix.declarations
                    .iter()
                    .map(|d| d.params.len())
                    .sum::<usize>(),
                fix.declarations
                    .iter()
                    .map(|d| d.generics.len())
                    .sum::<usize>(),
                fix.declarations
                    .iter()
                    .map(|d| d
                        .generics
                        .iter()
                        .map(|g| g.constraints.len())
                        .sum::<usize>())
                    .sum::<usize>(),
                fix.types.len(),
                fix.types.iter().map(|t| t.children.len()).sum::<usize>(),
                fix.attributes.len(),
                fix.docs.len(),
                fix.references.len()
            );
            return Err(ImageTestError::Image(fault));
        }
    };
    let class = image.declaration(0).map_err(ImageTestError::Image)?;
    let mut generics = class.type_parameters.iter();
    let generic = generics
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "generic parameter",
        })?;
    if generic.name.bytes != b"T"
        || generic.variance != compiler_languages_csharp::VarianceTag::Invariant
        || !generic.reference_type
        || !generic.constructor
    {
        return Err(ImageTestError::MissingFact {
            fact: "generic cells",
        });
    }
    let mut constraints = generic.constraints;
    let constraint = constraints
        .next()
        .ok_or(ImageTestError::Missing { fact: "constraint" })?;
    let node = image.type_node(constraint)?;
    if node.spelling.map(|atom| atom.bytes) != Some(b"Demo.IPart".as_slice()) {
        return Err(ImageTestError::MissingFact {
            fact: "constraint node",
        });
    }
    let field = image.declaration(1).map_err(ImageTestError::Image)?;
    let tuple = image.type_node(
        field
            .declared_type
            .ok_or(ImageTestError::Missing { fact: "field type" })?,
    )?;
    if tuple.kind != TypeNodeKind::Tuple || tuple.children.len() != 2 {
        return Err(ImageTestError::MissingFact { fact: "tuple node" });
    }
    let mut children = tuple.children;
    let label = children.nth(1).ok_or(ImageTestError::Missing {
        fact: "labelled child",
    })?;
    if label.name.map(|atom| atom.bytes) != Some(b"Name".as_slice()) {
        return Err(ImageTestError::MissingFact {
            fact: "tuple label",
        });
    }
    Ok(())
}

#[test]
fn image_carries_references_with_closed_kinds_and_foreign_targets() -> Result<(), ImageTestError> {
    let mut fix = Fixture::default();
    let widget = fix.class(b"Demo.Widget", 6, 12);
    let spelling = fix.atom(b"System.Console.WriteLine");
    let file = fix.atom(b"Widget.cs");
    fix.references.push(ReferenceRow {
        owner: widget,
        target: None,
        spelling,
        file,
        start: 20,
        end: 44,
        kind: ReferenceTag::Invocation as u8,
    });
    fix.references.push(ReferenceRow {
        owner: widget,
        target: Some(widget),
        spelling,
        file,
        start: 50,
        end: 55,
        kind: ReferenceTag::UsingDirective as u8,
    });
    fix.references.push(ReferenceRow {
        owner: widget,
        target: Some(widget),
        spelling,
        file,
        start: 56,
        end: 60,
        kind: ReferenceTag::InterfaceImplementation as u8,
    });
    let source = b"class Widget { void m() { System.Console.WriteLine(); } }";
    let bytes = fix.encode(source)?;
    let image = CSharpImage::open(&bytes)?;
    let mut references = image.references();
    let first = references
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing { fact: "reference" })?;
    if first.owner != widget
        || first.target.is_some()
        || first.kind != ReferenceTag::Invocation
        || first.start != 20
        || first.end != 44
    {
        return Err(ImageTestError::MissingFact {
            fact: "foreign reference",
        });
    }
    let second = references
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "second reference",
        })?;
    if second.target != Some(widget) || second.kind != ReferenceTag::UsingDirective {
        return Err(ImageTestError::MissingFact {
            fact: "local reference",
        });
    }
    let third = references
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "third reference",
        })?;
    if third.target != Some(widget) || third.kind != ReferenceTag::InterfaceImplementation {
        return Err(ImageTestError::MissingFact {
            fact: "implementation binding",
        });
    }
    if references.next().is_some() {
        return Err(ImageTestError::MissingFact {
            fact: "exact reference count",
        });
    }
    Ok(())
}

fn plane_offset(image: &[u8], section: Section) -> Result<usize, ImageTestError> {
    let directory_index = section as usize - 1;
    let entry = DIRECTORY_OFFSET + directory_index * DIRECTORY_ENTRY_BYTES;
    if image.len() < entry + 16 {
        return Err(ImageTestError::MissingFact {
            fact: "directory entry",
        });
    }
    let bytes: [u8; 4] =
        image[entry + 8..entry + 12]
            .try_into()
            .map_err(|_| ImageTestError::MissingFact {
                fact: "fixed directory offset",
            })?;
    usize::try_from(u32::from_le_bytes(bytes)).map_err(|_| ImageTestError::FixtureOverflow {
        fact: "directory offset",
    })
}
