//! Tag admission for one semantic type record.
//!
//! Leaf construction and the child-count law stay on [`super::SemanticTypeRecord`].
//! This module proves payload, text, nominal, and per-child cells.

use super::*;

impl SemanticTypeRecord<'_> {
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
