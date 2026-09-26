//! Projects one libclang type graph into lattice rows.
//!
//! Pass one has already pushed the named declarations. This lane walks the
//! authority type graph from each declared root, interning nested compounds
//! as anonymous rows so every child coordinate is strictly backward.

use super::{
    ANON_RECORD_STRUCT, C_LONG_LONG_SPELLING, C_LONG_SPELLING, C_UNSIGNED_LONG_LONG_SPELLING,
    C_UNSIGNED_LONG_SPELLING, ClangCollectError, DEPTH_LIMIT, INTEGER_SIGNED_FLAG,
    INTEGER_WIDTH_SHIFT, Projected, ProjectionFault, Projector, SHAPE_BOOL, SHAPE_BUILTIN,
    SHAPE_C_BLOCK_POINTER, SHAPE_C_PLAIN_SIGNED_CHAR, SHAPE_C_PLAIN_UNSIGNED_CHAR, SHAPE_C_POINTER,
    SHAPE_C_SIGNED_CHAR, SHAPE_C_UNSIGNED_CHAR, SHAPE_C_WIDE_CHAR, SHAPE_C_WIDE_SIGNED_CHAR,
    SHAPE_C_WIDE_UNSIGNED_CHAR, SHAPE_CXX_LVALUE_REFERENCE, SHAPE_CXX_MEMBER_POINTER,
    SHAPE_CXX_RVALUE_REFERENCE, SHAPE_FLOAT, SHAPE_INTEGER, SHAPE_UTF16_CODE_UNIT,
    SHAPE_UTF32_CODE_UNIT, SYSTEM_FRAGMENT_DOMAIN, UNHOSTABLE, VOID_SPELLING, c_qualifiers, gap,
    lane_terminal, nominal_record, qualifiers_are_legal, span_contains, terminal, unknown_record,
};
use crate::driver::lower::MAX_TYPE_CHILDREN;
use crate::driver::types::FactFault;
use backend_frontend_clang::legacy::{
    DeclarationKind, IntegerRank, TypeFact, TypeId as AuthorityTypeId, TypeKind, TypeRelation,
};
use backend_semantic::ir::{
    DeclarationFamilyId, DeclarationIdentity, EntityKind, ExternalFragmentId, NominalRef,
    SemanticTypeRecord, SemanticTypeTag, StableRef, TypeReason, TypeWidth, VariantFingerprint,
};

impl<'authority, 'scratch, 'source> Projector<'authority, 'scratch, 'source> {
    /// Projects one authority type row onto a fact root: the record and its
    /// ordered child coordinates, with every nested compound interned as a
    /// pooled anonymous row.
    pub(super) fn project_root(
        &mut self,
        root: AuthorityTypeId,
    ) -> Result<Projected<'source>, ClangCollectError> {
        self.project_type(root, DEPTH_LIMIT)
    }

    /// Projects one authority type row into its lattice record and ordered
    /// child coordinates. Nested compound positions intern anonymous rows
    /// owned by the lane's current anchor; an anchorless lane folds the
    /// whole position to the typed oracle gap instead of a fabricated row.
    fn project_type(
        &mut self,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(row) = self.type_row(type_id) else {
            return Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        if depth == 0 {
            return Ok(Projected::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        let projected = match row.kind {
            TypeKind::Builtin => Ok(self.project_builtin(row)),
            TypeKind::Named => self.project_named(row, type_id, depth),
            TypeKind::Pointer => self.project_pointer(row, depth),
            TypeKind::BlockPointer => self.project_block_pointer(row, depth),
            TypeKind::MemberPointer => self.project_member_pointer(row, depth),
            TypeKind::LvalueReference | TypeKind::RvalueReference => {
                self.project_reference(row, depth)
            }
            TypeKind::Array => self.project_array(row, depth),
            TypeKind::Function => self.project_function_type(row, depth),
            TypeKind::Unknown => Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None))),
        }?;
        self.apply_c_qualifiers(row, projected)
    }

    /// Wraps any directly qualified authority row in one structural C-family
    /// qualifier node. The wrapper applies to exactly this row; children own
    /// their own qualifiers, which preserves `const T * const` instead of
    /// guessing pointee mutability from the outer pointer.
    fn apply_c_qualifiers(
        &mut self,
        row: &TypeFact,
        projected: Projected<'source>,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let qualifiers = c_qualifiers(row.qualifiers);
        if qualifiers == 0 {
            return Ok(projected);
        }
        if !qualifiers_are_legal(row.kind, row.qualifiers) {
            return Err(terminal(ProjectionFault::IllegalQualifierTarget {
                type_id: row.id,
                kind: row.kind,
                qualifiers: row.qualifiers,
            }));
        }
        let child = self.intern_row(projected)?;
        if child == UNHOSTABLE {
            return Ok(gap());
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::CQualified);
        record.payload0 = qualifiers;
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects one builtin row onto its exact width, signedness, and shape
    /// cells. Exotic builtins the classification keeps as `Other` have no
    /// spelling cell on the consumed surface and fold to the typed gap.
    fn project_builtin(&self, row: &TypeFact) -> Projected<'source> {
        let Some(builtin) = row.builtin else {
            return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let width = |bits: Option<u32>| match bits {
            Some(bits) => u32::try_from(bits).ok().map_or(None, |bits| {
                u32::try_from(TypeWidth::try_from_cell(bits).ok()?.to_cell()).ok()
            }),
            None => None,
        };
        let record = match builtin {
            backend_frontend_clang::legacy::BuiltinClass::Void => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(VOID_SPELLING);
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Bool => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BOOL;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::PlainCharSigned
            | backend_frontend_clang::legacy::BuiltinClass::PlainCharUnsigned
            | backend_frontend_clang::legacy::BuiltinClass::SignedChar
            | backend_frontend_clang::legacy::BuiltinClass::UnsignedChar
            | backend_frontend_clang::legacy::BuiltinClass::Utf16CodeUnit
            | backend_frontend_clang::legacy::BuiltinClass::Utf32CodeUnit
            | backend_frontend_clang::legacy::BuiltinClass::WideCharSigned
            | backend_frontend_clang::legacy::BuiltinClass::WideCharUnsigned
            | backend_frontend_clang::legacy::BuiltinClass::WideCharSignednessUnavailable => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = match builtin {
                    backend_frontend_clang::legacy::BuiltinClass::PlainCharSigned => {
                        SHAPE_C_PLAIN_SIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::PlainCharUnsigned => {
                        SHAPE_C_PLAIN_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::SignedChar => SHAPE_C_SIGNED_CHAR,
                    backend_frontend_clang::legacy::BuiltinClass::UnsignedChar => {
                        SHAPE_C_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Utf16CodeUnit => {
                        SHAPE_UTF16_CODE_UNIT
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Utf32CodeUnit => {
                        SHAPE_UTF32_CODE_UNIT
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharSigned => {
                        SHAPE_C_WIDE_SIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharUnsigned => {
                        SHAPE_C_WIDE_UNSIGNED_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::WideCharSignednessUnavailable => {
                        SHAPE_C_WIDE_CHAR
                    }
                    backend_frontend_clang::legacy::BuiltinClass::Void
                    | backend_frontend_clang::legacy::BuiltinClass::Bool
                    | backend_frontend_clang::legacy::BuiltinClass::Integer { .. }
                    | backend_frontend_clang::legacy::BuiltinClass::Float
                    | backend_frontend_clang::legacy::BuiltinClass::Other => {
                        unreachable!("character class matched above")
                    }
                };
                record.payload1 = width;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Integer { signed, rank } => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                match rank {
                    // `short`, `int`, and `__int128` have a width that no
                    // sibling C rank shares on any supported ABI, so the
                    // width/signedness cell already proves their exact
                    // identity.  `long` and `long long` do not: they share a
                    // width and signedness on LP64 (and `int`/`long` do on
                    // LLP64, which this spelling separates as well).  Those
                    // ranks carry their canonical C spelling, the exact fact
                    // the closed spelling-bearing builtin shape exists to own,
                    // while the measured width stays in the free payload1 cell
                    // so the owned-IR conversion never assumes an LP64 ABI.
                    IntegerRank::Short | IntegerRank::Int | IntegerRank::Int128 => {
                        record.payload0 = SHAPE_INTEGER;
                        record.payload1 = (width << INTEGER_WIDTH_SHIFT)
                            | u32::from(signed) * INTEGER_SIGNED_FLAG;
                    }
                    IntegerRank::Long => {
                        record.payload0 = SHAPE_BUILTIN;
                        record.text = Some(if signed {
                            C_LONG_SPELLING
                        } else {
                            C_UNSIGNED_LONG_SPELLING
                        });
                        record.payload1 = width;
                    }
                    IntegerRank::LongLong => {
                        record.payload0 = SHAPE_BUILTIN;
                        record.text = Some(if signed {
                            C_LONG_LONG_SPELLING
                        } else {
                            C_UNSIGNED_LONG_LONG_SPELLING
                        });
                        record.payload1 = width;
                    }
                }
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Float => {
                let Some(width) = width(row.size_bits) else {
                    return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
                };
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = width;
                record
            }
            backend_frontend_clang::legacy::BuiltinClass::Other => {
                return Projected::leaf(unknown_record(TypeReason::OracleGap, None));
            }
        };
        Projected::leaf(record)
    }

    /// Projects one named row: a pushed declaration becomes its backward
    /// nominal row, a template parameter its `TypeVar` row, an anonymous
    /// record its member row, a resolved specialization its `Apply` row over
    /// the backward base and the projected argument rows, an out-of-root alias
    /// with a measured canonical builtin its underlying primitive row, and
    /// every other target the typed unresolved gap.
    fn project_named(
        &mut self,
        row: &TypeFact,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(identity) = row.declaration else {
            return Ok(gap());
        };
        if let Some(ordinal) = self.ordinal_of(identity) {
            let arguments: Vec<AuthorityTypeId> = self.edges_of(type_id).collect();
            if arguments.is_empty() {
                return Ok(Projected::leaf(nominal_record(ordinal)));
            }
            // A specialization: `Apply` over the backward base nominal and
            // every projected template-argument row.
            let mut children = vec![(ordinal, None)];
            for argument in arguments {
                match self.project_child(argument, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => children.push((coordinate, None)),
                }
            }
            return self.finish_row(SemanticTypeRecord::leaf(SemanticTypeTag::Apply), children);
        }
        if let Some(spelling) = self
            .template_parameters
            .iter()
            .find(|(known, _)| *known == identity)
            .map(|(_, spelling)| *spelling)
        {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            return Ok(Projected::leaf(record));
        }
        if let Some(parameter) = self.authority.declarations.iter().find(|declaration| {
            declaration.kind == DeclarationKind::TemplateParameter
                && declaration.identity == Some(identity)
        }) {
            if let Ok(spelling) = self.name_of(parameter) {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(spelling);
                return Ok(Projected::leaf(record));
            }
        }
        if let Some(anonymous_index) = self.anonymous_declaration(identity) {
            return self.project_anonymous_record(anonymous_index, depth);
        }
        // An out-of-root named type has no local declaration row, but an alias
        // that canonicalizes to a builtin keeps that closed underlying class
        // and measured width. Projecting it keeps two overloads on distinct
        // typedefs (`uint16_t` versus `uint32_t`) structurally distinct instead
        // of collapsing every unresolved name to one shared unknown row.
        if row.builtin.is_some() {
            return Ok(self.project_builtin(row));
        }
        // An out-of-root named type has no local declaration row, but libclang
        // still proves its USR. Emitting a declaration-identified external
        // nominal keeps two overloads on genuinely distinct out-of-root types
        // structurally distinct instead of collapsing both to one unknown row.
        // An identity that does have an authority declaration row but has not
        // been projected yet keeps the existing typed gap rather than becoming
        // a fabricated external target.
        if self
            .authority
            .declarations
            .iter()
            .any(|declaration| declaration.identity == Some(identity))
        {
            return Ok(gap());
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::Stable(StableRef {
            fragment: ExternalFragmentId::from_canonical_bytes(SYSTEM_FRAGMENT_DOMAIN),
            declaration: DeclarationIdentity {
                family: DeclarationFamilyId::from_canonical_bytes(&identity.bytes),
                variant: VariantFingerprint::from_canonical_bytes(&identity.bytes),
            },
        }));
        Ok(Projected::leaf(record))
    }

    /// Projects one C-family pointer row. The common wrapper around this
    /// record owns direct pointer cv qualifiers; the child independently
    /// retains any pointee qualifier.
    fn project_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(pointee) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_C_POINTER;
        let child = match self.project_child(pointee, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects an Objective-C block pointer without collapsing it into the
    /// ordinary C pointer form. The later declarator dialect receives one
    /// exact structural node and owns `^` placement.
    fn project_block_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(pointee) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let child = match self.project_child(pointee, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_C_BLOCK_POINTER;
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects a C++ reference category over its referent. This path never
    /// converts `T&&` into the Rust borrow mutability bit.
    fn project_reference(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(referent) = self.edge_of(row.id, TypeRelation::Referent) else {
            return Ok(gap());
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = if row.kind == TypeKind::RvalueReference {
            SHAPE_CXX_RVALUE_REFERENCE
        } else {
            SHAPE_CXX_LVALUE_REFERENCE
        };
        let child = match self.project_child(referent, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects a C++ member pointer with ordered owner then member type.
    /// Missing either authority edge is an exact projection gap; a member
    /// pointer must never silently become an ordinary raw pointer.
    ///
    /// The owner's class-ness is checked against what the authority actually
    /// proved, because C++ admits a member pointer only into a complete class
    /// type and libclang guarantees the class operand of a
    /// `CXType_MemberPointer` is that class. An owner whose USR carries an
    /// authority declaration row of record kind is the in-root class. An
    /// owner whose USR has no declaration row is a class defined outside
    /// the walked translation unit — `fmt`'s
    /// `int (testing::TestSuite::*)() const` parameter is the
    /// measured case: the class lives in the included `gtest.h`, whose
    /// declarations are not children of the walked translation unit, while
    /// the type plane still carries its USR. The shared build admits only a
    /// local record nominal as a member-pointer owner, so that form has no
    /// representable owner row and the whole member pointer keeps the lane's
    /// typed projection gap instead of a fabricated shape or a whole-source
    /// terminal. Every other owner kind stays the illegal-owner
    /// terminal the original guard existed for.
    fn project_member_pointer(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(owner) = self.edge_of(row.id, TypeRelation::MemberOwner) else {
            return Ok(gap());
        };
        let Some(member) = self.edge_of(row.id, TypeRelation::Pointee) else {
            return Ok(gap());
        };
        let Some(owner_row) = self.type_row(owner) else {
            return Ok(gap());
        };
        let declared_record = owner_row.declaration.is_some_and(|identity| {
            self.authority.declarations.iter().any(|declaration| {
                declaration.identity == Some(identity)
                    && declaration.kind == DeclarationKind::Record
            })
        });
        // A USR that has no authority declaration row at all is a class
        // defined outside the walked translation unit; one that has a row of
        // another kind names a non-class entity in-root and stays illegal.
        let out_of_root_class = owner_row.declaration.is_some_and(|identity| {
            !self
                .authority
                .declarations
                .iter()
                .any(|declaration| declaration.identity == Some(identity))
        });
        if owner_row.kind != TypeKind::Named {
            return Err(terminal(ProjectionFault::IllegalMemberPointerOwner {
                pointer: row.id,
                owner,
                kind: owner_row.kind,
                declaration: owner_row.declaration,
            }));
        }
        if !declared_record {
            if !out_of_root_class {
                return Err(terminal(ProjectionFault::IllegalMemberPointerOwner {
                    pointer: row.id,
                    owner,
                    kind: owner_row.kind,
                    declaration: owner_row.declaration,
                }));
            }
            return Ok(gap());
        }
        let owner = match self.project_child(owner, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let member = match self.project_child(member, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_CXX_MEMBER_POINTER;
        self.finish_row(record, vec![(owner, None), (member, None)])
    }

    /// Projects one array row with a typed fixed extent or an explicit
    /// incomplete/dependent extent. No sentinel length shares either state.
    fn project_array(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(element) = self.edge_of(row.id, TypeRelation::Element) else {
            return Ok(gap());
        };
        let record = match row.array_len {
            Some(length) => {
                let bytes = length.to_le_bytes();
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayFixed);
                record.payload0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record.payload1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                record
            }
            None => SemanticTypeRecord::leaf(SemanticTypeTag::ArrayIncomplete),
        };
        let child = match self.project_child(element, depth)? {
            UNHOSTABLE => return Ok(gap()),
            coordinate => coordinate,
        };
        self.finish_row(record, vec![(child, None)])
    }

    /// Projects one function type: the structural `FunctionPointer` row over
    /// its projected parameter rows and its non-void result row, the result
    /// marked in the reserved payload cell.
    fn project_function_type(
        &mut self,
        row: &TypeFact,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let mut children: Vec<(u32, Option<&'source [u8]>)> = Vec::new();
        let edges: Vec<AuthorityTypeId> = self.edges_of(row.id).collect();
        for edge in edges {
            if self.edge_relation(row.id, edge) == Some(TypeRelation::Parameter) {
                match self.project_child(edge, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => children.push((coordinate, None)),
                }
            }
        }
        let mut has_result = false;
        if let Some(result) = self.edge_of(row.id, TypeRelation::Result) {
            let is_void = self.type_row(result).is_some_and(|result| result.is_void());
            if !is_void {
                match self.project_child(result, depth)? {
                    UNHOSTABLE => return Ok(gap()),
                    coordinate => {
                        children.push((coordinate, None));
                        has_result = true;
                    }
                }
            }
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if row.is_variadic {
            record.payload0 = SemanticTypeRecord::FUNCTION_C_VARIADIC_FLAG;
        }
        if has_result {
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        self.finish_row(record, children)
    }

    /// The relation of one direct edge, for ordered signature projections.
    fn edge_relation(
        &self,
        source: AuthorityTypeId,
        target: AuthorityTypeId,
    ) -> Option<TypeRelation> {
        let begin = self.edge_starts.get(source.raw as usize).copied()?;
        let end = self.edge_starts.get(source.raw as usize + 1).copied()?;
        self.edge_order
            .get(begin..end)?
            .iter()
            .filter_map(|position| self.authority.type_edges.get(*position))
            .find(|edge| edge.source == source && edge.target == target)
            .map(|edge| edge.relation)
    }

    /// Projects one type row for a nested position: an already-namable row
    /// keeps its record, every other row interns as a pooled anonymous row
    /// owned by the lane's reserved next fact and returns its coordinate.
    fn project_child(
        &mut self,
        type_id: AuthorityTypeId,
        depth: usize,
    ) -> Result<u32, ClangCollectError> {
        let projected = self.project_type(type_id, depth)?;
        if let Some(NominalRef::Local(entity)) = projected.record.nominal {
            return Ok(entity.raw);
        }
        self.intern_row(projected)
    }

    /// Interns one projected row into the pooled anonymous lane, appending
    /// its children before the parent row so the pooled lane stays
    /// topologically backward. Every admission fault crosses the language
    /// boundary intact rather than being collapsed into an unhostable row.
    fn intern_row(&mut self, projected: Projected<'source>) -> Result<u32, ClangCollectError> {
        let anchor = self.anchor().map_err(terminal)?;
        for position in 0..projected.child_count {
            let target = projected.children.get(position).copied().unwrap_or(0);
            let name = projected.child_names.get(position).copied().flatten();
            self.facts
                .anonymous_type_child(target, name, 0)
                .map_err(|fault| lane_terminal(self.facts, name.map_or(0, <[u8]>::len), fault))?;
        }
        self.facts
            .intern_reserved_anchor_type_row(anchor, projected.record)
            .map_err(|fault| lane_terminal(self.facts, 0, fault))
    }

    /// Finishes one compound row over its ordered children. A row wider than
    /// the type-child lane is a typed capacity fault, never an oracle gap
    /// and never a partial row.
    fn finish_row(
        &self,
        record: SemanticTypeRecord<'source>,
        children: Vec<(u32, Option<&'source [u8]>)>,
    ) -> Result<Projected<'source>, ClangCollectError> {
        if children.len() > MAX_TYPE_CHILDREN {
            return Err(lane_terminal(self.facts, 0, FactFault::TypeChildCapacity));
        }
        let mut projected = Projected::leaf(record);
        for (target, name) in children {
            if projected.child(target, name).is_none() {
                return Err(lane_terminal(
                    self.facts,
                    name.map_or(0, <[u8]>::len),
                    FactFault::TypeChildCapacity,
                ));
            }
        }
        Ok(projected)
    }

    /// Projects one anonymous record: its named member fields are pushed as
    /// facts on demand (so the row's children stay backward), then the
    /// `AnonymousRecord(Struct)` row is returned over those field ordinals.
    /// Reused anonymous records intern their nested row once.
    fn project_anonymous_record(
        &mut self,
        declaration_index: usize,
        depth: usize,
    ) -> Result<Projected<'source>, ClangCollectError> {
        let Some(record) = self.declaration(declaration_index) else {
            return Ok(Projected::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        if depth == 0 {
            return Ok(Projected::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        let declarations = self.authority.declarations;
        let mut members: Vec<usize> = Vec::new();
        for candidate in 0..declarations.len() {
            if self.representative.get(candidate).copied().flatten() != Some(candidate) {
                continue;
            }
            let Some(member) = declarations.get(candidate) else {
                continue;
            };
            if member.kind != DeclarationKind::Field {
                continue;
            }
            if member.owner != record.identity || member.owner.is_none() {
                continue;
            }
            if !span_contains(record.span, member.span) {
                continue;
            }
            members.push(candidate);
        }
        members.sort_by_key(|candidate| {
            declarations
                .get(*candidate)
                .map(|member| member.span.start)
                .unwrap_or(u32::MAX)
        });
        // Push every not-yet-pushed member field so the row's children stay
        // strictly backward, then build the member row over their ordinals.
        for candidate in &members {
            if self.ordinals.get(*candidate).copied().flatten().is_some() {
                continue;
            }
            self.push_typed_member(*candidate, EntityKind::Field)?;
        }
        let mut projected = Projected::leaf({
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
            record.payload0 = ANON_RECORD_STRUCT;
            record
        });
        for candidate in &members {
            let Some(member) = declarations.get(*candidate) else {
                continue;
            };
            let Some(ordinal) = self.ordinals.get(*candidate).copied().flatten() else {
                continue;
            };
            let Ok(member_name) = self.name_of(member) else {
                continue;
            };
            if projected.child(ordinal, Some(member_name)).is_none() {
                return Err(lane_terminal(
                    self.facts,
                    member_name.len(),
                    FactFault::TypeChildCapacity,
                ));
            }
        }
        Ok(projected)
    }
}
