//! Exercises the borrowed Java authority image with independent byte-level attacks.
//! The fixture is constructed as an image, never by serializing production DTOs.
//! Each mutation recomputes SHA-256 so structural checks cannot be bypassed by a checksum error.

use core::ops::Deref;

use compiler_languages_java::{
    DeclarationExtension, DeclarationKind, ImageError, ImagePlane, JavaImage, JavaRelease, TypeKind,
};
use sha2::{Digest, Sha256};

const V1_HEADER_BYTES: usize = 176;
const V2_HEADER_BYTES: usize = 208;
const DIRECTORY_OFFSET: usize = 48;
const DIRECTORY_ENTRY_BYTES: usize = 16;
const V1_DIGEST_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v1\0";
const V2_DIGEST_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v2\0";
const TYPES_DIRECTORY_INDEX: usize = 2;
const DECLARATIONS_DIRECTORY_INDEX: usize = 6;
const DECLARATION_EXTENSIONS_DIRECTORY_INDEX: usize = 8;
const EXTENSION_ENTRIES_DIRECTORY_INDEX: usize = 9;
const UNKNOWN_MODIFIER_BIT: u32 = 1 << 31;

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

#[test]
fn image_borrows_overload_identity_without_dto_reconstruction() -> Result<(), ImageTestError> {
    let bytes = fixture_image()?;
    let image = JavaImage::open(&bytes)?;
    if image.release != JavaRelease::Java21 {
        return Err(ImageTestError::MissingFact {
            fact: "closed Java release",
        });
    }
    let declaration = image
        .declarations()
        .next()
        .transpose()?
        .ok_or(ImageTestError::Missing {
            fact: "declaration",
        })?;
    if declaration.kind != DeclarationKind::Method {
        return Err(ImageTestError::MissingFact {
            fact: "method declaration kind",
        });
    }
    let name = declaration
        .name
        .utf8()
        .map_err(|_| ImageTestError::MissingFact { fact: "UTF-8 atom" })?;
    if name != "brew" {
        return Err(ImageTestError::Text {
            expected: "brew",
            actual: String::from(name),
        });
    }
    let symbol = declaration.symbol.ok_or(ImageTestError::Missing {
        fact: "overload symbol",
    })?;
    let parameter = image
        .symbol(symbol)?
        .parameters
        .next()
        .ok_or(ImageTestError::Missing {
            fact: "overload parameter",
        })?;
    let parameter = image.type_fact(parameter)?;
    if parameter.kind != TypeKind::Primitive {
        return Err(ImageTestError::MissingFact {
            fact: "primitive parameter type",
        });
    }
    let spelling = parameter.spelling.ok_or(ImageTestError::Missing {
        fact: "type spelling",
    })?;
    if spelling.deref() != b"int" {
        return Err(ImageTestError::MissingFact {
            fact: "borrowed atom bytes",
        });
    }
    Ok(())
}

#[test]
fn v2_extensions_preserve_typed_order_and_v1_is_empty() -> Result<(), ImageTestError> {
    let bytes = fixture_image()?;
    let image = JavaImage::open(&bytes)?;
    let mut extensions = image.declaration_extensions(0)?;
    let first = extensions.next().ok_or(ImageTestError::Missing {
        fact: "throws extension",
    })??;
    let DeclarationExtension::Throws(reference) = first else {
        return Err(ImageTestError::MissingFact {
            fact: "typed throws extension",
        });
    };
    image.type_fact(reference)?;
    let second = extensions.next().ok_or(ImageTestError::Missing {
        fact: "annotation extension",
    })??;
    let third = extensions.next().ok_or(ImageTestError::Missing {
        fact: "second annotation extension",
    })??;
    for extension in [second, third] {
        if !matches!(extension, DeclarationExtension::Annotation(_)) {
            return Err(ImageTestError::MissingFact {
                fact: "typed annotation extension",
            });
        }
    }
    if extensions.next().is_some()
        || image.declaration_extensions(1)?.next()
            != Some(Ok(DeclarationExtension::RecordComponent(1)))
    {
        return Err(ImageTestError::MissingFact {
            fact: "record-component extension order",
        });
    }

    let v1_bytes = fixture_v1()?;
    let v1 = JavaImage::open(&v1_bytes)?;
    if v1.declaration_extensions(0)?.next().is_some() {
        return Err(ImageTestError::MissingFact {
            fact: "v1 empty extension cursor",
        });
    }
    Ok(())
}

#[test]
fn v2_extensions_reject_each_hostile_plane_mutation() -> Result<(), ImageTestError> {
    let valid = fixture_image()?;
    for index in [
        DECLARATION_EXTENSIONS_DIRECTORY_INDEX,
        EXTENSION_ENTRIES_DIRECTORY_INDEX,
    ] {
        let mut mutant = valid.clone();
        let offset = plane_offset(&mutant, index)?;
        mutant[offset] ^= 1;
        if !matches!(JavaImage::open(&mutant), Err(ImageError::Digest)) {
            return Err(ImageTestError::MissingFact {
                fact: "post-checksum v2 plane mutation rejection",
            });
        }
    }
    for (index, expected) in [
        (
            DECLARATION_EXTENSIONS_DIRECTORY_INDEX,
            ImagePlane::DeclarationExtensions,
        ),
        (
            EXTENSION_ENTRIES_DIRECTORY_INDEX,
            ImagePlane::ExtensionEntries,
        ),
    ] {
        let mut mutant = valid.clone();
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        let length = u32_at(&mutant, entry + 12) - 1;
        mutant[entry + 12..entry + 16].copy_from_slice(&length.to_le_bytes());
        checksum(&mut mutant, 2);
        if !matches!(JavaImage::open(&mutant), Err(ImageError::Section { plane: found, cause: compiler_languages_java::SectionError::ByteCount { .. } }) if found == expected)
        {
            return Err(ImageTestError::MissingFact {
                fact: "v2 plane truncation rejection",
            });
        }
    }

    let mut mutant = valid.clone();
    let entry = DIRECTORY_OFFSET + DECLARATION_EXTENSIONS_DIRECTORY_INDEX * DIRECTORY_ENTRY_BYTES;
    mutant[entry + 4..entry + 8].copy_from_slice(&1_u32.to_le_bytes());
    checksum(&mut mutant, 2);
    if !matches!(
        JavaImage::open(&mutant),
        Err(ImageError::Section {
            plane: ImagePlane::DeclarationExtensions,
            cause: compiler_languages_java::SectionError::ByteCount { .. }
        })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "extension row count rejection",
        });
    }

    for (offset, value) in [(0, 4_u32), (4, 99_u32)] {
        let mut mutant = valid.clone();
        let plane = plane_offset(&mutant, DECLARATION_EXTENSIONS_DIRECTORY_INDEX)?;
        mutant[plane + offset..plane + offset + 4].copy_from_slice(&value.to_le_bytes());
        checksum(&mut mutant, 2);
        if !matches!(
            JavaImage::open(&mutant),
            Err(ImageError::ChildRange {
                plane: ImagePlane::ExtensionEntries,
                ..
            })
        ) {
            return Err(ImageTestError::MissingFact {
                fact: "extension range rejection",
            });
        }
    }

    let entry = plane_offset(&valid, EXTENSION_ENTRIES_DIRECTORY_INDEX)?;
    for (field, expected) in [
        (
            0,
            ImageError::Tag {
                plane: ImagePlane::ExtensionEntries,
                found: 9,
            },
        ),
        (1, ImageError::ExtensionReserved),
    ] {
        let mut mutant = valid.clone();
        mutant[entry + field] = if field == 0 { 9 } else { 1 };
        checksum(&mut mutant, 2);
        if JavaImage::open(&mutant).err() != Some(expected) {
            return Err(ImageTestError::MissingFact {
                fact: "extension tag/reserved rejection",
            });
        }
    }

    let cases = [
        (4, 99_u32, ImagePlane::Types),
        (12, 99_u32, ImagePlane::Atoms),
    ];
    for (entry_offset, value, plane) in cases {
        let mut mutant = valid.clone();
        let extension = plane_offset(&mutant, EXTENSION_ENTRIES_DIRECTORY_INDEX)?;
        mutant[extension + entry_offset..extension + entry_offset + 4]
            .copy_from_slice(&value.to_le_bytes());
        checksum(&mut mutant, 2);
        let error = JavaImage::open(&mutant).err();
        if !matches!(error, Some(ImageError::Coordinate { plane: found, .. }) if found == plane) {
            return Err(ImageTestError::MissingFact {
                fact: "extension coordinate rejection",
            });
        }
    }

    let mut mutant = valid;
    let extension = plane_offset(&mutant, EXTENSION_ENTRIES_DIRECTORY_INDEX)? + 24;
    mutant[extension + 4..extension + 8].copy_from_slice(&0_u32.to_le_bytes());
    checksum(&mut mutant, 2);
    if !matches!(
        JavaImage::open(&mutant),
        Err(ImageError::RecordComponentKind { index: 0 })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "record component kind rejection",
        });
    }
    Ok(())
}

#[test]
fn image_rejects_checksum_and_post_checksum_structural_mutations() -> Result<(), ImageTestError> {
    let valid = fixture_image()?;
    let mut checksum_mutant = valid.clone();
    checksum_mutant[V2_HEADER_BYTES] ^= 1;
    if !matches!(JavaImage::open(&checksum_mutant), Err(ImageError::Digest)) {
        return Err(ImageTestError::MissingFact {
            fact: "checksum mutation rejection",
        });
    }

    let mut tag_mutant = valid.clone();
    let declarations = plane_offset(&tag_mutant, DECLARATIONS_DIRECTORY_INDEX)?;
    tag_mutant[declarations] = 0;
    checksum(&mut tag_mutant, 2);
    if !matches!(
        JavaImage::open(&tag_mutant),
        Err(ImageError::Tag {
            plane: ImagePlane::Declarations,
            ..
        })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "closed declaration tag rejection",
        });
    }

    let mut child_mutant = valid;
    let types = plane_offset(&child_mutant, TYPES_DIRECTORY_INDEX)?;
    child_mutant[types + 2..types + 4].copy_from_slice(&1_u16.to_le_bytes());
    checksum(&mut child_mutant, 2);
    if !matches!(
        JavaImage::open(&child_mutant),
        Err(ImageError::ChildRange {
            plane: ImagePlane::TypeChildren,
            ..
        })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "type-child range rejection",
        });
    }

    let mut modifier_mutant = fixture_image()?;
    let declarations = plane_offset(&modifier_mutant, DECLARATIONS_DIRECTORY_INDEX)?;
    modifier_mutant[declarations + 4..declarations + 8]
        .copy_from_slice(&UNKNOWN_MODIFIER_BIT.to_le_bytes());
    checksum(&mut modifier_mutant, 2);
    if !matches!(
        JavaImage::open(&modifier_mutant),
        Err(ImageError::ModifierBits { .. })
    ) {
        return Err(ImageTestError::MissingFact {
            fact: "closed modifier rejection",
        });
    }
    Ok(())
}

fn fixture_image() -> Result<Vec<u8>, ImageTestError> {
    fixture(2)
}

fn fixture_v1() -> Result<Vec<u8>, ImageTestError> {
    fixture(1)
}

fn fixture(version: u16) -> Result<Vec<u8>, ImageTestError> {
    let atom_text = [
        b"demo.Cafe".as_slice(),
        b"brew",
        b"Cafe.java",
        b"int",
        b"runs",
        b"value",
        b"java.io.IOException",
        b"@Deprecated",
        b"@Anno",
    ];
    let mut atoms = Vec::new();
    let mut atom_bytes = Vec::new();
    for text in atom_text {
        push_u32(
            &mut atoms,
            u32::try_from(atom_bytes.len()).map_err(|_| ImageTestError::FixtureOverflow {
                fact: "atom offset",
            })?,
        );
        push_u32(
            &mut atoms,
            u32::try_from(text.len()).map_err(|_| ImageTestError::FixtureOverflow {
                fact: "atom length",
            })?,
        );
        atom_bytes.extend_from_slice(text);
    }
    let mut types = Vec::new();
    types.extend_from_slice(&[1, 0]);
    push_u16(&mut types, 0);
    push_u32(&mut types, 3);
    push_u32(&mut types, 0);
    types.extend_from_slice(&[3, 0, 0, 0]);
    push_u32(&mut types, 6);
    push_u32(&mut types, 0);
    push_u32(&mut types, 0);
    push_u32(&mut types, 0);
    let type_children = Vec::new();
    let mut symbols = Vec::new();
    push_u32(&mut symbols, 0);
    push_u32(&mut symbols, 1);
    push_u32(&mut symbols, 0);
    push_u16(&mut symbols, 1);
    push_u16(&mut symbols, 0);
    let mut parameters = Vec::new();
    push_u32(&mut parameters, 0);
    let mut declarations = Vec::new();
    declarations.extend_from_slice(&[11, 1, 1, 0]);
    push_u32(&mut declarations, 1);
    push_u32(&mut declarations, 1);
    push_u32(&mut declarations, 0);
    push_u32(&mut declarations, 4);
    push_u32(&mut declarations, 0);
    push_u32(&mut declarations, 0);
    push_u32(&mut declarations, 0);
    declarations.extend_from_slice(&[8, 1, 0, 0]);
    push_u32(&mut declarations, 0);
    push_u32(&mut declarations, 5);
    push_u32(&mut declarations, u32::MAX);
    push_u32(&mut declarations, u32::MAX);
    push_u32(&mut declarations, u32::MAX);
    push_u32(&mut declarations, u32::MAX);
    push_u32(&mut declarations, 0);
    let mut references = Vec::new();
    for value in [0, 0, 2, 3, 6] {
        push_u32(&mut references, value);
    }

    let declaration_extensions = [0_u32, 3, 3, 1]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let extension_entries = [(1_u8, 1_u32), (2, 7), (2, 8), (3, 1)]
        .into_iter()
        .flat_map(|(tag, value)| [tag, 0, 0, 0].into_iter().chain(value.to_le_bytes()))
        .collect::<Vec<_>>();
    let mut sections = vec![
        atoms,
        atom_bytes,
        types,
        type_children,
        symbols,
        parameters,
        declarations,
        references,
    ];
    if version == 2 {
        sections.extend([declaration_extensions, extension_entries]);
    }
    let row_bytes = if version == 1 {
        vec![8_u16, 1, 16, 4, 16, 4, 32, 20]
    } else {
        vec![8_u16, 1, 16, 4, 16, 4, 32, 20, 8, 8]
    };
    let header_bytes = if version == 1 {
        V1_HEADER_BYTES
    } else {
        V2_HEADER_BYTES
    };
    let mut image = vec![0; header_bytes];
    image[..4].copy_from_slice(b"NJAI");
    image[4..6].copy_from_slice(&version.to_le_bytes());
    image[6..8].copy_from_slice(
        &u16::try_from(header_bytes)
            .map_err(|_| ImageTestError::FixtureOverflow {
                fact: "fixed header length",
            })?
            .to_le_bytes(),
    );
    image[8..10].copy_from_slice(&21_u16.to_le_bytes());
    image[10..12].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    let body_bytes = sections.iter().map(Vec::len).sum::<usize>();
    image[12..16].copy_from_slice(
        &u32::try_from(body_bytes)
            .map_err(|_| ImageTestError::FixtureOverflow {
                fact: "body byte length",
            })?
            .to_le_bytes(),
    );
    let mut offset = header_bytes;
    for (index, section) in sections.iter().enumerate() {
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        image[entry..entry + 2].copy_from_slice(
            &u16::try_from(index + 1)
                .map_err(|_| ImageTestError::FixtureOverflow { fact: "plane tag" })?
                .to_le_bytes(),
        );
        image[entry + 2..entry + 4].copy_from_slice(&row_bytes[index].to_le_bytes());
        image[entry + 4..entry + 8].copy_from_slice(
            &u32::try_from(section.len() / usize::from(row_bytes[index]))
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "plane count",
                })?
                .to_le_bytes(),
        );
        image[entry + 8..entry + 12].copy_from_slice(
            &u32::try_from(offset)
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "plane offset",
                })?
                .to_le_bytes(),
        );
        image[entry + 12..entry + 16].copy_from_slice(
            &u32::try_from(section.len())
                .map_err(|_| ImageTestError::FixtureOverflow {
                    fact: "plane byte length",
                })?
                .to_le_bytes(),
        );
        offset += section.len();
    }
    for section in sections {
        image.extend_from_slice(&section);
    }
    checksum(&mut image, version);
    Ok(image)
}

fn plane_offset(image: &[u8], directory_index: usize) -> Result<usize, ImageTestError> {
    let entry = DIRECTORY_OFFSET + directory_index * DIRECTORY_ENTRY_BYTES;
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

fn checksum(image: &mut [u8], version: u16) {
    let mut digest = Sha256::new();
    digest.update(if version == 1 {
        V1_DIGEST_DOMAIN
    } else {
        V2_DIGEST_DOMAIN
    });
    digest.update(&image[..16]);
    let header_bytes = if version == 1 {
        V1_HEADER_BYTES
    } else {
        V2_HEADER_BYTES
    };
    digest.update(&image[DIRECTORY_OFFSET..header_bytes]);
    digest.update(&image[header_bytes..]);
    image[16..DIRECTORY_OFFSET].copy_from_slice(&digest.finalize());
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
