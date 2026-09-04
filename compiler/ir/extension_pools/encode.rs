//! Current-schema encoder for admitted pooled extension facts.

use super::{grammar::*, model::*};
use crate::{TypeParameterInference, TypeParameterPrimaryRequirement, TypeParameterRequirements, Variance};

impl<'bytes> ExtensionPoolsLane<'bytes> {
    /// The exact current-schema serialized payload length for these pooled lanes.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        let mut length = 4;
        for parameter in self.type_parameters {
            length += 5 + parameter.name.len();
            length += 8;
            length += optional_len(parameter.default);
            length += 4;
            if matches!(parameter.kind, ExtensionTypeParameterKind::ConstValue { .. }) {
                length += 4;
            }
        }
        length += 4;
        for bound in self.type_parameter_bounds {
            length += match bound {
                ExtensionTypeParameterBound::Type(_) => 5,
                ExtensionTypeParameterBound::Lifetime(name) => 6 + name.len(),
            };
        }
        length += 4 + 8 * self.type_parameter_lists.len();
        for lane in [self.atom_lists, self.type_lists, self.entity_lists] {
            length += 4;
            for list in lane {
                length += 4 + 4 * list.elements.len();
            }
        }
        length
    }

    /// Serializes current-schema pooled lanes in canonical order. Admission
    /// has already proved every cell and the payload has exact measured size.
    pub fn write_payload(&self, payload: &mut [u8]) {
        let mut cursor = write_u32(payload, 0, u32::try_from(self.type_parameters.len()).unwrap_or(u32::MAX));
        for parameter in self.type_parameters {
            cursor = write_cell(payload, cursor, parameter.name);
            cursor = write_u32(payload, cursor, parameter.bounds.start);
            cursor = write_u32(payload, cursor, parameter.bounds.length);
            cursor = write_optional(payload, cursor, parameter.default);
            payload[cursor] = variance_tag(parameter.variance);
            cursor += 1;
            match parameter.kind {
                ExtensionTypeParameterKind::Type { inference } => {
                    payload[cursor] = type_parameter_kind_tag(inference);
                    cursor += 1;
                }
                ExtensionTypeParameterKind::ConstValue { value_type } => {
                    payload[cursor] = TYPE_PARAMETER_KIND_CONST_VALUE;
                    cursor += 1;
                    cursor = write_u32(payload, cursor, value_type);
                }
                ExtensionTypeParameterKind::Lifetime => {
                    payload[cursor] = TYPE_PARAMETER_KIND_LIFETIME;
                    cursor += 1;
                }
            }
            payload[cursor] = primary_requirement_tag(parameter.requirements.primary);
            cursor += 1;
            payload[cursor] = requirement_flags(parameter.requirements);
            cursor += 1;
        }
        cursor = write_u32(payload, cursor, u32::try_from(self.type_parameter_bounds.len()).unwrap_or(u32::MAX));
        for bound in self.type_parameter_bounds {
            match bound {
                ExtensionTypeParameterBound::Type(raw) => {
                    payload[cursor] = TYPE_PARAMETER_BOUND_TYPE;
                    cursor = write_u32(payload, cursor + 1, *raw);
                }
                ExtensionTypeParameterBound::Lifetime(name) => {
                    payload[cursor] = TYPE_PARAMETER_BOUND_LIFETIME;
                    cursor = write_cell(payload, cursor + 1, name);
                }
            }
        }
        cursor = write_u32(payload, cursor, u32::try_from(self.type_parameter_lists.len()).unwrap_or(u32::MAX));
        for range in self.type_parameter_lists {
            cursor = write_u32(payload, cursor, range.start);
            cursor = write_u32(payload, cursor, range.length);
        }
        for lane in [self.atom_lists, self.type_lists, self.entity_lists] {
            cursor = write_u32(payload, cursor, u32::try_from(lane.len()).unwrap_or(u32::MAX));
            for list in lane {
                cursor = write_u32(payload, cursor, u32::try_from(list.elements.len()).unwrap_or(u32::MAX));
                for raw in list.elements {
                    cursor = write_u32(payload, cursor, *raw);
                }
            }
        }
    }
}

const fn optional_len(value: Option<u32>) -> usize {
    match value { None => 1, Some(_) => 5 }
}

const fn variance_tag(variance: Variance) -> u8 {
    match variance {
        Variance::Invariant => VARIANCE_INVARIANT,
        Variance::Covariant => VARIANCE_COVARIANT,
        Variance::Contravariant => VARIANCE_CONTRAVARIANT,
        Variance::Bivariant => VARIANCE_BIVARIANT,
    }
}

const fn type_parameter_kind_tag(inference: TypeParameterInference) -> u8 {
    match inference {
        TypeParameterInference::Ordinary => TYPE_PARAMETER_KIND_TYPE,
        TypeParameterInference::Const => TYPE_PARAMETER_KIND_CONST_INFERENCE,
    }
}

const fn primary_requirement_tag(primary: TypeParameterPrimaryRequirement) -> u8 {
    match primary {
        TypeParameterPrimaryRequirement::None => PRIMARY_REQUIREMENT_NONE,
        TypeParameterPrimaryRequirement::Reference { nullable: false } => {
            PRIMARY_REQUIREMENT_REFERENCE
        }
        TypeParameterPrimaryRequirement::Reference { nullable: true } => {
            PRIMARY_REQUIREMENT_NULLABLE_REFERENCE
        }
        TypeParameterPrimaryRequirement::Value => PRIMARY_REQUIREMENT_VALUE,
        TypeParameterPrimaryRequirement::Unmanaged => PRIMARY_REQUIREMENT_UNMANAGED,
        TypeParameterPrimaryRequirement::NotNull => PRIMARY_REQUIREMENT_NOT_NULL,
        TypeParameterPrimaryRequirement::Default => PRIMARY_REQUIREMENT_DEFAULT,
    }
}

const fn requirement_flags(requirements: TypeParameterRequirements) -> u8 {
    (if requirements.constructor { REQUIREMENT_CONSTRUCTOR } else { 0 })
        | if requirements.allows_ref_like { REQUIREMENT_ALLOWS_REF_LIKE } else { 0 }
}

fn write_u32(payload: &mut [u8], at: usize, value: u32) -> usize {
    payload[at..at + 4].copy_from_slice(&value.to_le_bytes());
    at + 4
}

fn write_cell(payload: &mut [u8], cursor: usize, bytes: &[u8]) -> usize {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    payload[cursor] = PRESENCE_SOME;
    payload[cursor + 1..cursor + 5].copy_from_slice(&len.to_le_bytes());
    payload[cursor + 5..cursor + 5 + bytes.len()].copy_from_slice(bytes);
    cursor + 5 + bytes.len()
}

fn write_optional(payload: &mut [u8], cursor: usize, value: Option<u32>) -> usize {
    match value {
        None => { payload[cursor] = PRESENCE_NONE; cursor + 1 }
        Some(raw) => {
            payload[cursor] = PRESENCE_SOME;
            payload[cursor + 1..cursor + 5].copy_from_slice(&raw.to_le_bytes());
            cursor + 5
        }
    }
}
