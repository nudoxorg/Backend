//! Opens a Java authority image and resolves its plane coordinates.
use super::*;

impl<'image> JavaImage<'image> {
    /// Opens one complete image after checking its canonical envelope and all planes.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < V1_HEADER_BYTES {
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
        let section_count = match version {
            V1 => V1_SECTIONS,
            V2 | V3 => V2_SECTIONS,
            V4 => V4_SECTIONS,
            _ => return Err(ImageError::Header(HeaderError::Version { found: version })),
        };
        let header_size = DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES * section_count;
        if bytes.len() < header_size {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let header_bytes = u16_at(bytes, 6);
        if usize::from(header_bytes) != header_size {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let release_raw = u16_at(bytes, 8);
        let release =
            decode_release(release_raw).ok_or(ImageError::Header(HeaderError::Release {
                found: release_raw,
            }))?;
        let declared_section_count = u16_at(bytes, 10);
        if usize::from(declared_section_count) != section_count {
            return Err(ImageError::Header(HeaderError::SectionCount {
                found: declared_section_count,
            }));
        }

        let body_bytes = usize::try_from(u32_at(bytes, 12)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len() - header_size,
            })
        })?;
        let image_bytes = header_size
            .checked_add(body_bytes)
            .ok_or(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - header_size,
            }))?;
        if image_bytes != bytes.len() {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - header_size,
            }));
        }

        let sections = read_directory(bytes, section_count, header_size, version)?;
        let image = Self {
            bytes,
            sections,
            section_count,
            version,
            release,
        };
        image.validate_digest()?;
        image.validate_planes()?;
        Ok(image)
    }

    /// Borrows the complete canonical image bytes.
    #[must_use]
    pub const fn as_bytes(self) -> &'image [u8] {
        self.bytes
    }

    /// Iterates atom bytes in canonical directory order.
    #[must_use]
    pub const fn atoms(self) -> AtomIter<'image> {
        AtomIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates fixed-width type facts in canonical order.
    #[must_use]
    pub const fn types(self) -> TypeIter<'image> {
        TypeIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates executable symbols in canonical order.
    #[must_use]
    pub const fn symbols(self) -> SymbolIter<'image> {
        SymbolIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates declaration facts in canonical order.
    #[must_use]
    pub const fn declarations(self) -> DeclarationIter<'image> {
        DeclarationIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates compiler-resolved call references in canonical order.
    #[must_use]
    pub const fn references(self) -> ReferenceIter<'image> {
        ReferenceIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates compiler-resolved non-invocation uses in canonical order.
    /// Every version below v4 carries no such plane, so the cursor is empty.
    #[must_use]
    pub const fn uses(self) -> UseIter<'image> {
        UseIter {
            image: self,
            next: 0,
        }
    }

    /// Returns the source extent of one declaration ordinal. Versions below
    /// v4 and unpositioned synthetic rows (packages or modules emitted
    /// without a tree) carry no extent.
    pub fn declaration_extent(self, ordinal: usize) -> Result<DeclarationExtent, ImageError> {
        if self.version < V4 {
            return Ok(DeclarationExtent::absent());
        }
        let index = row_index(
            ordinal,
            ImagePlane::Declarations,
            self.section(ImagePlane::Declarations).count,
        )? as usize;
        let row = self.row(ImagePlane::DeclarationExtents, index);
        Ok(DeclarationExtent {
            start: optional_offset(u32_at(row, 0)),
            end: optional_offset(u32_at(row, 4)),
        })
    }

    /// Returns the borrowed extension cursor for one declaration ordinal.
    pub fn declaration_extensions(
        self,
        ordinal: usize,
    ) -> Result<ExtensionIter<'image>, ImageError> {
        if self.version == V1 {
            return Ok(ExtensionIter {
                image: self,
                next: 0,
                end: 0,
            });
        }
        let ordinal = row_index(
            ordinal,
            ImagePlane::Declarations,
            self.section(ImagePlane::Declarations).count,
        )? as usize;
        let row = self.row(ImagePlane::DeclarationExtensions, ordinal);
        self.extension_range(u32_at(row, 0) as usize, u32_at(row, 4) as usize)
    }

    /// Resolves one type coordinate into its borrowed type fact.
    pub fn type_fact(self, reference: TypeRef) -> Result<TypeFact<'image>, ImageError> {
        self.type_at(reference.ordinal)
    }

    /// Resolves one executable coordinate into its borrowed symbol fact.
    pub fn symbol(self, reference: SymbolRef) -> Result<Symbol<'image>, ImageError> {
        self.symbol_at(reference.ordinal)
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(match self.version {
            V1 => V1_DIGEST_DOMAIN,
            V2 => V2_DIGEST_DOMAIN,
            V3 => V3_DIGEST_DOMAIN,
            _ => V4_DIGEST_DOMAIN,
        });
        digest.update(&self.bytes[..16]);
        let header_bytes = self.header_bytes();
        digest.update(&self.bytes[DIRECTORY_OFFSET..header_bytes]);
        digest.update(&self.bytes[header_bytes..]);
        if digest.finalize().as_slice() != &self.bytes[16..DIRECTORY_OFFSET] {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_planes(self) -> Result<(), ImageError> {
        self.validate_atoms()?;
        self.validate_types()?;
        self.validate_symbols()?;
        self.validate_declarations()?;
        self.validate_references()?;
        if self.version == V2 {
            self.validate_extensions()?;
        }
        if self.version >= V4 {
            self.validate_uses()?;
            self.validate_extents()?;
        }
        Ok(())
    }

    fn validate_atoms(self) -> Result<(), ImageError> {
        let mut next_byte = 0;
        for index in 0..self.section(ImagePlane::Atoms).count {
            let row = self.row(ImagePlane::Atoms, index);
            let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            if offset != next_byte {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::NonCanonicalOffset { found: offset },
                });
            }
            let end = offset.checked_add(length).ok_or(ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            if end > self.section(ImagePlane::AtomBytes).count {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::Range,
                });
            }
            if str::from_utf8(self.atom_bytes(offset, length)).is_err() {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::Utf8,
                });
            }
            next_byte = end;
        }
        if next_byte != self.section(ImagePlane::AtomBytes).count {
            return Err(ImageError::Atom {
                index: self.section(ImagePlane::Atoms).count,
                cause: AtomError::TrailingBytes,
            });
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::TypeChildren).count {
            self.type_at(u32_at(self.row(ImagePlane::TypeChildren, index), 0))?;
        }
        for index in 0..self.section(ImagePlane::Types).count {
            self.type_at(row_index(
                index,
                ImagePlane::Types,
                self.section(ImagePlane::Types).count,
            )?)?;
        }
        Ok(())
    }

    fn validate_symbols(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::SymbolParameters).count {
            let row = self.row(ImagePlane::SymbolParameters, index);
            self.type_at(u32_at(row, 0))?;
            if self.version >= V3 {
                // The declared parameter name is optional: the absent sentinel
                // is allowed, but any present coordinate must resolve to a
                // validated atom.
                self.optional_atom(u32_at(row, 4))?;
            }
        }
        for index in 0..self.section(ImagePlane::Symbols).count {
            self.symbol_at(row_index(
                index,
                ImagePlane::Symbols,
                self.section(ImagePlane::Symbols).count,
            )?)?;
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::Declarations).count {
            Declaration::decode(self, self.row(ImagePlane::Declarations, index))?;
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::References).count {
            Reference::decode(self, self.row(ImagePlane::References, index))?;
        }
        Ok(())
    }

    fn validate_uses(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::ResolvedUses).count {
            ResolvedUse::decode(self, self.row(ImagePlane::ResolvedUses, index))?;
        }
        Ok(())
    }

    fn validate_extents(self) -> Result<(), ImageError> {
        if self.section(ImagePlane::DeclarationExtents).count
            != self.section(ImagePlane::Declarations).count
        {
            return Err(ImageError::Coordinate {
                plane: ImagePlane::DeclarationExtents,
                index: self.section(ImagePlane::DeclarationExtents).count,
                upper_bound: self.section(ImagePlane::Declarations).count,
            });
        }
        for index in 0..self.section(ImagePlane::DeclarationExtents).count {
            let row = self.row(ImagePlane::DeclarationExtents, index);
            let start = u32_at(row, 0);
            let end = u32_at(row, 4);
            if start != ABSENT && end != ABSENT && start > end {
                return Err(ImageError::ReferenceRange { start, end });
            }
        }
        Ok(())
    }

    fn validate_extensions(self) -> Result<(), ImageError> {
        let mut previous_end = 0;
        for index in 0..self.section(ImagePlane::DeclarationExtensions).count {
            let row = self.row(ImagePlane::DeclarationExtensions, index);
            let start = u32_at(row, 0) as usize;
            let count = u32_at(row, 4) as usize;
            if (count == 0 && start != 0) || (count != 0 && start < previous_end) {
                return Err(ImageError::ChildRange {
                    plane: ImagePlane::ExtensionEntries,
                    start,
                    count,
                    upper_bound: self.section(ImagePlane::ExtensionEntries).count,
                });
            }
            for entry in self.extension_range(start, count)? {
                let entry = entry?;
                match entry {
                    DeclarationExtension::Throws(reference) => {
                        self.type_at(reference.ordinal)?;
                    }
                    DeclarationExtension::Annotation(_) => {}
                    DeclarationExtension::RecordComponent(index) => {
                        let declaration_index = coordinate(
                            index as u32,
                            ImagePlane::Declarations,
                            self.section(ImagePlane::Declarations).count,
                        )?;
                        let declaration = Declaration::decode(
                            self,
                            self.row(ImagePlane::Declarations, declaration_index),
                        )?;
                        if !matches!(
                            declaration.kind,
                            DeclarationKind::Field | DeclarationKind::EnumConstant
                        ) {
                            return Err(ImageError::RecordComponentKind { index });
                        }
                    }
                }
            }
            previous_end = start + count;
        }
        Ok(())
    }

    fn extension_range(
        self,
        start: usize,
        count: usize,
    ) -> Result<ExtensionIter<'image>, ImageError> {
        let upper_bound = self.section(ImagePlane::ExtensionEntries).count;
        let end = start.checked_add(count).ok_or(ImageError::ChildRange {
            plane: ImagePlane::ExtensionEntries,
            start,
            count,
            upper_bound,
        })?;
        if end > upper_bound {
            return Err(ImageError::ChildRange {
                plane: ImagePlane::ExtensionEntries,
                start,
                count,
                upper_bound,
            });
        }
        Ok(ExtensionIter {
            image: self,
            next: start,
            end,
        })
    }

    /// Returns the validated directory entry for one image plane.
    pub(super) fn section(self, plane: ImagePlane) -> Section {
        self.sections[plane as usize - 1]
    }

    fn header_bytes(self) -> usize {
        DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES * self.section_count
    }

    /// Borrows one fixed-width row from a validated plane.
    pub(super) fn row(self, plane: ImagePlane, index: usize) -> &'image [u8] {
        let section = self.section(plane);
        let row_bytes = plane_row_bytes(plane, self.version);
        let start = section.offset + index * row_bytes;
        &self.bytes[start..start + row_bytes]
    }

    fn atom_bytes(self, offset: usize, length: usize) -> &'image [u8] {
        let start = self.section(ImagePlane::AtomBytes).offset + offset;
        &self.bytes[start..start + length]
    }

    /// Resolves one atom coordinate to its borrowed UTF-8 bytes.
    pub(super) fn atom(self, raw: u32) -> Result<Atom<'image>, ImageError> {
        if raw == ABSENT {
            return Err(ImageError::AbsentAtom);
        }
        let index = usize::try_from(raw).map_err(|_| ImageError::Coordinate {
            plane: ImagePlane::Atoms,
            index: usize::MAX,
            upper_bound: self.section(ImagePlane::Atoms).count,
        })?;
        if index >= self.section(ImagePlane::Atoms).count {
            return Err(ImageError::Coordinate {
                plane: ImagePlane::Atoms,
                index,
                upper_bound: self.section(ImagePlane::Atoms).count,
            });
        }
        let row = self.row(ImagePlane::Atoms, index);
        let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::Atom {
            index,
            cause: AtomError::Range,
        })?;
        let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::Atom {
            index,
            cause: AtomError::Range,
        })?;
        Ok(Atom {
            bytes: self.atom_bytes(offset, length),
        })
    }

    /// Resolves one atom coordinate, treating the absent sentinel as no atom.
    pub(super) fn optional_atom(self, raw: u32) -> Result<Option<Atom<'image>>, ImageError> {
        if raw == ABSENT {
            Ok(None)
        } else {
            self.atom(raw).map(Some)
        }
    }

    /// Decodes one type-plane row by coordinate.
    pub(super) fn type_at(self, raw: u32) -> Result<TypeFact<'image>, ImageError> {
        let index = coordinate(
            raw,
            ImagePlane::Types,
            self.section(ImagePlane::Types).count,
        )?;
        TypeFact::decode(self, index)
    }

    /// Decodes one symbol-plane row by coordinate.
    pub(super) fn symbol_at(self, raw: u32) -> Result<Symbol<'image>, ImageError> {
        let index = coordinate(
            raw,
            ImagePlane::Symbols,
            self.section(ImagePlane::Symbols).count,
        )?;
        Symbol::decode(self, index)
    }
}
