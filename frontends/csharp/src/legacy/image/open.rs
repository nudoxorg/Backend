//! Opens one Roslyn authority image and reads its canonical sections.

use super::*;

impl<'image> CSharpImage<'image> {
    /// Opens one complete image after checking its canonical envelope,
    /// domain-separated checksum, and every section's closed layout.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let found_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if found_magic != MAGIC {
            return Err(ImageError::Header(HeaderError::Magic {
                found: found_magic,
            }));
        }
        let version = u16_at(bytes, 4);
        if version != VERSION {
            return Err(ImageError::Header(HeaderError::Version { found: version }));
        }
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let section_count = usize::from(u16_at(bytes, 44));
        if section_count != SECTION_COUNT {
            return Err(ImageError::Header(HeaderError::SectionCount {
                found: section_count,
            }));
        }
        if bytes[46..DIRECTORY_OFFSET] != [0; 2] {
            return Err(ImageError::Header(HeaderError::Reserved));
        }
        let body_bytes = usize::try_from(u32_at(bytes, 8)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len().saturating_sub(HEADER_BYTES),
            })
        })?;
        if HEADER_BYTES.checked_add(body_bytes) != Some(bytes.len()) {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len().saturating_sub(HEADER_BYTES),
            }));
        }
        let sections = read_directory(bytes)?;
        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[SOURCE_DIGEST_OFFSET..SOURCE_DIGEST_OFFSET + 32]);
        let image = Self {
            bytes,
            sections,
            source_digest,
        };
        image.validate_digest()?;
        image.validate_atoms()?;
        image.validate_declarations()?;
        image.validate_parameters()?;
        image.validate_type_parameters()?;
        image.validate_type_constraints()?;
        image.validate_types()?;
        image.validate_type_children()?;
        image.validate_attributes()?;
        image.validate_docs()?;
        image.validate_references()?;
        Ok(image)
    }

    /// Returns the SHA-256 digest of the source Roslyn bound before emission.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Iterates all declarations attributed to the source-binding file.
    #[must_use]
    pub const fn declarations(self) -> DeclarationIter<'image> {
        DeclarationIter {
            image: self,
            next: 0,
        }
    }

    /// Resolves one declaration coordinate into its borrowed row.
    pub fn declaration(self, index: u32) -> Result<Declaration<'image>, ImageError> {
        let index = self.coordinate(
            index,
            Section::Declarations,
            self.section(Section::Declarations).count,
        )?;
        self.declaration_at(index)
    }

    /// Resolves one type coordinate into its borrowed recursive node.
    pub fn type_node(self, reference: TypeRef) -> Result<TypeNode<'image>, ImageError> {
        let index = self.coordinate(
            reference.0,
            Section::Types,
            self.section(Section::Types).count,
        )?;
        let row = self.row(Section::Types, index);
        let child_start = self.range_start(u32_at(row, 8), Section::TypeChildren, index)?;
        let child_count = usize::try_from(u32_at(row, 12)).map_err(|_| ImageError::Span {
            index,
            start: u32::try_from(child_start).unwrap_or(u32::MAX),
            end: u32::try_from(self.section(Section::TypeChildren).count).unwrap_or(u32::MAX),
        })?;
        let child_end = self.range_end(child_start, child_count, Section::TypeChildren, index)?;
        let kind = TypeNodeKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
            plane: Section::Types,
        })?;
        let nullable = NullabilityCell::decode(row[1]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[1],
            plane: Section::Types,
        })?;
        Ok(TypeNode {
            kind,
            nullable,
            has_return: row[3] & 0x1 != 0,
            spelling: self.optional_atom(u32_at(row, 4)),
            children: TypeChildIter {
                image: self,
                next: child_start,
                end: child_end,
            },
        })
    }

    /// Iterates every applied-attribute row in canonical order.
    #[must_use]
    pub const fn attributes(self) -> AttributeIter<'image> {
        AttributeIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates every documentation row in canonical order.
    #[must_use]
    pub const fn docs(self) -> DocIter<'image> {
        DocIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates every compiler-resolved reference in canonical order.
    #[must_use]
    pub const fn references(self) -> ReferenceIter<'image> {
        ReferenceIter {
            image: self,
            next: 0,
        }
    }

    /// Returns one validated declaration row.
    pub(super) fn declaration_at(self, index: usize) -> Result<Declaration<'image>, ImageError> {
        let row = self.row(Section::Declarations, index);
        let kind = DeclarationKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
            plane: Section::Declarations,
        })?;
        let flags = DeclarationFlags::decode(row[1]).ok_or(ImageError::DeclarationReserved {
            index,
            plane: Section::Declarations,
        })?;
        let partial = PartialRole::decode(row[2]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[2],
            plane: Section::Declarations,
        })?;
        let ref_kind = RefKind::decode(row[3]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[3],
            plane: Section::Declarations,
        })?;
        let name = self.atom(u32_at(row, 4))?;
        self.check_optional_atom(u32_at(row, 8))?;
        let owner = self.optional_coordinate(u32_at(row, 12), Section::Declarations)?;
        let declared_type = self.optional_type(u32_at(row, 16))?;
        let decl_start = u32_at(row, 20);
        let name_start = u32_at(row, 24);
        let name_end = u32_at(row, 28);
        let decl_end = u32_at(row, 48);
        if decl_start > name_start || name_start > name_end || name_end > decl_end {
            return Err(ImageError::Span {
                index,
                start: decl_start,
                end: name_end,
            });
        }
        let param_start = self.range_start(u32_at(row, 32), Section::Parameters, index)?;
        let param_count = usize::from(u16_at(row, 36));
        let param_end = self.range_end(param_start, param_count, Section::Parameters, index)?;
        let generic_start = self.range_start(u32_at(row, 38), Section::TypeParameters, index)?;
        let generic_count = usize::from(u16_at(row, 42));
        let generic_end =
            self.range_end(generic_start, generic_count, Section::TypeParameters, index)?;
        let doc = self.optional_coordinate(u32_at(row, 44), Section::Docs)?;
        Ok(Declaration {
            kind,
            flags,
            partial,
            ref_kind,
            name,
            qualified: self.optional_atom(u32_at(row, 8)),
            owner,
            declared_type,
            decl_start,
            decl_end,
            name_start,
            name_end,
            parameters: ParamSlice {
                image: self,
                start: param_start,
                end: param_end,
            },
            type_parameters: GenericSlice {
                image: self,
                start: generic_start,
                end: generic_end,
            },
            doc,
        })
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&self.bytes[..IMAGE_DIGEST_OFFSET]);
        digest.update(&self.bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice()
            != &self.bytes[IMAGE_DIGEST_OFFSET..IMAGE_DIGEST_OFFSET + 32]
        {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_atoms(self) -> Result<(), ImageError> {
        let mut next_byte = 0usize;
        let count = self.section(Section::Atoms).count;
        for index in 0..count {
            let row = self.row(Section::Atoms, index);
            let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::NameRange {
                index,
                offset: u32::MAX,
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            })?;
            let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::NameRange {
                index,
                offset: u32::try_from(offset).unwrap_or(u32::MAX),
                length: u32::MAX,
                atom_bytes: self.section(Section::AtomBytes).count,
            })?;
            let end = offset
                .checked_add(length)
                .filter(|end| length > 0 && *end <= self.section(Section::AtomBytes).count)
                .ok_or(ImageError::NameRange {
                    index,
                    offset: u32::try_from(offset).unwrap_or(u32::MAX),
                    length: u32::try_from(length).unwrap_or(u32::MAX),
                    atom_bytes: self.section(Section::AtomBytes).count,
                })?;
            if str::from_utf8(self.atom_bytes(u32_at(row, 0))).is_err() {
                return Err(ImageError::NameUtf8 { index });
            }
            next_byte = end;
        }
        if next_byte != self.section(Section::AtomBytes).count {
            return Err(ImageError::NameRange {
                index: count,
                offset: u32::try_from(next_byte).unwrap_or(u32::MAX),
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            });
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Declarations).count {
            self.declaration_at(index)?;
        }
        Ok(())
    }

    fn validate_parameters(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Parameters).count {
            let row = self.row(Section::Parameters, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Types,
                self.section(Section::Types).count,
            )?;
            self.atom(u32_at(row, 4))?;
            if RefKind::decode(row[8]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[8],
                    plane: Section::Parameters,
                });
            }
            if row[9] & !0x3 != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Parameters,
                });
            }
            if row[10..12] != [0; 2] {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Parameters,
                });
            }
            self.check_optional_atom(u32_at(row, 12))?;
            let start = u32_at(row, 16);
            let end = u32_at(row, 20);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    fn validate_type_parameters(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeParameters).count {
            let row = self.row(Section::TypeParameters, index);
            self.atom(u32_at(row, 0))?;
            let start = self.range_start(u32_at(row, 4), Section::TypeConstraints, index)?;
            let count = usize::from(u16_at(row, 8));
            let _ = self.range_end(start, count, Section::TypeConstraints, index)?;
            if VarianceTag::decode(row[10]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[10],
                    plane: Section::TypeParameters,
                });
            }
            if row[11] & !0x3f != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::TypeParameters,
                });
            }
        }
        Ok(())
    }

    fn validate_type_constraints(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeConstraints).count {
            self.coordinate(
                u32_at(self.row(Section::TypeConstraints, index), 0),
                Section::Types,
                self.section(Section::Types).count,
            )?;
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Types).count {
            let row = self.row(Section::Types, index);
            let kind = TypeNodeKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
                index,
                found: row[0],
                plane: Section::Types,
            })?;
            if NullabilityCell::decode(row[1]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[1],
                    plane: Section::Types,
                });
            }
            if row[2] != 0 || row[3] & !0x1 != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Types,
                });
            }
            self.check_optional_atom(u32_at(row, 4))?;
            let start = self.range_start(u32_at(row, 8), Section::TypeChildren, index)?;
            let count = usize::try_from(u32_at(row, 12)).map_err(|_| ImageError::Span {
                index,
                start: u32::try_from(start).unwrap_or(u32::MAX),
                end: u32::try_from(self.section(Section::TypeChildren).count).unwrap_or(u32::MAX),
            })?;
            self.range_end(start, count, Section::TypeChildren, index)?;
            let (min, max) = kind.child_law();
            if count < min || count > max {
                return Err(ImageError::TypeChildCount {
                    index,
                    kind,
                    min,
                    max,
                    actual: count,
                });
            }
        }
        Ok(())
    }

    fn validate_type_children(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeChildren).count {
            let row = self.row(Section::TypeChildren, index);
            self.coordinate(
                u32_at(row, 4),
                Section::Types,
                self.section(Section::Types).count,
            )?;
            self.check_optional_atom(u32_at(row, 0))?;
        }
        Ok(())
    }

    fn validate_attributes(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Attributes).count {
            let row = self.row(Section::Attributes, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.atom(u32_at(row, 4))?;
        }
        Ok(())
    }

    fn validate_docs(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Docs).count {
            let row = self.row(Section::Docs, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.atom(u32_at(row, 4))?;
            self.atom(u32_at(row, 16))?;
            let start = u32_at(row, 8);
            let end = u32_at(row, 12);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::References).count {
            let row = self.row(Section::References, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.optional_coordinate(u32_at(row, 4), Section::Declarations)?;
            self.atom(u32_at(row, 8))?;
            self.atom(u32_at(row, 12))?;
            if ReferenceTag::decode(row[24]).is_none() || row[25..28] != [0; 3] {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[24],
                    plane: Section::References,
                });
            }
            let start = u32_at(row, 16);
            let end = u32_at(row, 20);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    /// Returns the bounds of one canonical section.
    pub(super) fn section(self, section: Section) -> SectionBounds {
        self.sections[section as usize - 1]
    }

    /// Borrows one section row.
    pub(super) fn row(self, section: Section, index: usize) -> &'image [u8] {
        let bounds = self.section(section);
        let start = bounds.offset + index * section.row_bytes();
        &self.bytes[start..start + section.row_bytes()]
    }

    /// Borrows the UTF-8 bytes of one atom.
    pub(super) fn atom_bytes(self, raw: u32) -> &'image [u8] {
        let Some(index) = self.valid_atom_index(raw) else {
            return &[];
        };
        let row = self.row(Section::Atoms, index);
        let Ok(offset) = usize::try_from(u32_at(row, 0)) else {
            return &[];
        };
        let Ok(length) = usize::try_from(u32_at(row, 4)) else {
            return &[];
        };
        let base = self.section(Section::AtomBytes).offset;
        &self.bytes[base + offset..base + offset + length]
    }

    /// Resolves one atom or a name-range fault.
    pub(super) fn atom(self, raw: u32) -> Result<Atom<'image>, ImageError> {
        let index = raw;
        if self.valid_atom_index(index).is_none() {
            return Err(ImageError::NameRange {
                index: usize::try_from(raw).unwrap_or(usize::MAX),
                offset: raw,
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            });
        }
        Ok(Atom {
            bytes: self.atom_bytes(raw),
        })
    }

    fn valid_atom_index(self, raw: u32) -> Option<usize> {
        if raw == ABSENT {
            return None;
        }
        let index = usize::try_from(raw).ok()?;
        (index < self.section(Section::Atoms).count).then_some(index)
    }

    fn check_optional_atom(self, raw: u32) -> Result<(), ImageError> {
        if raw == ABSENT {
            return Ok(());
        }
        self.atom(raw).map(|_| ())
    }

    /// Resolves an optional atom, with absence as `None`.
    pub(super) fn optional_atom(self, raw: u32) -> Option<Atom<'image>> {
        self.valid_atom_index(raw).map(|_| Atom {
            bytes: self.atom_bytes(raw),
        })
    }

    fn optional_type(self, raw: u32) -> Result<Option<TypeRef>, ImageError> {
        if raw == ABSENT {
            return Ok(None);
        }
        let index = self.coordinate(raw, Section::Types, self.section(Section::Types).count)?;
        Ok(Some(TypeRef(u32::try_from(index).unwrap_or(u32::MAX))))
    }

    /// Resolves an optional coordinate in one section.
    pub(super) fn optional_coordinate(
        self,
        raw: u32,
        section: Section,
    ) -> Result<Option<u32>, ImageError> {
        if raw == ABSENT {
            return Ok(None);
        }
        let index = self.coordinate(raw, section, self.section(section).count)?;
        Ok(Some(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Resolves one coordinate against a section bound.
    pub(super) fn coordinate(
        self,
        raw: u32,
        _section: Section,
        bound: usize,
    ) -> Result<usize, ImageError> {
        let index = usize::try_from(raw).map_err(|_| ImageError::Span {
            index: usize::MAX,
            start: raw,
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })?;
        if index >= bound {
            return Err(ImageError::Span {
                index,
                start: raw,
                end: u32::try_from(bound).unwrap_or(u32::MAX),
            });
        }
        Ok(index)
    }

    fn range_start(self, raw: u32, section: Section, row: usize) -> Result<usize, ImageError> {
        let bound = self.section(section).count;
        usize::try_from(raw).map_err(|_| ImageError::Span {
            index: row,
            start: raw,
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })
    }

    fn range_end(
        self,
        start: usize,
        count: usize,
        section: Section,
        row: usize,
    ) -> Result<usize, ImageError> {
        let bound = self.section(section).count;
        let end = start.checked_add(count).ok_or(ImageError::Span {
            index: row,
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })?;
        if end > bound {
            return Err(ImageError::Span {
                index: row,
                start: u32::try_from(end).unwrap_or(u32::MAX),
                end: u32::try_from(bound).unwrap_or(u32::MAX),
            });
        }
        Ok(end)
    }
}
