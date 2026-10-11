//! Shallow subtype checks used during union/intersection normalization. Their
//! explicit task/result worklists use inline storage for common cases and may
//! grow for deeply nested function signatures.
//!
//! Every method here reads types through `lookup()` / `type_list()` style
//! accessors only -- never `intern()` or `evaluate()` -- so union and
//! intersection reduction can compare members without re-entering the interner
//! or triggering project-scale evaluation. Extracted from `normalize.rs` to keep
//! both files under the source-size cap; `normalize.rs` retains the canonical
//! reduction passes (`reduce_union_subtypes`, `reduce_intersection_subtypes`)
//! that call into this engine.

use super::normalize::{LiteralDomain, PrimitiveClass};
use super::{TypeInterner, TypeListBuffer};
use crate::intern::{UnionExecutionWork, expect_unmetered_union, tick_union_execution_work};
use crate::types::{
    FunctionShapeId, IntrinsicKind, LiteralValue, ObjectShapeId, TemplateLiteralId, TemplateSpan,
    TypeData, TypeId, TypeListId,
};
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use std::sync::Arc;
use tsz_common::ProjectExecutionStop;
use tsz_common::interner::Atom;

enum ShallowSubtypeTask {
    Evaluate {
        source: TypeId,
        target: TypeId,
        depth: u32,
    },
    AllSourceUnion {
        members: TypeListId,
        target: TypeId,
        depth: u32,
        index: usize,
    },
    AfterSourceUnionMember {
        members: TypeListId,
        target: TypeId,
        depth: u32,
        next_index: usize,
    },
    AnyTargetUnion {
        source: TypeId,
        members: TypeListId,
        depth: u32,
        index: usize,
    },
    AfterTargetUnionMember {
        source: TypeId,
        members: TypeListId,
        depth: u32,
        next_index: usize,
    },
    AfterFunctionReturn {
        source: FunctionShapeId,
        target: FunctionShapeId,
        depth: u32,
    },
    FunctionParameter {
        source: FunctionShapeId,
        target: FunctionShapeId,
        depth: u32,
        index: usize,
    },
    AfterFunctionParameter {
        source: FunctionShapeId,
        target: FunctionShapeId,
        depth: u32,
        next_index: usize,
        undefined_target_suffices: bool,
    },
    FinishFunction {
        source: FunctionShapeId,
        target: FunctionShapeId,
    },
}

impl TypeInterner {
    /// Shallow subtype check that avoids infinite recursion.
    /// Uses `TypeId` identity for nested components instead of recursive checking.
    /// This is safe for use during normalization because it only uses `lookup()` and
    /// never calls `intern()` or `evaluate()`.
    #[inline]
    pub(super) fn is_subtype_shallow(&self, source: TypeId, target: TypeId) -> bool {
        let mut work = None;
        expect_unmetered_union(self.is_subtype_shallow_with_work(source, target, &mut work))
    }

    pub(super) fn is_subtype_shallow_with_work(
        &self,
        source: TypeId,
        target: TypeId,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        self.is_subtype_shallow_depth_with_work(source, target, 3, work)
    }

    fn is_subtype_shallow_depth_with_work(
        &self,
        source: TypeId,
        target: TypeId,
        depth: u32,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        // Function return and parameter relations keep `depth` unchanged, so
        // recursive Rust calls could grow with the source program's nesting.
        // Keep the same branch order and depth rules on an explicit worklist;
        // every relation/parameter step is charged before more tasks are added.
        let mut tasks: SmallVec<[ShallowSubtypeTask; 16]> = SmallVec::new();
        let mut results: SmallVec<[bool; 16]> = SmallVec::new();
        tasks.push(ShallowSubtypeTask::Evaluate {
            source,
            target,
            depth,
        });

        while let Some(task) = tasks.pop() {
            match task {
                ShallowSubtypeTask::Evaluate {
                    source,
                    target,
                    depth,
                } => {
                    tick_union_execution_work(work, 1)?;
                    if source == target {
                        results.push(true);
                        continue;
                    }
                    if depth == 0 {
                        results.push(false);
                        continue;
                    }
                    if target.is_any_or_unknown() || source.is_never() {
                        results.push(true);
                        continue;
                    }

                    let s_data = self.lookup(source);
                    let t_data = self.lookup(target);
                    if matches!(
                        (&s_data, &t_data),
                        (
                            Some(TypeData::TypeParameter(_)) | _,
                            Some(TypeData::TypeParameter(_))
                        ) | (Some(TypeData::Lazy(_)) | _, Some(TypeData::Lazy(_)))
                    ) {
                        results.push(false);
                        continue;
                    }

                    if matches!(s_data, Some(TypeData::Literal(_))) {
                        if matches!(t_data, Some(TypeData::Literal(_))) {
                            results.push(false);
                            continue;
                        }
                        if let (
                            Some(TypeData::Literal(LiteralValue::String(literal))),
                            Some(TypeData::TemplateLiteral(template_id)),
                        ) = (&s_data, &t_data)
                        {
                            results.push(self.literal_string_matches_template_literal_with_work(
                                *literal,
                                *template_id,
                                work,
                            )?);
                            continue;
                        }
                        if let Some(TypeData::Union(members)) = t_data {
                            // Preserve the literal-source rule: unlike the
                            // general target-union check below, it scans every
                            // arm. Each child Evaluate task charges work, so a
                            // wide union stays cancellable without changing its
                            // prior subtype result.
                            tasks.push(ShallowSubtypeTask::AnyTargetUnion {
                                source,
                                members,
                                depth,
                                index: 0,
                            });
                            continue;
                        }
                        if let Some(domain) = self.literal_domain_from_type(source)
                            && let Some(target_class) = self.primitive_class_for(target)
                            && self.literal_domain_matches_primitive(domain, target_class)
                        {
                            results.push(true);
                            continue;
                        }
                    }

                    if let Some(TypeData::Enum(_, structural)) = s_data
                        && !matches!(t_data, Some(TypeData::Enum(..)))
                    {
                        tasks.push(ShallowSubtypeTask::Evaluate {
                            source: structural,
                            target,
                            depth,
                        });
                        continue;
                    }

                    if self.is_builtin_type(source)
                        && let Some(TypeData::Union(members)) = t_data
                    {
                        let mut found = false;
                        for &member in self.type_list(members).iter() {
                            tick_union_execution_work(work, 1)?;
                            if member == source {
                                found = true;
                                break;
                            }
                        }
                        results.push(found);
                        continue;
                    }

                    if let Some(TypeData::Union(members)) = s_data {
                        let member_count = self.type_list(members).len();
                        if member_count > 8 {
                            results.push(false);
                        } else if member_count == 0 {
                            results.push(true);
                        } else {
                            tasks.push(ShallowSubtypeTask::AllSourceUnion {
                                members,
                                target,
                                depth: depth - 1,
                                index: 0,
                            });
                        }
                        continue;
                    }

                    if let Some(TypeData::Union(members)) = t_data {
                        let member_count = self.type_list(members).len();
                        if member_count > 8 || member_count == 0 {
                            results.push(false);
                        } else {
                            tasks.push(ShallowSubtypeTask::AnyTargetUnion {
                                source,
                                members,
                                depth: depth - 1,
                                index: 0,
                            });
                        }
                        continue;
                    }

                    match (s_data, t_data) {
                        (
                            Some(TypeData::Object(s_id) | TypeData::ObjectWithIndex(s_id)),
                            Some(TypeData::Object(t_id) | TypeData::ObjectWithIndex(t_id)),
                        ) => results.push(self.is_object_shape_subtype_shallow_depth_with_work(
                            s_id, t_id, 0, work,
                        )?),
                        (Some(TypeData::Function(s_id)), Some(TypeData::Function(t_id))) => {
                            tick_union_execution_work(work, 1)?;
                            if s_id == t_id {
                                results.push(true);
                                continue;
                            }
                            let source_shape = self.function_shape(s_id);
                            let target_shape = self.function_shape(t_id);
                            if !source_shape.type_params.is_empty()
                                || !target_shape.type_params.is_empty()
                                || source_shape.this_type != target_shape.this_type
                            {
                                results.push(false);
                                continue;
                            }
                            if source_shape.return_type == target_shape.return_type {
                                tasks.push(ShallowSubtypeTask::FunctionParameter {
                                    source: s_id,
                                    target: t_id,
                                    depth,
                                    index: 0,
                                });
                            } else {
                                tasks.push(ShallowSubtypeTask::AfterFunctionReturn {
                                    source: s_id,
                                    target: t_id,
                                    depth,
                                });
                                tasks.push(ShallowSubtypeTask::Evaluate {
                                    source: source_shape.return_type,
                                    target: target_shape.return_type,
                                    depth,
                                });
                            }
                        }
                        _ => results.push(false),
                    }
                }
                ShallowSubtypeTask::AllSourceUnion {
                    members,
                    target,
                    depth,
                    index,
                } => {
                    let list = self.type_list(members);
                    if index >= list.len() {
                        results.push(true);
                    } else {
                        let member = list[index];
                        tasks.push(ShallowSubtypeTask::AfterSourceUnionMember {
                            members,
                            target,
                            depth,
                            next_index: index + 1,
                        });
                        tasks.push(ShallowSubtypeTask::Evaluate {
                            source: member,
                            target,
                            depth,
                        });
                    }
                }
                ShallowSubtypeTask::AfterSourceUnionMember {
                    members,
                    target,
                    depth,
                    next_index,
                } => {
                    if !results.pop().expect("union member result is present") {
                        results.push(false);
                    } else {
                        tasks.push(ShallowSubtypeTask::AllSourceUnion {
                            members,
                            target,
                            depth,
                            index: next_index,
                        });
                    }
                }
                ShallowSubtypeTask::AnyTargetUnion {
                    source,
                    members,
                    depth,
                    index,
                } => {
                    let list = self.type_list(members);
                    if index >= list.len() {
                        results.push(false);
                    } else {
                        let member = list[index];
                        tasks.push(ShallowSubtypeTask::AfterTargetUnionMember {
                            source,
                            members,
                            depth,
                            next_index: index + 1,
                        });
                        tasks.push(ShallowSubtypeTask::Evaluate {
                            source,
                            target: member,
                            depth,
                        });
                    }
                }
                ShallowSubtypeTask::AfterTargetUnionMember {
                    source,
                    members,
                    depth,
                    next_index,
                } => {
                    if results.pop().expect("union member result is present") {
                        results.push(true);
                    } else {
                        tasks.push(ShallowSubtypeTask::AnyTargetUnion {
                            source,
                            members,
                            depth,
                            index: next_index,
                        });
                    }
                }
                ShallowSubtypeTask::AfterFunctionReturn {
                    source,
                    target,
                    depth,
                } => {
                    if results.pop().expect("function return result is present") {
                        tasks.push(ShallowSubtypeTask::FunctionParameter {
                            source,
                            target,
                            depth,
                            index: 0,
                        });
                    } else {
                        results.push(false);
                    }
                }
                ShallowSubtypeTask::FunctionParameter {
                    source,
                    target,
                    depth,
                    index,
                } => {
                    let source_shape = self.function_shape(source);
                    let target_shape = self.function_shape(target);
                    let min_len = source_shape.params.len().min(target_shape.params.len());
                    if index >= min_len {
                        if source_shape.params.len() > target_shape.params.len() {
                            results.push(false);
                        } else {
                            tasks.push(ShallowSubtypeTask::FinishFunction { source, target });
                        }
                        continue;
                    }

                    tick_union_execution_work(work, 1)?;
                    let source_param = source_shape.params[index];
                    let target_param = target_shape.params[index];
                    tick_union_execution_work(work, 1)?;
                    if source_param.type_id == target_param.type_id {
                        if target_param.optional
                            && !source_param.optional
                            && !self
                                .type_contains_undefined_with_work(source_param.type_id, work)?
                        {
                            results.push(false);
                        } else {
                            tasks.push(ShallowSubtypeTask::FunctionParameter {
                                source,
                                target,
                                depth,
                                index: index + 1,
                            });
                        }
                        continue;
                    }

                    let (has_undefined, undefined_target_suffices) =
                        match (target_param.optional, source_param.optional) {
                            (false, false) | (false, true) => (true, false),
                            (true, true) => (true, target_param.type_id == TypeId::UNDEFINED),
                            (true, false) => (
                                self.type_contains_undefined_with_work(source_param.type_id, work)?,
                                false,
                            ),
                        };
                    if !has_undefined {
                        results.push(false);
                        continue;
                    }
                    tasks.push(ShallowSubtypeTask::AfterFunctionParameter {
                        source,
                        target,
                        depth,
                        next_index: index + 1,
                        undefined_target_suffices,
                    });
                    tasks.push(ShallowSubtypeTask::Evaluate {
                        source: target_param.type_id,
                        target: source_param.type_id,
                        depth,
                    });
                }
                ShallowSubtypeTask::AfterFunctionParameter {
                    source,
                    target,
                    depth,
                    next_index,
                    undefined_target_suffices,
                } => {
                    if results.pop().expect("function parameter result is present")
                        || undefined_target_suffices
                    {
                        tasks.push(ShallowSubtypeTask::FunctionParameter {
                            source,
                            target,
                            depth,
                            index: next_index,
                        });
                    } else {
                        results.push(false);
                    }
                }
                ShallowSubtypeTask::FinishFunction { source, target } => {
                    let source_shape = self.function_shape(source);
                    let target_shape = self.function_shape(target);
                    let min_len = source_shape.params.len().min(target_shape.params.len());
                    let mut all_identical = min_len > 0;
                    for index in 0..min_len {
                        tick_union_execution_work(work, 1)?;
                        all_identical &= source_shape.params[index].type_id
                            == target_shape.params[index].type_id;
                    }
                    results.push(!all_identical);
                }
            }
        }

        Ok(results.pop().expect("shallow subtype result is present"))
    }

    fn literal_string_matches_template_literal_with_work(
        &self,
        literal: Atom,
        template_id: TemplateLiteralId,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        tick_union_execution_work(work, 1)?;
        let literal = self.resolve_atom(literal);
        if literal.len() > 128 {
            return Ok(false);
        }
        let spans = self.template_list(template_id);
        if spans.len() > 8 {
            return Ok(false);
        }
        self.match_template_literal_shallow_with_work(literal.as_str(), &spans, 0, work)
    }

    pub(super) fn remove_string_literals_matched_by_templates_with_work(
        &self,
        flat: &mut TypeListBuffer,
        string_literals: &[(usize, Atom)],
        templates: &[TemplateLiteralId],
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        type TemplateCandidate = (Option<Arc<str>>, Arc<[TemplateSpan]>);
        let mut candidates: SmallVec<[TemplateCandidate; 8]> = SmallVec::new();
        for &template_id in templates {
            tick_union_execution_work(work, 1)?;
            let spans = self.template_list(template_id);
            if spans.len() > 8 {
                continue;
            }
            let prefix = match spans.first() {
                Some(TemplateSpan::Text(atom)) => Some(self.resolve_atom_ref(*atom)),
                _ => None,
            };
            candidates.push((prefix, spans));
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let mut removed: FxHashSet<usize> = FxHashSet::default();
        for &(index, atom) in string_literals {
            tick_union_execution_work(work, 1)?;
            let literal = self.resolve_atom_ref(atom);
            if literal.len() > 128 {
                continue;
            }
            for (prefix, spans) in &candidates {
                tick_union_execution_work(work, 1)?;
                if prefix
                    .as_ref()
                    .is_some_and(|prefix| !literal.starts_with(prefix.as_ref()))
                {
                    continue;
                }
                if self.match_template_literal_shallow_with_work(&literal, spans, 0, work)? {
                    removed.insert(index);
                    break;
                }
            }
        }
        if !removed.is_empty() {
            let mut write = 0;
            for read in 0..flat.len() {
                tick_union_execution_work(work, 1)?;
                if !removed.contains(&read) {
                    flat[write] = flat[read];
                    write += 1;
                }
            }
            flat.truncate(write);
        }
        Ok(())
    }

    fn match_template_literal_shallow_with_work(
        &self,
        remaining: &str,
        spans: &[TemplateSpan],
        span_idx: usize,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        tick_union_execution_work(work, 1)?;
        let Some(span) = spans.get(span_idx) else {
            return Ok(remaining.is_empty());
        };
        match span {
            TemplateSpan::Text(text) => {
                let text = self.resolve_atom(*text);
                if let Some(remaining) = remaining.strip_prefix(text.as_str()) {
                    self.match_template_literal_shallow_with_work(
                        remaining,
                        spans,
                        span_idx + 1,
                        work,
                    )
                } else {
                    Ok(false)
                }
            }
            TemplateSpan::Type(type_id) => self.match_template_type_span_shallow_with_work(
                remaining, spans, span_idx, *type_id, work,
            ),
        }
    }

    fn match_template_type_span_shallow_with_work(
        &self,
        remaining: &str,
        spans: &[TemplateSpan],
        span_idx: usize,
        type_id: TypeId,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        tick_union_execution_work(work, 1)?;
        match self.lookup(type_id) {
            Some(TypeData::Intrinsic(IntrinsicKind::Number)) => {
                let num_len =
                    crate::relations::subtype::rules::literals::find_number_length(remaining);
                for len in (1..=num_len).rev() {
                    tick_union_execution_work(work, 1)?;
                    if crate::relations::subtype::rules::literals::is_valid_number(
                        &remaining[..len],
                    ) && self.match_template_literal_shallow_with_work(
                        &remaining[len..],
                        spans,
                        span_idx + 1,
                        work,
                    )? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Some(TypeData::Intrinsic(IntrinsicKind::Bigint)) => {
                let len =
                    crate::relations::subtype::rules::literals::find_integer_length(remaining);
                Ok(len > 0
                    && self.match_template_literal_shallow_with_work(
                        &remaining[len..],
                        spans,
                        span_idx + 1,
                        work,
                    )?)
            }
            Some(TypeData::Intrinsic(IntrinsicKind::Boolean)) => {
                for prefix in ["true", "false"] {
                    tick_union_execution_work(work, 1)?;
                    if let Some(remaining) = remaining.strip_prefix(prefix)
                        && self.match_template_literal_shallow_with_work(
                            remaining,
                            spans,
                            span_idx + 1,
                            work,
                        )?
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Some(TypeData::Literal(literal)) => {
                let literal_text: std::borrow::Cow<'_, str> = match literal {
                    LiteralValue::String(atom) | LiteralValue::BigInt(atom) => {
                        self.resolve_atom(atom).into()
                    }
                    LiteralValue::Number(number) => crate::utils::js_number_to_string(number.0),
                    LiteralValue::Boolean(value) => {
                        if value {
                            "true".into()
                        } else {
                            "false".into()
                        }
                    }
                };
                if let Some(remaining) = remaining.strip_prefix(literal_text.as_ref()) {
                    self.match_template_literal_shallow_with_work(
                        remaining,
                        spans,
                        span_idx + 1,
                        work,
                    )
                } else {
                    Ok(false)
                }
            }
            Some(TypeData::Union(list_id)) => {
                let members = self.type_list(list_id);
                if members.len() > 8 {
                    return Ok(false);
                }
                for &member in members.iter() {
                    tick_union_execution_work(work, 1)?;
                    if self.match_template_type_span_shallow_with_work(
                        remaining, spans, span_idx, member, work,
                    )? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn is_object_shape_subtype_shallow_depth_with_work(
        &self,
        s_id: ObjectShapeId,
        t_id: ObjectShapeId,
        depth: u32,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        tick_union_execution_work(work, 1)?;
        if s_id == t_id {
            return Ok(true);
        }
        let source = self.object_shape(s_id);
        let target = self.object_shape(t_id);
        if target.symbol.is_some() && source.symbol != target.symbol {
            return Ok(false);
        }
        if target.string_index.is_some() || target.number_index.is_some() {
            return Ok(false);
        }

        let mut source_index = 0;
        let mut has_overlap = false;
        for target_property in &target.properties {
            tick_union_execution_work(work, 1)?;
            while source_index < source.properties.len()
                && source.properties[source_index].name < target_property.name
            {
                tick_union_execution_work(work, 1)?;
                source_index += 1;
            }

            if source_index < source.properties.len()
                && source.properties[source_index].name == target_property.name
            {
                let source_property = &source.properties[source_index];
                has_overlap = true;
                if source_property.type_id != target_property.type_id
                    && !self.is_subtype_shallow_depth_with_work(
                        source_property.type_id,
                        target_property.type_id,
                        depth,
                        work,
                    )?
                {
                    return Ok(false);
                }
                if (!target_property.optional && source_property.optional)
                    || (!target_property.readonly && source_property.readonly)
                {
                    return Ok(false);
                }
                source_index += 1;
            } else if !target_property.optional {
                return Ok(false);
            }
        }
        Ok(has_overlap)
    }

    /// Check if a type is a built-in primitive (string, number, boolean, etc.).
    /// These are safe to check against union targets without risk of cascading
    /// reductions that affect complex type inference.
    const fn is_builtin_type(&self, id: TypeId) -> bool {
        matches!(
            id,
            TypeId::STRING
                | TypeId::NUMBER
                | TypeId::BOOLEAN
                | TypeId::BIGINT
                | TypeId::SYMBOL
                | TypeId::VOID
                | TypeId::UNDEFINED
                | TypeId::NULL
                | TypeId::BOOLEAN_TRUE
                | TypeId::BOOLEAN_FALSE
        )
    }

    fn type_contains_undefined_with_work(
        &self,
        type_id: TypeId,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        tick_union_execution_work(work, 1)?;
        if type_id == TypeId::UNDEFINED {
            return Ok(true);
        }
        if type_id.is_intrinsic() {
            return Ok(false);
        }
        if let Some(TypeData::Union(members)) = self.lookup(type_id) {
            for &member in self.type_list(members).iter() {
                tick_union_execution_work(work, 1)?;
                if member == TypeId::UNDEFINED {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Check if a literal domain matches a primitive class.
    const fn literal_domain_matches_primitive(
        &self,
        domain: LiteralDomain,
        class: PrimitiveClass,
    ) -> bool {
        matches!(
            (domain, class),
            (LiteralDomain::String, PrimitiveClass::String)
                | (LiteralDomain::Number, PrimitiveClass::Number)
                | (LiteralDomain::Boolean, PrimitiveClass::Boolean)
                | (LiteralDomain::Bigint, PrimitiveClass::Bigint)
        )
    }

    /// Coarse structural bucket for a single union member, used to skip
    /// `is_subtype_shallow` calls on pairs that provably cannot relate.
    ///
    /// Each variant mirrors exactly one branch of `is_subtype_shallow_depth`,
    /// so `ShallowReduceKind::may_relate` is a sound over-approximation of that
    /// relation: it returns `true` for every pair the shallow engine could ever
    /// answer `true` on, and `false` only for pairs the engine answers `false`
    /// on in both directions. `Wildcard` is the conservative catch-all for any
    /// member that participates in a non-local branch (unions, or any leaf the
    /// engine could traverse that this classifier does not model precisely), so
    /// an unmodeled member never skips a check.
    ///
    /// Callers must classify a union **after** normalization has run its
    /// sentinel removal, literal absorption, and sort+dedup passes (i.e. exactly
    /// the input `reduce_union_subtypes` operates on). In that state
    /// `any`/`unknown`/`never` are gone and identical members are deduped, so
    /// the early identity / top / bottom branches of `is_subtype_shallow_depth`
    /// never fire across a distinct pair.
    pub(super) fn shallow_reduce_kind(&self, id: TypeId) -> ShallowReduceKind {
        // A widened builtin primitive (string/number/... and the boolean-literal
        // intrinsics) only relates upward into a union target, which is modeled
        // as `Wildcard` on the other endpoint. Against any non-union peer it is
        // disjoint, so it only ever needs a check versus a literal of its own
        // domain (handled by `Literal -> Primitive`) or a `Wildcard`.
        if let Some(class) = self.builtin_primitive_class(id) {
            return ShallowReduceKind::Primitive(class);
        }
        match self.lookup(id) {
            Some(TypeData::Literal(literal)) => {
                let domain = match literal {
                    LiteralValue::String(_) => LiteralDomain::String,
                    LiteralValue::Number(_) => LiteralDomain::Number,
                    LiteralValue::Boolean(_) => LiteralDomain::Boolean,
                    LiteralValue::BigInt(_) => LiteralDomain::Bigint,
                };
                ShallowReduceKind::Literal(domain)
            }
            Some(TypeData::Object(_) | TypeData::ObjectWithIndex(_)) => ShallowReduceKind::Object,
            Some(TypeData::Function(_)) => ShallowReduceKind::Function,
            Some(TypeData::TemplateLiteral(_)) => ShallowReduceKind::Template,
            // A union member recurses both directions in the shallow engine, and
            // any leaf the classifier does not model (Array/Tuple/Enum/
            // Application/Callable/intrinsics other than primitives/Readonly/
            // NoInfer/...) is treated conservatively as relatable so a real
            // relation is never skipped. These are rare relative to the
            // object/primitive/literal members that dominate the large-row shape.
            _ => ShallowReduceKind::Wildcard,
        }
    }

    /// Classify the builtin widened primitives (and the boolean-literal
    /// intrinsics) the shallow engine treats as absorbing targets for matching
    /// literals. Returns `None` for everything else, including bare
    /// `TypeData::Literal` (those are `ShallowReduceKind::Literal`).
    const fn builtin_primitive_class(&self, id: TypeId) -> Option<PrimitiveClass> {
        match id {
            TypeId::STRING => Some(PrimitiveClass::String),
            TypeId::NUMBER => Some(PrimitiveClass::Number),
            TypeId::BIGINT => Some(PrimitiveClass::Bigint),
            TypeId::SYMBOL => Some(PrimitiveClass::Symbol),
            TypeId::BOOLEAN | TypeId::BOOLEAN_TRUE | TypeId::BOOLEAN_FALSE => {
                Some(PrimitiveClass::Boolean)
            }
            TypeId::VOID | TypeId::UNDEFINED => Some(PrimitiveClass::Undefined),
            TypeId::NULL => Some(PrimitiveClass::Null),
            _ => None,
        }
    }
}

/// Coarse structural bucket used to skip provably-disjoint pairs in union
/// subtype reduction. See `TypeInterner::shallow_reduce_kind`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ShallowReduceKind {
    /// A bare `TypeData::Literal` of the given domain.
    Literal(LiteralDomain),
    /// A widened builtin primitive (or boolean-literal intrinsic) of the class.
    Primitive(PrimitiveClass),
    /// `Object` / `ObjectWithIndex`.
    Object,
    /// `Function`.
    Function,
    /// `TemplateLiteral`.
    Template,
    /// Union, or any member not modeled precisely: always needs a real check.
    Wildcard,
}

impl ShallowReduceKind {
    /// Sound over-approximation of `is_subtype_shallow(source, target)` for a
    /// distinct, post-normalization pair: `false` only when the shallow engine
    /// provably answers `false` in this direction.
    ///
    /// The direction matters — `Literal(String)` as a *source* can be a subtype
    /// of `Primitive(String)` / `Template`, but as a *target* a literal is only
    /// matched by an identical literal (already deduped). Modeling the source
    /// role lets the symmetric quadratic loop skip the reverse direction too.
    pub(super) const fn may_relate(source: Self, target: Self) -> bool {
        // Any union (or unmodeled leaf) on either side keeps the real check:
        // the shallow engine recurses into unions both as source and target.
        if matches!(source, Self::Wildcard) || matches!(target, Self::Wildcard) {
            return true;
        }
        match source {
            Self::Wildcard => true,
            // A literal source relates to a same-domain primitive, or (string
            // only) to a template literal. Never to another literal, object, or
            // function.
            Self::Literal(domain) => match target {
                Self::Primitive(class) => primitive_class_matches_domain(class, domain),
                Self::Template => matches!(domain, LiteralDomain::String),
                _ => false,
            },
            // Objects relate only to objects; functions only to functions.
            Self::Object => matches!(target, Self::Object),
            Self::Function => matches!(target, Self::Function),
            // A widened primitive source only relates upward into a union
            // (Wildcard, handled above), and a template source is only a subtype
            // by identity (already deduped) — the shallow engine has no
            // template-as-source relation. Versus any concrete peer both are
            // disjoint.
            Self::Primitive(_) | Self::Template => false,
        }
    }
}

/// Domain/class match used by `ShallowReduceKind::may_relate`. Kept as a free
/// function so the const matrix is shared without borrowing the interner.
const fn primitive_class_matches_domain(class: PrimitiveClass, domain: LiteralDomain) -> bool {
    matches!(
        (domain, class),
        (LiteralDomain::String, PrimitiveClass::String)
            | (LiteralDomain::Number, PrimitiveClass::Number)
            | (LiteralDomain::Boolean, PrimitiveClass::Boolean)
            | (LiteralDomain::Bigint, PrimitiveClass::Bigint)
    )
}
