//! Admission of one semantic type record.
//!
//! The tag vocabulary names every legal cell. This module rejects a
//! tag-foreign cell, a missing required cell, or a child shape the tag does
//! not define.

use super::{
    AnnotationKind, AnonRecordForm, CellLaw, ChannelDirection, ChildCountLaw, CvQualifiers,
    FunctionVariadicForm, MappedModifier, NominalRef, PrimitiveShape, SemanticTypeChild,
    SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag, TypeCell, TypeChildTarget, TypeReason,
    TypeWidth, Variance,
};
use crate::ir_vocabulary::ListSpan;

impl SemanticTypeRecord<'_> {
    /// A leaf row with no children and no cells.
    #[must_use]
    pub const fn leaf(tag: SemanticTypeTag) -> Self {
        Self {
            tag,
            payload0: 0,
            payload1: 0,
            text: None,
            text2: None,
            nominal: None,
            children: ListSpan::new(0, 0),
        }
    }

    /// Exact trailing result count for a one-result function-pointer row.
    ///
    /// The child sequence is `[parameters..., results...]`; zero therefore
    /// denotes a no-result callable, and every positive payload1 value is an
    /// unambiguous trailing result count.
    pub const FUNCTION_RESULT_COUNT_ONE: u32 = 1;
    /// Schema-2 result-presence bit accepted only when reopening historical
    /// fragments. New writers must use [`Self::FUNCTION_RESULT_COUNT_ONE`] or
    /// a larger direct count for multi-result signatures.
    pub const LEGACY_RESULT_FLAG: u32 = 1 << 31;
    /// Function-pointer payload0: exactly the final parameter is a typed
    /// variadic/rest element (Go, TypeScript, Python).
    pub const FUNCTION_TYPED_VARIADIC_FLAG: u32 = 1;
    /// Function-pointer payload0: an unnamed C-family `...` follows the
    /// parameter range. It is distinct from a typed rest parameter.
    pub const FUNCTION_C_VARIADIC_FLAG: u32 = 1 << 1;
    /// Function-pointer payload0 mask for [`FunctionVariadicForm`].
    pub const FUNCTION_VARIADIC_MASK: u32 =
        Self::FUNCTION_TYPED_VARIADIC_FLAG | Self::FUNCTION_C_VARIADIC_FLAG;
    /// Function-pointer payload0: the callable is unsafe.
    pub const FUNCTION_UNSAFE_FLAG: u32 = 1 << 2;
    /// All closed function-pointer payload0 modifier bits.
    pub const FUNCTION_FLAGS: u32 = Self::FUNCTION_VARIADIC_MASK | Self::FUNCTION_UNSAFE_FLAG;

    /// Decodes the closed staged variadic form, rejecting the unused mask
    /// state `3` instead of treating a mixed typed/C tail as two tails.
    #[must_use]
    pub const fn function_variadic_form(&self) -> Option<FunctionVariadicForm> {
        match self.payload0 & Self::FUNCTION_VARIADIC_MASK {
            0 => Some(FunctionVariadicForm::None),
            Self::FUNCTION_TYPED_VARIADIC_FLAG => Some(FunctionVariadicForm::TypedLast),
            Self::FUNCTION_C_VARIADIC_FLAG => Some(FunctionVariadicForm::CUnbounded),
            _ => None,
        }
    }

    /// Decodes the exact result count carried by one function row. The old
    /// high-bit presence form remains reopenable as one result, while any
    /// other high-bit value is a malformed legacy payload.
    #[must_use]
    pub const fn function_result_count(&self) -> Option<u32> {
        if self.payload1 == Self::LEGACY_RESULT_FLAG {
            Some(1)
        } else if self.payload1 & Self::LEGACY_RESULT_FLAG == 0 {
            Some(self.payload1)
        } else {
            None
        }
    }

    /// The integer signedness bit (payload1 bit 0); the width cell occupies
    /// the bits above it.
    pub const INTEGER_SIGNED_FLAG: u32 = 1;
    /// Bit offset of the integer width cell above the signedness bit.
    const INTEGER_WIDTH_SHIFT: u32 = 1;

    /// The closed child-count law of one tag.
    #[must_use]
    pub const fn child_law(tag: SemanticTypeTag) -> ChildCountLaw {
        match tag {
            SemanticTypeTag::Slice
            | SemanticTypeTag::Array
            | SemanticTypeTag::Annotated
            | SemanticTypeTag::Channel
            | SemanticTypeTag::ArraySequence
            | SemanticTypeTag::ArrayRectangular
            | SemanticTypeTag::ArrayFixed
            | SemanticTypeTag::ArrayConstExpression
            | SemanticTypeTag::ArrayIncomplete
            | SemanticTypeTag::CQualified => ChildCountLaw { min: 1, max: 1 },
            SemanticTypeTag::Map => ChildCountLaw { min: 2, max: 2 },
            SemanticTypeTag::Conditional => ChildCountLaw { min: 4, max: 4 },
            // constraint, optional key-remap (`as`), value
            SemanticTypeTag::Mapped => ChildCountLaw { min: 2, max: 3 },
            SemanticTypeTag::Apply => ChildCountLaw {
                min: 1,
                max: u32::MAX,
            },
            SemanticTypeTag::QualifiedPath => ChildCountLaw { min: 1, max: 2 },
            SemanticTypeTag::Wildcard => ChildCountLaw { min: 0, max: 1 },
            SemanticTypeTag::Tuple
            | SemanticTypeTag::Union
            | SemanticTypeTag::Intersection
            | SemanticTypeTag::FunctionPointer
            | SemanticTypeTag::TemplateLiteral
            | SemanticTypeTag::AnonymousRecord
            | SemanticTypeTag::ImplTrait
            | SemanticTypeTag::DynTrait => ChildCountLaw {
                min: 0,
                max: u32::MAX,
            },
            SemanticTypeTag::SelfType
            | SemanticTypeTag::Never
            | SemanticTypeTag::Any
            | SemanticTypeTag::Unknown
            | SemanticTypeTag::Nominal
            | SemanticTypeTag::TypeVar
            | SemanticTypeTag::Inferred => ChildCountLaw { min: 0, max: 0 },
            // The primitive tag-level law stays permissive: pointer and
            // reference shapes own exactly one child, every other shape
            // owns none, and `validate_primitive` proves the exact law
            // from the shape cell.
            SemanticTypeTag::Primitive => ChildCountLaw {
                min: 0,
                max: u32::MAX,
            },
        }
    }

    /// Proves one record's cells against its tag: payload ownership, text
    /// presence, nominal presence, and the child-count law across
    /// `child_count` pooled positions. Per-child name/flags/text laws are
    /// proven by [`SemanticTypeRecord::validate_child`].
    pub fn validate(&self, child_count: u32) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        let law = Self::child_law(tag);
        if child_count < law.min || child_count > law.max {
            return Err(SemanticTypeFault::ChildCount {
                tag,
                law,
                actual: child_count,
            });
        }
        match tag {
            SemanticTypeTag::Never
            | SemanticTypeTag::Any
            | SemanticTypeTag::Tuple
            | SemanticTypeTag::Slice
            | SemanticTypeTag::Union
            | SemanticTypeTag::Intersection
            | SemanticTypeTag::ImplTrait
            | SemanticTypeTag::DynTrait
            | SemanticTypeTag::TemplateLiteral => {
                self.require_no_cells()?;
            }
            // `Self` needs no spelling, but TypeScript's distinct `this`
            // type must survive the common row so language policy can render
            // it without pretending it is Rust `Self`.
            SemanticTypeTag::SelfType => {
                if self.payload0 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                if self.payload1 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
                self.check_cell(TypeCell::Text, CellLaw::Optional, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Inferred => {
                self.require_zero_payloads()?;
                self.check_cell(TypeCell::Text, CellLaw::Optional, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Primitive => self.validate_primitive(child_count)?,
            SemanticTypeTag::Unknown => {
                let reason = TypeReason::try_from(self.payload0).map_err(|error| {
                    SemanticTypeFault::Reason {
                        actual: error.actual,
                    }
                })?;
                let text_law = if reason.carries_spelling() {
                    CellLaw::Required
                } else {
                    CellLaw::Forbidden
                };
                self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
                self.require_zero_payload1()?;
            }
            SemanticTypeTag::Nominal => {
                // Local nominals are resolved solely by their typed entity
                // coordinate.  Foreign nominals additionally carry the
                // authority-provided module-qualified display/path spelling;
                // legacy fragments without it remain reopenable but cannot
                // be rendered as an invented `foreign` name.
                let text_law = match self.nominal {
                    Some(NominalRef::External(_)) => CellLaw::Optional,
                    Some(NominalRef::Local(_)) | Some(NominalRef::Stable(_)) | None => {
                        CellLaw::Forbidden
                    }
                };
                self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(TypeCell::Nominal, CellLaw::Required, self.nominal.is_some())?;
                self.require_zero_payloads()?;
            }
            SemanticTypeTag::TypeVar => {
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
                self.require_zero_payloads()?;
            }
            SemanticTypeTag::Array => {
                // Schema-2 compatibility only. Fresh producers use one of
                // the closed Array* tags below, so a renderer never has to
                // infer extent semantics from a shared `[]` spelling.
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::ArraySequence | SemanticTypeTag::ArrayIncomplete => {
                self.require_no_cells()?;
            }
            SemanticTypeTag::CQualified => {
                CvQualifiers::try_from(self.payload0).map_err(|error| {
                    SemanticTypeFault::CvQualifiers {
                        actual: error.actual,
                    }
                })?;
                if self.payload0 == 0 {
                    return Err(SemanticTypeFault::CvQualifiers {
                        actual: self.payload0,
                    });
                }
                self.require_zero_payload1()?;
                self.require_no_text()?;
            }
            SemanticTypeTag::ArrayRectangular => {
                if self.payload0 == 0 || self.payload0 > u32::from(u16::MAX) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_zero_payload1()?;
                self.require_no_text()?;
            }
            SemanticTypeTag::ArrayFixed => {
                self.require_no_text()?;
            }
            SemanticTypeTag::ArrayConstExpression => {
                self.require_zero_payloads()?;
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::QualifiedPath => {
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
                self.require_zero_payloads()?;
            }
            SemanticTypeTag::Wildcard => {
                if self.payload0 > u32::from(Variance::Contravariant) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_no_text()?;
                self.require_zero_payload1()?;
                let expected_children = match self.payload0 {
                    value if value == u32::from(Variance::Invariant) => 0,
                    value
                        if value == u32::from(Variance::Covariant)
                            || value == u32::from(Variance::Contravariant) =>
                    {
                        1
                    }
                    _ => unreachable!("variance cell was bounded above"),
                };
                if child_count != expected_children {
                    return Err(SemanticTypeFault::ChildCount {
                        tag,
                        law: ChildCountLaw {
                            min: expected_children,
                            max: expected_children,
                        },
                        actual: child_count,
                    });
                }
            }
            SemanticTypeTag::FunctionPointer => {
                let Some(results) = self.function_result_count() else {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                };
                if results > child_count {
                    return Err(SemanticTypeFault::ChildCount {
                        tag,
                        law: ChildCountLaw {
                            min: results,
                            max: u32::MAX,
                        },
                        actual: child_count,
                    });
                }
                if self.payload0 & !Self::FUNCTION_FLAGS != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                let Some(variadic) = self.function_variadic_form() else {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                };
                if variadic == FunctionVariadicForm::TypedLast {
                    let minimum_variadic_children =
                        results
                            .checked_add(1)
                            .ok_or(SemanticTypeFault::ReservedCell {
                                tag,
                                cell: TypeCell::Payload1,
                                actual: self.payload1,
                            })?;
                    if child_count < minimum_variadic_children {
                        return Err(SemanticTypeFault::ChildCount {
                            tag,
                            law: ChildCountLaw {
                                min: minimum_variadic_children,
                                max: u32::MAX,
                            },
                            actual: child_count,
                        });
                    }
                }
                self.check_cell(TypeCell::Text, CellLaw::Optional, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::Annotated => {
                if AnnotationKind::try_from(self.payload0).is_err() {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_zero_payload1()?;
                self.require_no_text()?;
            }
            SemanticTypeTag::Conditional | SemanticTypeTag::Apply => {
                self.require_no_cells()?;
            }
            SemanticTypeTag::Mapped => {
                if self.payload0 > u32::from(MappedModifier::Absent) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                if self.payload1 > u32::from(MappedModifier::Absent) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
                self.check_cell(TypeCell::Text, CellLaw::Required, self.text.is_some())?;
                self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
                self.check_cell(
                    TypeCell::Nominal,
                    CellLaw::Forbidden,
                    self.nominal.is_some(),
                )?;
            }
            SemanticTypeTag::AnonymousRecord => {
                if self.payload0 > u32::from(AnonRecordForm::Interface) {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_no_text()?;
            }
            SemanticTypeTag::Map => self.require_no_cells()?,
            SemanticTypeTag::Channel => {
                if ChannelDirection::try_from(self.payload0).is_err() {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    });
                }
                self.require_zero_payload1()?;
                self.require_no_text()?;
            }
        }
        Ok(())
    }

    /// Proves one pooled child against this row's tag at `position`.
    ///
    /// The tag owns every child fact: names only where the grammar demands
    /// them, modifiers only under their matching structural parents, and text
    /// targets only under template literals.
    pub fn validate_child(
        &self,
        position: u32,
        child: &SemanticTypeChild<'_>,
    ) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        // Template text is a distinct target kind, not an unnamed type
        // child.  In particular, an empty literal segment is meaningful in
        // `${T}` and must retain its empty byte spelling rather than being
        // rejected by the ordinary member-label grammar.
        if matches!(child.target, TypeChildTarget::Text) {
            if tag != SemanticTypeTag::TemplateLiteral || child.flags != 0 {
                return Err(if tag != SemanticTypeTag::TemplateLiteral {
                    SemanticTypeFault::ChildTextForbidden { tag, position }
                } else {
                    SemanticTypeFault::ChildFlagsForbidden {
                        tag,
                        position,
                        actual: child.flags,
                    }
                });
            }
            if child.name.is_none() {
                return Err(SemanticTypeFault::ChildNameRequired { tag, position });
            }
            return Ok(());
        }
        // A callable element may carry the label its source wrote (a Go
        // named result). It is optional: every other callable element takes
        // its label from the carrier fact it targets, and a result slot
        // without an explicit label is unlabelled.
        let name_allowed = matches!(
            tag,
            SemanticTypeTag::Tuple
                | SemanticTypeTag::AnonymousRecord
                | SemanticTypeTag::FunctionPointer
        );
        let name_required = tag == SemanticTypeTag::AnonymousRecord;
        if !name_allowed && child.name.is_some() {
            return Err(SemanticTypeFault::ChildNameForbidden { tag, position });
        }
        if name_required && child.name.is_none() {
            return Err(SemanticTypeFault::ChildNameRequired { tag, position });
        }
        let allowed_flags = match tag {
            SemanticTypeTag::AnonymousRecord => {
                SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_READONLY
            }
            SemanticTypeTag::Tuple | SemanticTypeTag::FunctionPointer => {
                SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST
            }
            _ => 0,
        };
        if child.flags & !allowed_flags != 0 {
            return Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual: child.flags,
            });
        }
        if child.flags & !SemanticTypeChild::FLAG_ALL != 0 {
            return Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual: child.flags,
            });
        }
        if child.flags & (SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST)
            == SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST
        {
            return Err(SemanticTypeFault::ChildFlagsForbidden {
                tag,
                position,
                actual: child.flags,
            });
        }
        Ok(())
    }

    /// Validates one child together with row-wide constraints that depend on
    /// the result range. Callers admitting a full row use this instead of
    /// [`Self::validate_child`] so a variadic marker cannot point at a result
    /// or an unmarked parameter.
    pub fn validate_child_in_row(
        &self,
        position: u32,
        child_count: u32,
        child: &SemanticTypeChild<'_>,
    ) -> Result<(), SemanticTypeFault> {
        self.validate_child(position, child)?;
        if self.tag == SemanticTypeTag::FunctionPointer
            && self.function_variadic_form() == Some(FunctionVariadicForm::TypedLast)
        {
            let results = self
                .function_result_count()
                .ok_or(SemanticTypeFault::ReservedCell {
                    tag: self.tag,
                    cell: TypeCell::Payload1,
                    actual: self.payload1,
                })?;
            let minimum_variadic_children =
                results
                    .checked_add(1)
                    .ok_or(SemanticTypeFault::ReservedCell {
                        tag: self.tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    })?;
            let parameters =
                child_count
                    .checked_sub(results)
                    .ok_or(SemanticTypeFault::ChildCount {
                        tag: self.tag,
                        law: ChildCountLaw {
                            min: minimum_variadic_children,
                            max: u32::MAX,
                        },
                        actual: child_count,
                    })?;
            let final_parameter =
                parameters
                    .checked_sub(1)
                    .ok_or(SemanticTypeFault::ChildCount {
                        tag: self.tag,
                        law: ChildCountLaw {
                            min: minimum_variadic_children,
                            max: u32::MAX,
                        },
                        actual: child_count,
                    })?;
            if position == final_parameter && child.flags & SemanticTypeChild::FLAG_REST == 0 {
                return Err(SemanticTypeFault::VariadicParameter {
                    position,
                    actual: child.flags,
                });
            }
            if position != final_parameter && child.flags & SemanticTypeChild::FLAG_REST != 0 {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
        }
        if self.tag == SemanticTypeTag::FunctionPointer {
            let variadic =
                self.function_variadic_form()
                    .ok_or(SemanticTypeFault::ReservedCell {
                        tag: self.tag,
                        cell: TypeCell::Payload0,
                        actual: self.payload0,
                    })?;
            let results = self
                .function_result_count()
                .ok_or(SemanticTypeFault::ReservedCell {
                    tag: self.tag,
                    cell: TypeCell::Payload1,
                    actual: self.payload1,
                })?;
            let parameters =
                child_count
                    .checked_sub(results)
                    .ok_or(SemanticTypeFault::ChildCount {
                        tag: self.tag,
                        law: ChildCountLaw {
                            min: results,
                            max: u32::MAX,
                        },
                        actual: child_count,
                    })?;
            if position >= parameters
                && child.flags & (SemanticTypeChild::FLAG_OPTIONAL | SemanticTypeChild::FLAG_REST)
                    != 0
            {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
            if variadic != FunctionVariadicForm::TypedLast
                && child.flags & SemanticTypeChild::FLAG_REST != 0
            {
                return Err(SemanticTypeFault::ChildFlagsForbidden {
                    tag: self.tag,
                    position,
                    actual: child.flags,
                });
            }
        }
        Ok(())
    }

    fn validate_primitive(&self, child_count: u32) -> Result<(), SemanticTypeFault> {
        let tag = self.tag;
        let shape = PrimitiveShape::try_from(self.payload0).map_err(|error| {
            SemanticTypeFault::PrimitiveShape {
                actual: error.actual,
            }
        })?;
        match shape {
            PrimitiveShape::Integer => {
                if TypeWidth::try_from_cell(self.payload1 >> Self::INTEGER_WIDTH_SHIFT).is_err() {
                    return Err(SemanticTypeFault::Width {
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Float => {
                if TypeWidth::try_from_cell(self.payload1).is_err() {
                    return Err(SemanticTypeFault::Width {
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Reference => {
                if self.payload1 & !Self::INTEGER_SIGNED_FLAG != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Bool
            | PrimitiveShape::LegacyChar
            | PrimitiveShape::Str
            | PrimitiveShape::MutPointer
            | PrimitiveShape::ConstPointer
            | PrimitiveShape::CPointer
            | PrimitiveShape::CxxLvalueReference
            | PrimitiveShape::CxxRvalueReference
            | PrimitiveShape::CxxMemberPointer
            | PrimitiveShape::ArbitraryInteger
            | PrimitiveShape::NativeSignedInteger
            | PrimitiveShape::NativeUnsignedInteger
            | PrimitiveShape::PointerAddressInteger => {
                if self.payload1 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::UnicodeScalar
            | PrimitiveShape::Utf16CodeUnit
            | PrimitiveShape::Utf32CodeUnit
            | PrimitiveShape::CPlainSignedChar
            | PrimitiveShape::CPlainUnsignedChar
            | PrimitiveShape::CSignedChar
            | PrimitiveShape::CUnsignedChar
            | PrimitiveShape::CWideChar
            | PrimitiveShape::CWideSignedChar
            | PrimitiveShape::CWideUnsignedChar => match TypeWidth::try_from_cell(self.payload1) {
                Ok(TypeWidth::Fixed(_)) => {}
                Ok(TypeWidth::Arch) | Err(_) => {
                    return Err(SemanticTypeFault::Width {
                        actual: self.payload1,
                    });
                }
            },
            PrimitiveShape::CBlockPointer => {
                if self.payload1 != 0 {
                    return Err(SemanticTypeFault::ReservedCell {
                        tag,
                        cell: TypeCell::Payload1,
                        actual: self.payload1,
                    });
                }
            }
            PrimitiveShape::Builtin => {}
        }
        let text_law = match shape {
            PrimitiveShape::Builtin => CellLaw::Required,
            PrimitiveShape::Reference => CellLaw::Optional,
            _ => CellLaw::Forbidden,
        };
        self.check_cell(TypeCell::Text, text_law, self.text.is_some())?;
        self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
        self.check_cell(
            TypeCell::Nominal,
            CellLaw::Forbidden,
            self.nominal.is_some(),
        )?;
        let owns_child = matches!(
            shape,
            PrimitiveShape::MutPointer
                | PrimitiveShape::ConstPointer
                | PrimitiveShape::Reference
                | PrimitiveShape::CPointer
                | PrimitiveShape::CBlockPointer
                | PrimitiveShape::CxxLvalueReference
                | PrimitiveShape::CxxRvalueReference
        );
        let expected_children = if shape == PrimitiveShape::CxxMemberPointer {
            2
        } else if owns_child {
            1
        } else {
            0
        };
        if child_count != expected_children {
            let law = if owns_child {
                ChildCountLaw { min: 1, max: 1 }
            } else if shape == PrimitiveShape::CxxMemberPointer {
                ChildCountLaw { min: 2, max: 2 }
            } else {
                ChildCountLaw { min: 0, max: 0 }
            };
            return Err(SemanticTypeFault::ChildCount {
                tag,
                law,
                actual: child_count,
            });
        }
        Ok(())
    }

    fn check_cell(
        &self,
        cell: TypeCell,
        law: CellLaw,
        present: bool,
    ) -> Result<(), SemanticTypeFault> {
        match (present, law) {
            (false, CellLaw::Required) => Err(SemanticTypeFault::MissingCell {
                tag: self.tag,
                cell,
            }),
            (true, CellLaw::Forbidden) => Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell,
                actual: 1,
            }),
            _ => Ok(()),
        }
    }

    fn require_no_cells(&self) -> Result<(), SemanticTypeFault> {
        if self.payload0 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload0,
                actual: self.payload0,
            });
        }
        if self.payload1 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload1,
                actual: self.payload1,
            });
        }
        self.require_no_text()
    }

    fn require_zero_payloads(&self) -> Result<(), SemanticTypeFault> {
        if self.payload0 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload0,
                actual: self.payload0,
            });
        }
        self.require_zero_payload1()
    }

    fn require_zero_payload1(&self) -> Result<(), SemanticTypeFault> {
        if self.payload1 != 0 {
            return Err(SemanticTypeFault::ReservedCell {
                tag: self.tag,
                cell: TypeCell::Payload1,
                actual: self.payload1,
            });
        }
        Ok(())
    }

    fn require_no_text(&self) -> Result<(), SemanticTypeFault> {
        self.check_cell(TypeCell::Text, CellLaw::Forbidden, self.text.is_some())?;
        self.check_cell(TypeCell::Text2, CellLaw::Forbidden, self.text2.is_some())?;
        self.check_cell(
            TypeCell::Nominal,
            CellLaw::Forbidden,
            self.nominal.is_some(),
        )
    }
}
