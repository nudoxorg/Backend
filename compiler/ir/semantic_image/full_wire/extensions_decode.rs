//! Closed decoding for the seven sparse extension fact grammars.
//!
//! These rows are deliberately decoded from canonical coordinates rather than
//! replaying NXLE/native storage bytes.  Every reference is checked against
//! the complete full-image plan before a borrowed reader may return a fact.

use crate::{
    AtomId, AtomListId, CSharpFacts, CSharpMemberEffects, CSharpNullability,
    CSharpPartialRole, CSharpReferenceKind, ClangFacts, ClangLayout,
    ClangQualifiers, ClangStorageClass, Confidence, EntityListId, GoFacts,
    GoSignature, JavaFacts, PythonFacts, PythonParameterKind, RustFacts,
    RustOwnership, SourceSpan, TypeId, TypeListId, TypeParameterListId,
    TypeScriptFacts,
};

use super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    validate::TypedLayout,
};

#[derive(Clone, Copy)]
pub(crate) struct ExtensionCounts {
    pub(crate) atoms: u32,
    pub(crate) members: u32,
    pub(crate) typed: TypedLayout,
}

/// Validates every fact payload in its named plane. Sparse binding/profile
/// authority is owned by `validate`; this module owns only each plane's
/// exact field grammar and cross-pool coordinates.
pub(crate) fn validate(
    tag: u8,
    value: &[u8],
    row: u32,
    counts: ExtensionCounts,
) -> Result<(), FullSemanticImageFault> {
    match tag {
        0 => { let _ = typescript(value, row, counts)?; }
        1 => { let _ = csharp(value, row, counts)?; }
        2 => { let _ = go(value, row, counts)?; }
        3 => { let _ = rust(value, row, counts)?; }
        4 => { let _ = python(value, row, counts)?; }
        5 => { let _ = java(value, row, counts)?; }
        6 => { let _ = clang(value, row, counts)?; }
        observed => return Err(FullSemanticImageFault::Discriminant {
            field: FullSemanticImageField::ExtensionFacts, row, observed,
        }),
    }
    Ok(())
}

pub(crate) fn typescript(
    value: &[u8], row: u32, counts: ExtensionCounts,
) -> Result<TypeScriptFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 0)?;
    let type_parameters = TypeParameterListId::new(cursor.id(counts.typed.count(6), counts, 0)?);
    let declared = cursor.optional_type(counts, 1)?;
    let observed = cursor.optional_type(counts, 2)?;
    cursor.finish()?;
    Ok(TypeScriptFacts { type_parameters, declared, observed })
}

pub(crate) fn csharp(
    value: &[u8], row: u32, counts: ExtensionCounts,
) -> Result<CSharpFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 1)?;
    let nullability = match cursor.byte()? {
        0 => CSharpNullability::Oblivious,
        1 => CSharpNullability::NonNullable,
        2 => CSharpNullability::Nullable,
        observed => return Err(cursor.discriminant(observed)),
    };
    let reference_kind = match cursor.byte()? {
        0 => CSharpReferenceKind::Value,
        1 => CSharpReferenceKind::In,
        2 => CSharpReferenceKind::Ref,
        3 => CSharpReferenceKind::Out,
        observed => return Err(cursor.discriminant(observed)),
    };
    let constraints = TypeParameterListId::new(cursor.id(counts.typed.count(6), counts, 0)?);
    let effects = CSharpMemberEffects {
        is_async: cursor.boolean()?,
        is_iterator: cursor.boolean()?,
        is_extension: cursor.boolean()?,
    };
    let attributes = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let partial = match cursor.byte()? {
        0 => CSharpPartialRole::None,
        1 => CSharpPartialRole::Definition,
        2 => CSharpPartialRole::Implementation,
        observed => return Err(cursor.discriminant(observed)),
    };
    let xml_provenance = cursor.optional_source(counts)?;
    cursor.finish()?;
    Ok(CSharpFacts { nullability, reference_kind, constraints, effects, attributes, partial, xml_provenance })
}

pub(crate) fn go(value: &[u8], row: u32, counts: ExtensionCounts) -> Result<GoFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 2)?;
    let signature = GoSignature {
        parameters: TypeListId::new(cursor.id(counts.typed.count(1), counts, 0)?),
        results: TypeListId::new(cursor.id(counts.typed.count(1), counts, 0)?),
        variadic: cursor.boolean()?,
    };
    let type_parameters = TypeParameterListId::new(cursor.id(counts.typed.count(6), counts, 0)?);
    let fields = EntityListId::new(cursor.id(Some(counts.members), counts, 0)?);
    let method_set = EntityListId::new(cursor.id(Some(counts.members), counts, 0)?);
    let build_constraints = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let constant_value = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let constant_group = i64::from_le_bytes(cursor.array::<8>()?);
    let constant_flags = cursor.u32()?;
    cursor.finish()?;
    Ok(GoFacts { signature, type_parameters, fields, method_set, build_constraints, constant_value, constant_group, constant_flags })
}

pub(crate) fn rust(value: &[u8], row: u32, counts: ExtensionCounts) -> Result<RustFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 3)?;
    let ownership = match cursor.byte()? {
        0 => RustOwnership::Value,
        1 => RustOwnership::SharedBorrow,
        2 => RustOwnership::MutableBorrow,
        3 => RustOwnership::Moved,
        observed => return Err(cursor.discriminant(observed)),
    };
    let lifetimes = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let where_clauses = TypeParameterListId::new(cursor.id(counts.typed.count(6), counts, 0)?);
    let macros = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    cursor.finish()?;
    Ok(RustFacts { ownership, lifetimes, where_clauses, macros })
}

pub(crate) fn python(value: &[u8], row: u32, counts: ExtensionCounts) -> Result<PythonFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 4)?;
    let decorators = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let parameter_kind = match cursor.byte()? {
        0 => PythonParameterKind::PositionalOnly,
        1 => PythonParameterKind::PositionalOrKeyword,
        2 => PythonParameterKind::VariadicPositional,
        3 => PythonParameterKind::KeywordOnly,
        4 => PythonParameterKind::VariadicKeyword,
        observed => return Err(cursor.discriminant(observed)),
    };
    let dynamic_confidence = confidence(cursor.byte()?, row)?;
    cursor.finish()?;
    Ok(PythonFacts { decorators, parameter_kind, dynamic_confidence })
}

pub(crate) fn java(value: &[u8], row: u32, counts: ExtensionCounts) -> Result<JavaFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 5)?;
    let throws = TypeListId::new(cursor.id(counts.typed.count(1), counts, 0)?);
    let annotations = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    let overloads = EntityListId::new(cursor.id(Some(counts.members), counts, 0)?);
    let record_components = EntityListId::new(cursor.id(Some(counts.members), counts, 0)?);
    cursor.finish()?;
    Ok(JavaFacts { throws, annotations, overloads, record_components })
}

pub(crate) fn clang(value: &[u8], row: u32, counts: ExtensionCounts) -> Result<ClangFacts, FullSemanticImageFault> {
    let mut cursor = Cursor::new(value, row, 6)?;
    let qualifiers = ClangQualifiers {
        is_const: cursor.boolean()?, is_volatile: cursor.boolean()?, is_restrict: cursor.boolean()?,
    };
    let storage = match cursor.byte()? {
        0 => ClangStorageClass::None,
        1 => ClangStorageClass::Auto,
        2 => ClangStorageClass::Static,
        3 => ClangStorageClass::Extern,
        4 => ClangStorageClass::Register,
        5 => ClangStorageClass::ThreadLocal,
        observed => return Err(cursor.discriminant(observed)),
    };
    let layout = ClangLayout { size_bits: cursor.optional_u32()?, align_bits: cursor.optional_u32()? };
    let templates = TypeParameterListId::new(cursor.id(counts.typed.count(6), counts, 0)?);
    let includes = AtomListId::new(cursor.id(counts.typed.count(5), counts, 0)?);
    cursor.finish()?;
    Ok(ClangFacts { qualifiers, storage, layout, templates, includes })
}

struct Cursor<'a> {
    value: &'a [u8],
    cursor: usize,
    row: u32,
}

impl<'a> Cursor<'a> {
    fn new(value: &'a [u8], row: u32, expected_tag: u8) -> Result<Self, FullSemanticImageFault> {
        let observed = *value.first().ok_or(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts, row, expected: 1, observed: 0,
        })?;
        if observed != expected_tag {
            return Err(FullSemanticImageFault::Discriminant {
                field: FullSemanticImageField::ExtensionFacts, row, observed,
            });
        }
        Ok(Self { value, cursor: 1, row })
    }
    fn byte(&mut self) -> Result<u8, FullSemanticImageFault> {
        let value = *self.value.get(self.cursor).ok_or(FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::ExtensionFacts, offset: self.cursor,
        })?;
        self.cursor = self.cursor.checked_add(1).ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], FullSemanticImageFault> {
        let end = self.cursor.checked_add(N).ok_or(FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?;
        let value = self.value.get(self.cursor..end).ok_or(FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::ExtensionFacts, offset: self.cursor,
        })?;
        self.cursor = end;
        <[u8; N]>::try_from(value).map_err(|_| FullSemanticImageFault::Truncated {
            field: FullSemanticImageField::ExtensionFacts, offset: self.cursor,
        })
    }
    fn u32(&mut self) -> Result<u32, FullSemanticImageFault> { Ok(u32::from_le_bytes(self.array::<4>()?)) }
    fn boolean(&mut self) -> Result<bool, FullSemanticImageFault> {
        match self.byte()? { 0 => Ok(false), 1 => Ok(true), observed => Err(self.discriminant(observed)) }
    }
    fn id(&mut self, count: Option<u32>, _: ExtensionCounts, _: u8) -> Result<u32, FullSemanticImageFault> {
        let raw = self.u32()?;
        let expected = count.ok_or(FullSemanticImageFault::TypedDomain { node: self.row, domain: 0 })?;
        if raw >= expected {
            return Err(FullSemanticImageFault::Reference {
                field: FullSemanticImageField::ExtensionFacts, row: self.row, expected, observed: raw,
            });
        }
        Ok(raw)
    }
    fn optional_type(&mut self, counts: ExtensionCounts, _: u8) -> Result<Option<TypeId>, FullSemanticImageFault> {
        let present = self.boolean()?;
        let raw = self.u32()?;
        if !present {
            if raw != 0 { return Err(FullSemanticImageFault::Reserved { field: FullSemanticImageField::ExtensionFacts, row: self.row, observed: raw.to_le_bytes()[0] }); }
            return Ok(None);
        }
        let expected = counts.typed.count(0).ok_or(FullSemanticImageFault::TypedDomain { node: self.row, domain: 0 })?;
        if raw >= expected { return Err(FullSemanticImageFault::Reference { field: FullSemanticImageField::ExtensionFacts, row: self.row, expected, observed: raw }); }
        Ok(Some(TypeId::new(raw)))
    }
    fn optional_source(&mut self, counts: ExtensionCounts) -> Result<Option<SourceSpan>, FullSemanticImageFault> {
        let present = self.boolean()?;
        let file = self.u32()?;
        let start = self.u32()?;
        let end = self.u32()?;
        if !present {
            if file != 0 || start != 0 || end != 0 { return Err(FullSemanticImageFault::Reserved { field: FullSemanticImageField::ExtensionFacts, row: self.row, observed: file.to_le_bytes()[0] }); }
            return Ok(None);
        }
        if file >= counts.atoms { return Err(FullSemanticImageFault::Reference { field: FullSemanticImageField::ExtensionFacts, row: self.row, expected: counts.atoms, observed: file }); }
        SourceSpan::new(AtomId::new(file), start, end).ok_or(FullSemanticImageFault::SourceSpan { row: self.row, start, end }).map(Some)
    }
    fn optional_u32(&mut self) -> Result<Option<u32>, FullSemanticImageFault> {
        let present = self.boolean()?;
        let value = self.u32()?;
        if present { Ok(Some(value)) } else if value == 0 { Ok(None) } else { Err(FullSemanticImageFault::Reserved { field: FullSemanticImageField::ExtensionFacts, row: self.row, observed: value.to_le_bytes()[0] }) }
    }
    fn finish(&self) -> Result<(), FullSemanticImageFault> {
        if self.cursor == self.value.len() { Ok(()) } else { Err(FullSemanticImageFault::Reference {
            field: FullSemanticImageField::ExtensionFacts, row: self.row,
            expected: u32::try_from(self.cursor).map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?,
            observed: u32::try_from(self.value.len()).map_err(|_| FullSemanticImageFault::LengthOverflow { field: FullSemanticImageField::ExtensionFacts })?,
        }) }
    }
    const fn discriminant(&self, observed: u8) -> FullSemanticImageFault {
        FullSemanticImageFault::Discriminant { field: FullSemanticImageField::ExtensionFacts, row: self.row, observed }
    }
}

fn confidence(value: u8, row: u32) -> Result<Confidence, FullSemanticImageFault> {
    match value {
        0 => Ok(Confidence::Syntactic), 1 => Ok(Confidence::Heuristic), 2 => Ok(Confidence::Indexed),
        3 => Ok(Confidence::Imported), 4 => Ok(Confidence::Compiler),
        observed => Err(FullSemanticImageFault::Discriminant { field: FullSemanticImageField::ExtensionFacts, row, observed }),
    }
}
