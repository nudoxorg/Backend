//! Reads validated rows from one borrowed Go authority image.
use super::*;

impl<'image> GoImage<'image> {
    /// Opens one complete fixed-layout Go authority image and proves every
    /// structural law before lending any row.
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
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(ImageError::Header(HeaderError::Version { found: version }));
        }
        let (declaration_bytes, method_bytes, reference_bytes, digest_domain) = if version == 6 {
            (
                DECLARATION_BYTES_V6,
                METHOD_BYTES_V6,
                REFERENCE_BYTES_V6,
                DIGEST_DOMAIN_V6,
            )
        } else {
            (
                DECLARATION_BYTES_V5,
                METHOD_BYTES_V5,
                REFERENCE_BYTES_V5,
                DIGEST_DOMAIN_V5,
            )
        };
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let actual_body = bytes.len() - HEADER_BYTES;
        let declaration_count = plane_count(bytes, 8, actual_body)?;
        let atom_bytes = plane_count(bytes, 12, actual_body)?;
        let body_bytes = plane_count(bytes, 16, actual_body)?;
        let type_count = plane_count(bytes, 84, actual_body)?;
        let reference_count = plane_count(bytes, 88, actual_body)?;
        let method_count = plane_count(bytes, 92, actual_body)?;
        let type_parameter_count = plane_count(bytes, 96, actual_body)?;
        let member_count = plane_count(bytes, 100, actual_body)?;
        let doc_count = plane_count(bytes, 104, actual_body)?;
        let constraint_count = plane_count(bytes, 108, actual_body)?;
        let satisfaction_count = plane_count(bytes, 112, actual_body)?;
        let module_count = plane_count(bytes, 116, actual_body)?;
        if module_count > 1 {
            return Err(ImageError::Header(HeaderError::ModuleCount {
                found: module_count,
            }));
        }
        let package_count = plane_count(bytes, 120, actual_body)?;
        let signature_parameter_count = plane_count(bytes, 124, actual_body)?;
        let method_set_count = plane_count(bytes, 128, actual_body)?;
        let unresolved_cgo_count = plane_count(bytes, 132, actual_body)?;

        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[20..52]);

        // Body planes, in frozen order: declarations, types, methods, type
        // parameters, members, docs, references, constraints, satisfactions,
        // module, packages, signature parameters, interface method sets,
        // pooled children, unresolved-cgo cells, atoms.
        let declarations_offset = HEADER_BYTES;
        let types_offset = declarations_offset + declaration_count * declaration_bytes;
        let methods_offset = types_offset + type_count * TYPE_ROW_BYTES;
        let type_parameters_offset = methods_offset + method_count * method_bytes;
        let members_offset = type_parameters_offset + type_parameter_count * TYPE_PARAMETER_BYTES;
        let docs_offset = members_offset + member_count * MEMBER_BYTES;
        let references_offset = docs_offset + doc_count * DOC_BYTES;
        let constraints_offset = references_offset + reference_count * reference_bytes;
        let satisfactions_offset = constraints_offset + constraint_count * CONSTRAINT_BYTES;
        let module_offset = satisfactions_offset + satisfaction_count * SATISFACTION_BYTES;
        let packages_offset = module_offset + module_count * MODULE_BYTES;
        let signature_parameters_offset = packages_offset + package_count * PACKAGE_BYTES;
        let method_sets_offset =
            signature_parameters_offset + signature_parameter_count * SIGNATURE_PARAMETER_BYTES;
        let children_offset = method_sets_offset + method_set_count * METHOD_SET_BYTES;
        let Some(atoms_end) = HEADER_BYTES.checked_add(body_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        if atoms_end != bytes.len() || atoms_end < children_offset {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }
        let cgo_plane_bytes =
            unresolved_cgo_count
                .checked_mul(CHILD_BYTES)
                .ok_or(ImageError::Header(HeaderError::BodyLength {
                    declared: body_bytes,
                    actual: actual_body,
                }))?;
        let Some(atom_offset) = atoms_end.checked_sub(atom_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        let Some(cgo_offset) = atom_offset.checked_sub(cgo_plane_bytes) else {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        };
        if cgo_offset < children_offset
            || !(cgo_offset - children_offset).is_multiple_of(CHILD_BYTES)
        {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }
        let child_count = (cgo_offset - children_offset) / CHILD_BYTES;
        // Every fixed plane must sit inside the children plane origin, so no
        // declared count can push a row read past the validated body.
        let chain = [
            declarations_offset,
            types_offset,
            methods_offset,
            type_parameters_offset,
            members_offset,
            docs_offset,
            references_offset,
            constraints_offset,
            satisfactions_offset,
            module_offset,
            packages_offset,
            signature_parameters_offset,
            method_sets_offset,
        ];
        if chain.iter().any(|offset| *offset > atom_offset) {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: actual_body,
            }));
        }

        let image = Self {
            bytes,
            version,
            declaration_bytes,
            method_bytes,
            reference_bytes,
            digest_domain,
            module_count,
            package_count,
            declaration_count,
            type_count,
            signature_parameter_count,
            method_count,
            type_parameter_count,
            member_count,
            method_set_count,
            doc_count,
            reference_count,
            constraint_count,
            satisfaction_count,
            child_count,
            unresolved_cgo_count,
            declarations_offset,
            types_offset,
            methods_offset,
            type_parameters_offset,
            members_offset,
            docs_offset,
            references_offset,
            constraints_offset,
            satisfactions_offset,
            module_offset,
            packages_offset,
            signature_parameters_offset,
            method_sets_offset,
            children_offset,
            cgo_offset,
            atom_offset,
            atom_bytes,
            source_digest,
        };
        image.validate_digest()?;
        image.validate_module()?;
        image.validate_declarations()?;
        image.validate_packages()?;
        image.validate_types()?;
        image.validate_methods()?;
        image.validate_type_parameters()?;
        image.validate_members()?;
        image.validate_method_sets()?;
        image.validate_docs()?;
        image.validate_references()?;
        image.validate_constraints()?;
        image.validate_satisfactions()?;
        Ok(image)
    }

    /// Returns the SHA-256 digest of the exact configured Go source file.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Number of validated declarations.
    #[must_use]
    pub const fn declaration_count(self) -> usize {
        self.declaration_count
    }

    /// Number of validated type rows.
    #[must_use]
    pub const fn type_count(self) -> usize {
        self.type_count
    }

    /// Number of validated method rows.
    #[must_use]
    pub const fn method_count(self) -> usize {
        self.method_count
    }

    /// Number of validated member rows.
    #[must_use]
    pub const fn member_count(self) -> usize {
        self.member_count
    }

    /// Number of validated type-parameter rows.
    #[must_use]
    pub const fn type_parameter_count(self) -> usize {
        self.type_parameter_count
    }

    /// Number of validated documentation rows.
    #[must_use]
    pub const fn doc_count(self) -> usize {
        self.doc_count
    }

    /// Number of validated reference rows.
    #[must_use]
    pub const fn reference_count(self) -> usize {
        self.reference_count
    }

    /// Number of validated build-constraint rows.
    #[must_use]
    pub const fn constraint_count(self) -> usize {
        self.constraint_count
    }

    /// Number of validated satisfaction rows.
    #[must_use]
    pub const fn satisfaction_count(self) -> usize {
        self.satisfaction_count
    }

    /// Number of unresolved-cgo name cells carried before the atom plane.
    #[must_use]
    pub const fn unresolved_cgo_count(self) -> usize {
        self.unresolved_cgo_count
    }

    /// Borrows one unresolved-cgo name atom by index.
    pub fn unresolved_cgo(self, index: usize) -> Result<&'image [u8], ImageError> {
        if index >= self.unresolved_cgo_count {
            return Err(ImageError::RowBounds {
                plane: "unresolved cgo",
                index,
                count: self.unresolved_cgo_count,
            });
        }
        let row = self.plane_row(self.cgo_offset, index, CHILD_BYTES);
        self.atom("unresolved cgo", index, u32_at(row, 0), u32_at(row, 4))
    }

    /// Borrows one validated declaration row.
    pub fn declaration(self, index: usize) -> Result<Declaration<'image>, ImageError> {
        if index >= self.declaration_count {
            return Err(ImageError::RowBounds {
                plane: "declaration",
                index,
                count: self.declaration_count,
            });
        }
        let row = self.plane_row(self.declarations_offset, index, self.declaration_bytes);
        let kind = DeclarationKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
        })?;
        let exported = flag_at(row, 1).ok_or(ImageError::ExportedFlag {
            index,
            found: row[1],
        })?;
        let iota = flag_at(row, 2).ok_or(ImageError::DeclarationIota {
            index,
            found: row[2],
        })?;
        if row[3] != 0 {
            return Err(ImageError::DeclarationReserved { index });
        }
        let name = self.atom("declaration", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "declaration",
                index,
            });
        }
        let package = self.atom("declaration", index, u32_at(row, 12), u32_at(row, 16))?;
        let type_root = optional_row(u32_at(row, 20));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::DeclarationTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let span = span_at(row, 24, 28, index)?;
        let file = self.atom("declaration", index, u32_at(row, 32), u32_at(row, 36))?;
        let value = self.atom("declaration", index, u32_at(row, 40), u32_at(row, 44))?;
        let const_group = i64::from_le_bytes([
            row[48], row[49], row[50], row[51], row[52], row[53], row[54], row[55],
        ]);
        // Version 6 tail: the declared identifier's byte extent and the
        // authority-bound flag. Version 5 rows carry neither fact.
        let (name_span, bound) = if self.version == 6 {
            let name_span = span_at(row, 56, 60, index)?;
            if let (Some((span_start, span_end)), Some((name_start, name_end))) = (span, name_span)
                && (name_start < span_start || name_end > span_end)
            {
                // The identifier must sit inside its own declaration: a
                // name extent escaping the span could never anchor an
                // owner-relative occurrence inside the owner.
                return Err(ImageError::DeclarationSpan {
                    index,
                    start: name_start,
                    end: name_end,
                });
            }
            let bound = flag_at(row, 64).ok_or(ImageError::DeclarationReserved { index })?;
            if row[65..72] != [0; 7] {
                return Err(ImageError::DeclarationReserved { index });
            }
            (name_span, bound)
        } else {
            // Version 5 rows end at the const-group cell: no v6 facts exist.
            (None, false)
        };
        Ok(Declaration {
            kind,
            exported,
            iota,
            name,
            package,
            type_root,
            span,
            name_span,
            bound,
            file,
            value,
            const_group,
        })
    }

    /// Iterates all package-scope declarations in producer order.
    pub fn declarations(self) -> impl Iterator<Item = Result<Declaration<'image>, ImageError>> {
        (0..self.declaration_count).map(move |index| self.declaration(index))
    }

    /// Borrows one validated type row.
    pub fn type_row(self, index: usize) -> Result<TypeRow<'image>, ImageError> {
        if index >= self.type_count {
            return Err(ImageError::RowBounds {
                plane: "type",
                index,
                count: self.type_count,
            });
        }
        let row = self.plane_row(self.types_offset, index, TYPE_ROW_BYTES);
        let kind = TypeRowKind::decode(row[0]).ok_or(ImageError::TypeKind {
            index,
            found: row[0],
        })?;
        let dir = match row[1] {
            0 => ChanDir::Both,
            1 => ChanDir::Send,
            2 => ChanDir::Recv,
            found => return Err(ImageError::TypeDirection { index, found }),
        };
        let variadic = flag_at(row, 2).ok_or(ImageError::TypeVariadicFlag {
            index,
            found: row[2],
        })?;
        if row[3] != 0 || row[44..48] != [0; 4] {
            return Err(ImageError::TypeReserved { index });
        }
        let name = self.atom("type", index, u32_at(row, 4), u32_at(row, 8))?;
        let package = self.atom("type", index, u32_at(row, 12), u32_at(row, 16))?;
        let length = i64::from_le_bytes([
            row[20], row[21], row[22], row[23], row[24], row[25], row[26], row[27],
        ]);
        Ok(TypeRow {
            kind,
            dir,
            variadic,
            name,
            package,
            length,
            param_count: u32_at(row, 48),
            children: (u32_at(row, 28), u32_at(row, 32)),
            members: (u32_at(row, 36), u32_at(row, 40)),
        })
    }

    /// Borrows one validated pooled type child: target row plus term flags.
    pub fn type_child(self, index: usize) -> Result<(u32, bool), ImageError> {
        if index >= self.child_count {
            return Err(ImageError::RowBounds {
                plane: "type child",
                index,
                count: self.child_count,
            });
        }
        let row = self.plane_row(self.children_offset, index, CHILD_BYTES);
        let target = u32_at(row, 0);
        if usize::try_from(target).is_ok_and(|target| target >= self.type_count) {
            return Err(ImageError::TypeChildTarget {
                index,
                target,
                type_count: self.type_count,
            });
        }
        let flags = u32_at(row, 4);
        if flags & !CHILD_TILDE_FLAG != 0 {
            return Err(ImageError::TypeChildFlags { index, flags });
        }
        Ok((target, flags & CHILD_TILDE_FLAG == CHILD_TILDE_FLAG))
    }

    /// Borrows one validated method row.
    pub fn method(self, index: usize) -> Result<MethodRow<'image>, ImageError> {
        if index >= self.method_count {
            return Err(ImageError::RowBounds {
                plane: "method",
                index,
                count: self.method_count,
            });
        }
        let row = self.plane_row(self.methods_offset, index, self.method_bytes);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.declaration_count) {
            return Err(ImageError::MethodOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let exported = flag_at(row, 4).ok_or(ImageError::MethodFlag {
            index,
            cell: "exported",
            found: row[4],
        })?;
        let pointer_receiver = flag_at(row, 5).ok_or(ImageError::MethodFlag {
            index,
            cell: "pointer-receiver",
            found: row[5],
        })?;
        let promoted = flag_at(row, 6).ok_or(ImageError::MethodFlag {
            index,
            cell: "promoted",
            found: row[6],
        })?;
        if row[7] != 0 {
            return Err(ImageError::MethodReserved { index });
        }
        let name = self.atom("method", index, u32_at(row, 8), u32_at(row, 12))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "method",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 16));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MethodTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let receiver = self.atom("method", index, u32_at(row, 20), u32_at(row, 24))?;
        let blob_length = u32_at(row, 32);
        let receiver_type_params = self.atom("method", index, u32_at(row, 28), blob_length)?;
        let param_count = u32_at(row, 36);
        if split_nul(receiver_type_params, param_count).is_none() {
            return Err(ImageError::MethodReceiverParams {
                index,
                count: param_count,
                blob_bytes: blob_length,
            });
        }
        let origin = self.atom("method", index, u32_at(row, 40), u32_at(row, 44))?;
        let span = span_at(row, 48, 52, index)?;
        let file = self.atom("method", index, u32_at(row, 56), u32_at(row, 60))?;
        // Version 6 tail: the authority-bound flag. Version 5 rows carry no
        // such fact.
        let bound = if self.version == 6 {
            let bound = flag_at(row, 64).ok_or(ImageError::MethodReserved { index })?;
            if row[65..72] != [0; 7] {
                return Err(ImageError::MethodReserved { index });
            }
            bound
        } else {
            // Version 5 rows end at the file cell: no bound fact exists.
            false
        };
        Ok(MethodRow {
            owner,
            exported,
            pointer_receiver,
            promoted,
            name,
            type_root,
            receiver,
            receiver_type_params,
            origin,
            span,
            bound,
            file,
        })
    }

    /// Iterates every method row in producer order.
    pub fn methods(self) -> impl Iterator<Item = Result<MethodRow<'image>, ImageError>> {
        (0..self.method_count).map(move |index| self.method(index))
    }

    /// Borrows one validated type-parameter row.
    pub fn type_parameter(self, index: usize) -> Result<TypeParameterRow<'image>, ImageError> {
        if index >= self.type_parameter_count {
            return Err(ImageError::RowBounds {
                plane: "type parameter",
                index,
                count: self.type_parameter_count,
            });
        }
        let row = self.plane_row(self.type_parameters_offset, index, TYPE_PARAMETER_BYTES);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.declaration_count) {
            return Err(ImageError::TypeParameterOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let name = self.atom("type parameter", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "type parameter",
                index,
            });
        }
        let constraint = optional_row(u32_at(row, 12));
        if let Some(root) = out_of_bounds_root(constraint, self.type_count) {
            return Err(ImageError::TypeParameterConstraint {
                index,
                root,
                type_count: self.type_count,
            });
        }
        Ok(TypeParameterRow {
            owner,
            name,
            constraint,
        })
    }

    /// Borrows one validated member row.
    pub fn member(self, index: usize) -> Result<MemberRow<'image>, ImageError> {
        if index >= self.member_count {
            return Err(ImageError::RowBounds {
                plane: "member",
                index,
                count: self.member_count,
            });
        }
        let row = self.plane_row(self.members_offset, index, MEMBER_BYTES);
        let kind = match row[4] {
            0 => MemberKind::Field,
            1 => MemberKind::Method,
            found => return Err(ImageError::MemberKind { index, found }),
        };
        let embedded = flag_at(row, 5).ok_or(ImageError::MemberFlag {
            index,
            cell: "embedded",
            found: row[5],
        })?;
        if embedded && kind != MemberKind::Field {
            return Err(ImageError::MemberEmbedded { index });
        }
        let exported = flag_at(row, 6).ok_or(ImageError::MemberFlag {
            index,
            cell: "exported",
            found: row[6],
        })?;
        if row[7] != 0 || row[36..40] != [0; 4] {
            return Err(ImageError::MemberReserved { index });
        }
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::MemberOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let name = self.atom("member", index, u32_at(row, 8), u32_at(row, 12))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "member",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 16));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MemberTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        let tag = self.atom("member", index, u32_at(row, 20), u32_at(row, 24))?;
        let package = self.atom("member", index, u32_at(row, 28), u32_at(row, 32))?;
        Ok(MemberRow {
            owner,
            kind,
            embedded,
            exported,
            name,
            type_root,
            tag,
            package,
        })
    }

    /// Borrows one validated documentation row.
    pub fn doc(self, index: usize) -> Result<DocRow<'image>, ImageError> {
        if index >= self.doc_count {
            return Err(ImageError::RowBounds {
                plane: "doc",
                index,
                count: self.doc_count,
            });
        }
        let row = self.plane_row(self.docs_offset, index, DOC_BYTES);
        let owner_kind = match row[0] {
            0 => DocOwner::Declaration,
            1 => DocOwner::Method,
            2 => DocOwner::Member,
            3 => DocOwner::Package,
            found => return Err(ImageError::DocOwnerKind { index, found }),
        };
        if row[1..4] != [0; 3] {
            return Err(ImageError::DocReserved { index });
        }
        let bound = match owner_kind {
            DocOwner::Declaration => self.declaration_count,
            DocOwner::Method => self.method_count,
            DocOwner::Member => self.member_count,
            DocOwner::Package => self.declaration_count,
        };
        let owner = u32_at(row, 4);
        if usize::try_from(owner).is_ok_and(|owner| owner >= bound) {
            return Err(ImageError::DocOwner {
                index,
                owner,
                bound,
            });
        }
        let text = self.atom("doc", index, u32_at(row, 8), u32_at(row, 12))?;
        if text.is_empty() {
            return Err(ImageError::EmptyDoc { index });
        }
        Ok(DocRow {
            owner_kind,
            owner,
            text,
        })
    }

    /// Borrows one validated reference row with its resolved, owner-relative
    /// span.
    pub fn reference(self, index: usize) -> Result<ReferenceRow<'image>, ImageError> {
        if index >= self.reference_count {
            return Err(ImageError::RowBounds {
                plane: "reference",
                index,
                count: self.reference_count,
            });
        }
        let row = self.plane_row(self.references_offset, index, self.reference_bytes);
        let owner = u32_at(row, 0);
        let Ok(owner_index) = usize::try_from(owner) else {
            return Err(ImageError::ReferenceOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        };
        if owner_index >= self.declaration_count {
            return Err(ImageError::ReferenceOwner {
                index,
                owner,
                declaration_count: self.declaration_count,
            });
        }
        let target = self.atom("reference", index, u32_at(row, 4), u32_at(row, 8))?;
        if target.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "reference target",
                index,
            });
        }
        let target_package = self.atom("reference", index, u32_at(row, 12), u32_at(row, 16))?;
        let start = u32_at(row, 20);
        let end = u32_at(row, 24);
        if start > end {
            return Err(ImageError::ReferenceSpan { index, start, end });
        }
        let file = self.atom("reference", index, u32_at(row, 28), u32_at(row, 32))?;
        let receiver = self.atom("reference", index, u32_at(row, 36), u32_at(row, 40))?;
        // Version 6: the closed use kind, the used object's closed class,
        // and the target receiver type-name atom. Version 5 rows carry a
        // four-byte reserved tail and only ever record free-function calls.
        let (use_kind, target_class, recv_type) = if self.version == 6 {
            let use_kind =
                ReferenceUseKind::decode(row[44]).ok_or(ImageError::ReferenceReserved { index })?;
            let target_class = ReferenceTargetClass::decode(row[45])
                .ok_or(ImageError::ReferenceReserved { index })?;
            if row[46..48] != [0; 2] {
                return Err(ImageError::ReferenceReserved { index });
            }
            let recv_type = self.atom("reference", index, u32_at(row, 48), u32_at(row, 52))?;
            (use_kind, target_class, recv_type)
        } else {
            if row[44..48] != [0; 4] {
                return Err(ImageError::ReferenceReserved { index });
            }
            (
                ReferenceUseKind::Call,
                ReferenceTargetClass::Func,
                &self.bytes[0..0],
            )
        };
        let (owner_is_declaration, owner_row, owner_span, owner_file) = if receiver.is_empty() {
            let declaration = self.declaration(owner_index)?;
            (true, owner, declaration.span, declaration.file)
        } else {
            match self.method_containing(owner_index, start, end, file) {
                Some(method_index) => {
                    let method = self.method(method_index)?;
                    (false, method_index as u32, method.span, method.file)
                }
                None => {
                    return Err(ImageError::ReferenceOwnerUnresolved {
                        index,
                        owner_bytes: receiver.len(),
                        function_bytes: target.len(),
                    });
                }
            }
        };
        let Some((owner_start, owner_end)) = owner_span else {
            return Err(ImageError::ReferenceOwnerSpan { index });
        };
        if file != owner_file {
            return Err(ImageError::ReferenceFile { index });
        }
        if start < owner_start || end > owner_end {
            return Err(ImageError::ReferenceContainment {
                index,
                start,
                end,
                owner_start,
                owner_end,
            });
        }
        Ok(ReferenceRow {
            owner,
            target,
            target_package,
            receiver,
            span: (start, end),
            file,
            owner_is_declaration,
            owner_row,
            relative: (start - owner_start, end - owner_start),
            use_kind,
            target_class,
            recv_type,
        })
    }

    /// Iterates every reference row in producer order.
    pub fn references(self) -> impl Iterator<Item = Result<ReferenceRow<'image>, ImageError>> {
        (0..self.reference_count).map(move |index| self.reference(index))
    }

    /// Borrows one validated build-constraint row.
    pub fn constraint(self, index: usize) -> Result<ConstraintRow<'image>, ImageError> {
        if index >= self.constraint_count {
            return Err(ImageError::RowBounds {
                plane: "build constraint",
                index,
                count: self.constraint_count,
            });
        }
        let row = self.plane_row(self.constraints_offset, index, CONSTRAINT_BYTES);
        let file = self.atom("build constraint", index, u32_at(row, 0), u32_at(row, 4))?;
        if file.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "build constraint",
                index,
            });
        }
        let constraint = self.atom("build constraint", index, u32_at(row, 8), u32_at(row, 12))?;
        if constraint.is_empty() {
            return Err(ImageError::EmptyConstraint { index });
        }
        let blob_length = u32_at(row, 20);
        let exported = self.atom("build constraint", index, u32_at(row, 16), blob_length)?;
        let exported_count = u32_at(row, 24);
        if parse_constraint_blob(exported, exported_count).is_none() {
            return Err(ImageError::ConstraintBlob {
                index,
                count: exported_count,
                blob_bytes: blob_length,
            });
        }
        Ok(ConstraintRow {
            file,
            constraint,
            exported,
            exported_count,
        })
    }

    /// Iterates every build-constraint row in producer order.
    pub fn constraints(self) -> impl Iterator<Item = Result<ConstraintRow<'image>, ImageError>> {
        (0..self.constraint_count).map(move |index| self.constraint(index))
    }

    /// Borrows one validated interface-satisfaction row.
    pub fn satisfaction(self, index: usize) -> Result<SatisfactionRow<'image>, ImageError> {
        if index >= self.satisfaction_count {
            return Err(ImageError::RowBounds {
                plane: "satisfaction",
                index,
                count: self.satisfaction_count,
            });
        }
        let row = self.plane_row(self.satisfactions_offset, index, SATISFACTION_BYTES);
        let subject = u32_at(row, 0);
        if usize::try_from(subject).is_ok_and(|subject| subject >= self.declaration_count) {
            return Err(ImageError::SatisfactionSubject {
                index,
                subject,
                declaration_count: self.declaration_count,
            });
        }
        let target = self.atom("satisfaction", index, u32_at(row, 4), u32_at(row, 8))?;
        if target.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "satisfaction target",
                index,
            });
        }
        let target_package = self.atom("satisfaction", index, u32_at(row, 12), u32_at(row, 16))?;
        Ok(SatisfactionRow {
            subject,
            target,
            target_package,
        })
    }

    /// Iterates every satisfaction row in producer order.
    pub fn satisfactions(
        self,
    ) -> impl Iterator<Item = Result<SatisfactionRow<'image>, ImageError>> {
        (0..self.satisfaction_count).map(move |index| self.satisfaction(index))
    }

    /// Borrows the validated module-metadata row, when the oracle resolved a
    /// module.
    pub fn module(self) -> Result<Option<ModuleRow<'image>>, ImageError> {
        if self.module_count == 0 {
            return Ok(None);
        }
        let row = self.plane_row(self.module_offset, 0, MODULE_BYTES);
        Ok(Some(ModuleRow {
            path: self.atom("module", 0, u32_at(row, 0), u32_at(row, 4))?,
            directory: self.atom("module", 0, u32_at(row, 8), u32_at(row, 12))?,
            go_version: self.atom("module", 0, u32_at(row, 16), u32_at(row, 20))?,
            version: self.atom("module", 0, u32_at(row, 24), u32_at(row, 28))?,
        }))
    }

    /// Number of validated package rows.
    #[must_use]
    pub const fn package_count(self) -> usize {
        self.package_count
    }

    /// Number of validated module rows (zero or one).
    #[must_use]
    pub const fn module_count(self) -> usize {
        self.module_count
    }

    /// Borrows one validated package row.
    pub fn package(self, index: usize) -> Result<PackageRow<'image>, ImageError> {
        if index >= self.package_count {
            return Err(ImageError::RowBounds {
                plane: "package",
                index,
                count: self.package_count,
            });
        }
        let row = self.plane_row(self.packages_offset, index, PACKAGE_BYTES);
        let import_path = self.atom("package", index, u32_at(row, 0), u32_at(row, 4))?;
        if import_path.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "package",
                index,
            });
        }
        let name = self.atom("package", index, u32_at(row, 8), u32_at(row, 12))?;
        let blob_bytes = u32_at(row, 20);
        let files = self.atom("package", index, u32_at(row, 16), blob_bytes)?;
        let file_count = u32_at(row, 24);
        if split_nul(files, file_count).is_none() {
            return Err(ImageError::PackageFiles {
                index,
                count: file_count,
                blob_bytes,
            });
        }
        Ok(PackageRow {
            import_path,
            name,
            files,
            file_count,
        })
    }

    /// Iterates every package row in producer order.
    pub fn packages(self) -> impl Iterator<Item = Result<PackageRow<'image>, ImageError>> {
        (0..self.package_count).map(move |index| self.package(index))
    }

    /// Number of validated signature-parameter rows.
    #[must_use]
    pub const fn signature_parameter_count(self) -> usize {
        self.signature_parameter_count
    }

    /// Borrows one validated signature-parameter row: the exact source name
    /// and source position of one parameter or result of one func type row.
    /// The name may be empty — Go permits unnamed parameters and results —
    /// and a file-less position must be fully absent.
    pub fn signature_parameter(
        self,
        index: usize,
    ) -> Result<SignatureParameterRow<'image>, ImageError> {
        if index >= self.signature_parameter_count {
            return Err(ImageError::RowBounds {
                plane: "signature parameter",
                index,
                count: self.signature_parameter_count,
            });
        }
        let row = self.plane_row(
            self.signature_parameters_offset,
            index,
            SIGNATURE_PARAMETER_BYTES,
        );
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::SignatureParameterOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let file = self.atom(
            "signature parameter",
            index,
            u32_at(row, 16),
            u32_at(row, 20),
        )?;
        let offset = u32_at(row, 24);
        if file.is_empty() != (offset == NONE) {
            return Err(ImageError::SignatureParameterPosition { index });
        }
        Ok(SignatureParameterRow {
            owner,
            ordinal: u32_at(row, 4),
            name: self.atom(
                "signature parameter",
                index,
                u32_at(row, 8),
                u32_at(row, 12),
            )?,
            file,
            offset,
        })
    }

    /// Iterates every signature-parameter row in producer order.
    pub fn signature_parameters(
        self,
    ) -> impl Iterator<Item = Result<SignatureParameterRow<'image>, ImageError>> {
        (0..self.signature_parameter_count).map(move |index| self.signature_parameter(index))
    }

    /// Number of validated method-set rows.
    #[must_use]
    pub const fn method_set_count(self) -> usize {
        self.method_set_count
    }

    /// Borrows one validated method-set row: one method of an interface
    /// type row's complete post-embedding method set.
    pub fn method_set(self, index: usize) -> Result<MethodSetRow<'image>, ImageError> {
        if index >= self.method_set_count {
            return Err(ImageError::RowBounds {
                plane: "method set",
                index,
                count: self.method_set_count,
            });
        }
        let row = self.plane_row(self.method_sets_offset, index, METHOD_SET_BYTES);
        let owner = u32_at(row, 0);
        if usize::try_from(owner).is_ok_and(|owner| owner >= self.type_count) {
            return Err(ImageError::MethodSetOwner {
                index,
                owner,
                type_count: self.type_count,
            });
        }
        let name = self.atom("method set", index, u32_at(row, 4), u32_at(row, 8))?;
        if name.is_empty() {
            return Err(ImageError::EmptyName {
                plane: "method set",
                index,
            });
        }
        let type_root = optional_row(u32_at(row, 12));
        if let Some(root) = out_of_bounds_root(type_root, self.type_count) {
            return Err(ImageError::MethodSetTypeRoot {
                index,
                root,
                type_count: self.type_count,
            });
        }
        Ok(MethodSetRow {
            owner,
            name,
            type_root,
            package: self.atom("method set", index, u32_at(row, 16), u32_at(row, 20))?,
        })
    }

    /// Borrows one complete interface method-set row.
    pub fn interface_method_set(self, index: usize) -> Result<MethodSetRow<'image>, ImageError> {
        self.method_set(index)
    }

    /// Number of complete interface method-set rows.
    #[must_use]
    pub const fn interface_method_set_count(self) -> usize {
        self.method_set_count
    }

    /// Iterates every method-set row in producer order.
    pub fn method_sets(self) -> impl Iterator<Item = Result<MethodSetRow<'image>, ImageError>> {
        (0..self.method_set_count).map(move |index| self.method_set(index))
    }

    /// Resolves the method row declared on the receiver type `receiver` with
    /// the method name `name`, if any.
    #[must_use]
    pub fn method_row_of(self, name: &[u8], receiver: &[u8]) -> Option<usize> {
        for index in 0..self.method_count {
            let Ok(row) = self.method(index) else {
                continue;
            };
            let Ok(owner) = self.declaration(usize::try_from(row.owner).ok()?) else {
                continue;
            };
            if row.name == name && owner.name == receiver {
                return Some(index);
            }
        }
        None
    }

    /// Resolves the calling method row of one reference: the method owned by
    /// `owner` whose file and span contain the call site, if any.
    fn method_containing(self, owner: usize, start: u32, end: u32, file: &[u8]) -> Option<usize> {
        for index in 0..self.method_count {
            let Ok(row) = self.method(index) else {
                continue;
            };
            if usize::try_from(row.owner) != Ok(owner) || row.file != file {
                continue;
            }
            if let Some((row_start, row_end)) = row.span
                && start >= row_start
                && end <= row_end
            {
                return Some(index);
            }
        }
        None
    }

    fn plane_row(self, plane_offset: usize, index: usize, width: usize) -> &'image [u8] {
        let start = plane_offset + index * width;
        &self.bytes[start..start + width]
    }

    fn atom(
        self,
        plane: &'static str,
        index: usize,
        offset: u32,
        length: u32,
    ) -> Result<&'image [u8], ImageError> {
        let range_fault = || ImageError::AtomRange {
            plane,
            index,
            offset: usize::MAX,
            length: usize::MAX,
            atom_bytes: self.atom_bytes,
        };
        let offset = usize::try_from(offset).map_err(|_| range_fault())?;
        let length = usize::try_from(length).map_err(|_| range_fault())?;
        let Some(end) = offset.checked_add(length) else {
            return Err(range_fault());
        };
        if end > self.atom_bytes {
            return Err(ImageError::AtomRange {
                plane,
                index,
                offset,
                length,
                atom_bytes: self.atom_bytes,
            });
        }
        if length == 0 {
            return Ok(&self.bytes[0..0]);
        }
        let start = self.atom_offset + offset;
        let bytes = &self.bytes[start..start + length];
        if str::from_utf8(bytes).is_err() {
            return Err(ImageError::AtomUtf8 { plane, index });
        }
        Ok(bytes)
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(self.digest_domain);
        digest.update(&self.bytes[..52]);
        digest.update(&self.bytes[84..HEADER_BYTES]);
        digest.update(&self.bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice() != &self.bytes[52..84] {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.declaration_count {
            self.declaration(index)?;
        }
        Ok(())
    }

    fn validate_module(self) -> Result<(), ImageError> {
        if let Some(module) = self.module()?
            && module.path.is_empty()
        {
            return Err(ImageError::ModulePath);
        }
        Ok(())
    }

    /// Package rows must be canonically ordered by import path, and every
    /// declaration must name one of the package rows' import paths.
    fn validate_packages(self) -> Result<(), ImageError> {
        let mut previous_path: Option<&'image [u8]> = None;
        for index in 0..self.package_count {
            let row = self.package(index)?;
            if previous_path.is_some_and(|previous| row.import_path <= previous) {
                return Err(ImageError::PackageSort { index });
            }
            previous_path = Some(row.import_path);
        }
        for index in 0..self.declaration_count {
            let declared = self.declaration(index)?;
            let mut bound = false;
            for package_index in 0..self.package_count {
                let row = self.package(package_index)?;
                if declared.package == row.import_path {
                    bound = true;
                    break;
                }
            }
            if !bound {
                return Err(ImageError::DeclarationPackage {
                    index,
                    package_count: self.package_count,
                });
            }
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        let mut expected_child = 0_usize;
        let mut signature_parameter_cursor = 0_usize;
        for index in 0..self.type_count {
            let row = self.type_row(index)?;
            let name_required = matches!(
                row.kind,
                TypeRowKind::Basic
                    | TypeRowKind::Named
                    | TypeRowKind::Alias
                    | TypeRowKind::TypeParam
            );
            if name_required && row.name.is_empty() {
                return Err(ImageError::TypeNameRequired {
                    index,
                    kind: row.kind,
                });
            }
            let name_forbidden = matches!(
                row.kind,
                TypeRowKind::Pointer
                    | TypeRowKind::Slice
                    | TypeRowKind::Array
                    | TypeRowKind::Map
                    | TypeRowKind::Chan
                    | TypeRowKind::Func
                    | TypeRowKind::Struct
                    | TypeRowKind::Interface
                    | TypeRowKind::Union
                    | TypeRowKind::Tuple
            );
            if name_forbidden && !row.name.is_empty() {
                return Err(ImageError::TypeNameForbidden {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind != TypeRowKind::Chan && row.dir != ChanDir::Both {
                return Err(ImageError::TypeDirectionCell {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind != TypeRowKind::Func && row.variadic {
                return Err(ImageError::TypeVariadicCell {
                    index,
                    kind: row.kind,
                });
            }
            if row.kind == TypeRowKind::Array && row.length < 0 {
                return Err(ImageError::ArrayLength {
                    index,
                    length: row.length,
                });
            }
            let (min_children, max_children) = match row.kind {
                TypeRowKind::Pointer
                | TypeRowKind::Slice
                | TypeRowKind::Array
                | TypeRowKind::Chan => (1, Some(1)),
                TypeRowKind::Map => (2, Some(2)),
                TypeRowKind::Basic
                | TypeRowKind::TypeParam
                | TypeRowKind::Struct
                | TypeRowKind::Invalid => (0, Some(0)),
                TypeRowKind::Named
                | TypeRowKind::Alias
                | TypeRowKind::Func
                | TypeRowKind::Interface
                | TypeRowKind::Union
                | TypeRowKind::Tuple => (0, None),
            };
            let child_start =
                usize::try_from(row.children.0).map_err(|_| ImageError::TypeChildRange {
                    index,
                    start: usize::MAX,
                    count: usize::try_from(row.children.1).unwrap_or(usize::MAX),
                    child_count: self.child_count,
                })?;
            let child_count =
                usize::try_from(row.children.1).map_err(|_| ImageError::TypeChildRange {
                    index,
                    start: child_start,
                    count: usize::MAX,
                    child_count: self.child_count,
                })?;
            let param_count_law = match row.kind {
                TypeRowKind::Func => {
                    usize::try_from(row.param_count).is_ok_and(|count| count <= child_count)
                }
                _ => row.param_count == 0,
            };
            if !param_count_law {
                return Err(ImageError::TypeParamCount {
                    index,
                    kind: row.kind,
                    param_count: row.param_count,
                    child_count: row.children.1,
                });
            }
            let over_max = max_children.is_some_and(|max| child_count > max);
            let run_end = child_start.checked_add(child_count);
            if child_count < min_children
                || over_max
                || child_start != expected_child
                || run_end.is_none_or(|end| end > self.child_count)
            {
                return Err(ImageError::TypeChildRange {
                    index,
                    start: child_start,
                    count: child_count,
                    child_count: self.child_count,
                });
            }
            for child in child_start..child_start + child_count {
                self.type_child(child)?;
            }
            expected_child = child_start + child_count;
            let member_start =
                usize::try_from(row.members.0).map_err(|_| ImageError::TypeMemberRange {
                    index,
                    start: usize::MAX,
                    count: usize::try_from(row.members.1).unwrap_or(usize::MAX),
                    member_count: self.member_count,
                })?;
            let member_count =
                usize::try_from(row.members.1).map_err(|_| ImageError::TypeMemberRange {
                    index,
                    start: member_start,
                    count: usize::MAX,
                    member_count: self.member_count,
                })?;
            let member_law = match row.kind {
                TypeRowKind::Struct | TypeRowKind::Interface => None,
                _ => Some(0),
            };
            let member_end = member_start.checked_add(member_count);
            if member_law.is_some_and(|max| member_count > max)
                || member_end.is_none_or(|end| end > self.member_count)
            {
                return Err(ImageError::TypeMemberRange {
                    index,
                    start: member_start,
                    count: member_count,
                    member_count: self.member_count,
                });
            }
            // Every func row owns exactly one signature-parameter row per
            // child: parameters first, then results, ordinals ascending.
            if row.kind == TypeRowKind::Func {
                for ordinal in 0..child_count {
                    let row_index = signature_parameter_cursor + ordinal;
                    let parameter = self.signature_parameter(row_index)?;
                    if usize::try_from(parameter.owner).unwrap_or(usize::MAX) != index {
                        return Err(ImageError::SignatureParameterOwnerRow {
                            index: row_index,
                            owner: parameter.owner,
                            expected: u32::try_from(index).unwrap_or(u32::MAX),
                        });
                    }
                    if parameter.ordinal != u32::try_from(ordinal).unwrap_or(u32::MAX) {
                        return Err(ImageError::SignatureParameterOrdinal {
                            index: row_index,
                            ordinal: parameter.ordinal,
                            expected: u32::try_from(ordinal).unwrap_or(u32::MAX),
                        });
                    }
                }
                signature_parameter_cursor += child_count;
            }
        }
        if expected_child != self.child_count {
            return Err(ImageError::TypeChildTiling {
                declared: expected_child,
                plane: self.child_count,
            });
        }
        if signature_parameter_cursor != self.signature_parameter_count {
            return Err(ImageError::SignatureParameterTiling {
                declared: signature_parameter_cursor,
                plane: self.signature_parameter_count,
            });
        }
        Ok(())
    }

    fn validate_methods(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.method_count {
            let row = self.method(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::MethodSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        Ok(())
    }

    fn validate_type_parameters(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.type_parameter_count {
            let row = self.type_parameter(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::TypeParameterSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        Ok(())
    }

    fn validate_members(self) -> Result<(), ImageError> {
        let mut previous_owner = 0_u32;
        for index in 0..self.member_count {
            let row = self.member(index)?;
            if index > 0 && row.owner < previous_owner {
                return Err(ImageError::MemberSort {
                    index,
                    owner: row.owner,
                    previous: previous_owner,
                });
            }
            previous_owner = row.owner;
        }
        // Every owner's declared run must equal exactly its contiguous run
        // of same-owner rows, so the plane tiles without gaps or overlaps.
        let mut cursor = 0_usize;
        while cursor < self.member_count {
            let owner = self.member(cursor)?.owner;
            let mut end = cursor + 1;
            while end < self.member_count && self.member(end)?.owner == owner {
                end += 1;
            }
            let owner_index = usize::try_from(owner).map_err(|_| ImageError::MemberOwnerRange {
                owner,
                start: usize::MAX,
                count: 0,
                actual_start: cursor,
                actual_count: end - cursor,
            })?;
            let declared = self.type_row(owner_index)?.members;
            let declared_start = usize::try_from(declared.0).unwrap_or(usize::MAX);
            let declared_count = usize::try_from(declared.1).unwrap_or(usize::MAX);
            if declared_start != cursor || declared_count != end - cursor {
                return Err(ImageError::MemberOwnerRange {
                    owner,
                    start: declared_start,
                    count: declared_count,
                    actual_start: cursor,
                    actual_count: end - cursor,
                });
            }
            cursor = end;
        }
        Ok(())
    }

    /// Method-set rows must be canonically ordered by (owner, name) with
    /// owner-contiguous runs in type-row order, and every owner must be an
    /// interface type row.
    fn validate_method_sets(self) -> Result<(), ImageError> {
        let mut previous: Option<(u32, &'image [u8])> = None;
        for index in 0..self.method_set_count {
            let row = self.method_set(index)?;
            if previous.is_some_and(|(previous_owner, previous_name)| {
                row.owner < previous_owner
                    || (row.owner == previous_owner && row.name <= previous_name)
            }) {
                return Err(ImageError::MethodSetSort { index });
            }
            let owner = usize::try_from(row.owner).unwrap_or(usize::MAX);
            if owner < self.type_count && self.type_row(owner)?.kind != TypeRowKind::Interface {
                return Err(ImageError::MethodSetOwnerKind {
                    index,
                    kind: self.type_row(owner)?.kind,
                });
            }
            previous = Some((row.owner, row.name));
        }
        Ok(())
    }

    fn validate_docs(self) -> Result<(), ImageError> {
        let mut previous = (0_u8, 0_u32);
        for index in 0..self.doc_count {
            let row = self.doc(index)?;
            let key = (row.owner_kind as u8, row.owner);
            if index > 0 && key < previous {
                return Err(ImageError::DocSort {
                    index,
                    owner_kind: key.0,
                    owner: key.1,
                    previous_kind: previous.0,
                    previous_owner: previous.1,
                });
            }
            previous = key;
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        let mut previous: Option<(&'image [u8], u32)> = None;
        for index in 0..self.reference_count {
            let row = self.reference(index)?;
            if previous.is_some_and(|(previous_file, previous_start)| {
                row.file < previous_file
                    || (row.file == previous_file && row.span.0 < previous_start)
            }) {
                return Err(ImageError::ReferenceSort { index });
            }
            previous = Some((row.file, row.span.0));
        }
        Ok(())
    }

    fn validate_constraints(self) -> Result<(), ImageError> {
        let mut previous_file: Option<&'image [u8]> = None;
        for index in 0..self.constraint_count {
            let row = self.constraint(index)?;
            if previous_file.is_some_and(|previous| row.file < previous) {
                return Err(ImageError::ConstraintSort { index });
            }
            previous_file = Some(row.file);
        }
        Ok(())
    }

    /// Satisfaction rows must be canonically ordered by subject, and every
    /// subject must be a named-type declaration: the oracle only records
    /// method-set satisfaction for `type` declarations.
    fn validate_satisfactions(self) -> Result<(), ImageError> {
        let mut previous_subject = 0_u32;
        for index in 0..self.satisfaction_count {
            let row = self.satisfaction(index)?;
            if index > 0 && row.subject < previous_subject {
                return Err(ImageError::SatisfactionSort {
                    index,
                    subject: row.subject,
                    previous: previous_subject,
                });
            }
            let declaration = self.declaration(usize::try_from(row.subject).map_err(|_| {
                ImageError::SatisfactionSubject {
                    index,
                    subject: row.subject,
                    declaration_count: self.declaration_count,
                }
            })?)?;
            if declaration.kind != DeclarationKind::Type {
                return Err(ImageError::SatisfactionSubjectKind {
                    index,
                    kind: declaration.kind,
                });
            }
            previous_subject = row.subject;
        }
        Ok(())
    }
}
