//! Current-schema admission for pooled extension facts.

use super::model::*;

impl<'bytes> ExtensionPoolsLane<'bytes> {
    /// Admits the current pooled lanes against the carrying fragment's lane
    /// counts. Legacy schemas are reopen-only and cannot be encoded through
    /// this current-write input.
    pub fn admit(
        &self,
        atom_count: u32,
        type_count: u32,
        entity_count: u32,
    ) -> Result<(), ExtensionPoolFault> {
        for (ordinal, parameter) in self.type_parameters.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            if parameter.name.is_empty() {
                return Err(ExtensionPoolFault::EmptyName { ordinal });
            }
            let bound_count = u32::try_from(self.type_parameter_bounds.len()).unwrap_or(u32::MAX);
            let bound_end = parameter
                .bounds
                .start
                .checked_add(parameter.bounds.length)
                .ok_or(ExtensionPoolFault::TypeParameterBounds {
                    ordinal,
                    start: parameter.bounds.start,
                    length: parameter.bounds.length,
                    bound_count,
                })?;
            if usize::try_from(bound_end).map_or(true, |end| end > self.type_parameter_bounds.len())
            {
                return Err(ExtensionPoolFault::TypeParameterBounds {
                    ordinal,
                    start: parameter.bounds.start,
                    length: parameter.bounds.length,
                    bound_count,
                });
            }
            for (position, bound) in self.type_parameter_bounds
                [parameter.bounds.start as usize..bound_end as usize]
                .iter()
                .enumerate()
            {
                let bound_ordinal = parameter
                    .bounds
                    .start
                    .checked_add(u32::try_from(position).unwrap_or(u32::MAX))
                    .unwrap_or(u32::MAX);
                match bound {
                    ExtensionTypeParameterBound::Type(raw) if *raw >= type_count => {
                        return Err(ExtensionPoolFault::BoundTypeReference {
                            bound: bound_ordinal,
                            raw: *raw,
                            limit: type_count,
                        });
                    }
                    ExtensionTypeParameterBound::Lifetime(name) if name.is_empty() => {
                        return Err(ExtensionPoolFault::EmptyLifetime {
                            bound: bound_ordinal,
                        });
                    }
                    ExtensionTypeParameterBound::Type(_)
                    | ExtensionTypeParameterBound::Lifetime(_) => {}
                }
            }
            for (field, raw) in [
                (TypeParameterField::Default, parameter.default),
                (
                    TypeParameterField::ConstValueType,
                    match parameter.kind {
                        ExtensionTypeParameterKind::Type { .. }
                        | ExtensionTypeParameterKind::Lifetime => None,
                        ExtensionTypeParameterKind::ConstValue { value_type } => Some(value_type),
                    },
                ),
            ] {
                if raw.is_some_and(|raw| raw >= type_count) {
                    return Err(ExtensionPoolFault::TypeReference {
                        ordinal,
                        field,
                        raw: raw.unwrap_or_default(),
                        limit: type_count,
                    });
                }
            }
            if !parameter.requirements.is_valid() {
                return Err(ExtensionPoolFault::TypeParameterRequirements {
                    ordinal,
                    primary: parameter.requirements.primary,
                    constructor: parameter.requirements.constructor,
                    allows_ref_like: parameter.requirements.allows_ref_like,
                });
            }
        }
        let predicate_count = u32::try_from(self.free_predicates.len()).unwrap_or(u32::MAX);
        for (ordinal, predicate) in self.free_predicates.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
            if predicate.subject >= type_count {
                return Err(ExtensionPoolFault::FreePredicateSubject {
                    predicate: ordinal,
                    raw: predicate.subject,
                    limit: type_count,
                });
            }
            let bound_count = u32::try_from(self.type_parameter_bounds.len()).unwrap_or(u32::MAX);
            let bound_end = predicate
                .bounds
                .start
                .checked_add(predicate.bounds.length)
                .ok_or(ExtensionPoolFault::FreePredicateBounds {
                    predicate: ordinal,
                    start: predicate.bounds.start,
                    length: predicate.bounds.length,
                    bound_count,
                })?;
            if usize::try_from(bound_end).map_or(true, |end| end > self.type_parameter_bounds.len())
            {
                return Err(ExtensionPoolFault::FreePredicateBounds {
                    predicate: ordinal,
                    start: predicate.bounds.start,
                    length: predicate.bounds.length,
                    bound_count,
                });
            }
            for (position, bound) in self.type_parameter_bounds
                [predicate.bounds.start as usize..bound_end as usize]
                .iter()
                .enumerate()
            {
                let bound_ordinal = predicate
                    .bounds
                    .start
                    .checked_add(u32::try_from(position).unwrap_or(u32::MAX))
                    .unwrap_or(u32::MAX);
                match bound {
                    ExtensionTypeParameterBound::Type(raw) if *raw >= type_count => {
                        return Err(ExtensionPoolFault::BoundTypeReference {
                            bound: bound_ordinal,
                            raw: *raw,
                            limit: type_count,
                        });
                    }
                    ExtensionTypeParameterBound::Lifetime(name) if name.is_empty() => {
                        return Err(ExtensionPoolFault::EmptyLifetime {
                            bound: bound_ordinal,
                        });
                    }
                    ExtensionTypeParameterBound::Type(_)
                    | ExtensionTypeParameterBound::Lifetime(_) => {}
                }
            }
        }
        for (list, range) in self.free_predicate_lists.iter().enumerate() {
            let list = u32::try_from(list).unwrap_or(u32::MAX);
            let end = range.start.checked_add(range.length).ok_or(
                ExtensionPoolFault::FreePredicateList {
                    list,
                    start: range.start,
                    length: range.length,
                    predicate_count,
                },
            )?;
            if end > predicate_count {
                return Err(ExtensionPoolFault::FreePredicateList {
                    list,
                    start: range.start,
                    length: range.length,
                    predicate_count,
                });
            }
        }
        let element_count = u32::try_from(self.type_parameters.len()).unwrap_or(u32::MAX);
        for (list, range) in self.type_parameter_lists.iter().enumerate() {
            let list = u32::try_from(list).unwrap_or(u32::MAX);
            let end = range.start.checked_add(range.length).ok_or(
                ExtensionPoolFault::TypeParameterRange {
                    list,
                    start: range.start,
                    length: range.length,
                    element_count,
                },
            )?;
            if end > element_count {
                return Err(ExtensionPoolFault::TypeParameterRange {
                    list,
                    start: range.start,
                    length: range.length,
                    element_count,
                });
            }
        }
        for (lane, lists, limit) in [
            (ExtensionPoolListLane::Atoms, self.atom_lists, atom_count),
            (ExtensionPoolListLane::Types, self.type_lists, type_count),
            (
                ExtensionPoolListLane::Entities,
                self.entity_lists,
                entity_count,
            ),
        ] {
            for (list, refs) in lists.iter().enumerate() {
                let list = u32::try_from(list).unwrap_or(u32::MAX);
                for (position, raw) in refs.elements.iter().enumerate() {
                    if *raw >= limit {
                        return Err(ExtensionPoolFault::Reference {
                            lane,
                            list,
                            position: u32::try_from(position).unwrap_or(u32::MAX),
                            raw: *raw,
                            limit,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}
