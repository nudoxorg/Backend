// Cooperative union construction helpers used by QueryCache.

impl TypeInterner {
    fn with_union_execution_work<'checkpoint, Output>(
        &self,
        checkpoint: &'checkpoint dyn ExecutionCheckpoint,
        operation: impl FnOnce(
            &mut Option<&mut UnionExecutionWork<'checkpoint>>,
        ) -> Result<Output, ProjectExecutionStop>,
    ) -> Result<Output, ProjectExecutionStop> {
        let complexity_checkpoint = self.union_complexity_checkpoint();
        let result = (|| {
            let mut execution_work = UnionExecutionWork::new(checkpoint)?;
            let output = {
                let mut metered_work = Some(&mut execution_work);
                let output = operation(&mut metered_work)?;
                drop(metered_work);
                output
            };
            Ok(output)
        })();
        if result.is_err() {
            self.discard_union_too_complex_since(complexity_checkpoint);
        }
        result
    }

    /// Budgeted counterpart to [`Self::union`]. The checkpoint is borrowed for
    /// this one normalization call; it is never stored on the shared interner.
    pub(crate) fn union_with_execution_checkpoint(
        &self,
        members: Vec<TypeId>,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_from_iter_with_work(members, work)
        })
    }

    pub(crate) fn union_from_slice_with_execution_checkpoint(
        &self,
        members: &[TypeId],
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_from_iter_with_work(members.iter().copied(), work)
        })
    }

    pub(crate) fn union_literal_reduce_with_execution_checkpoint(
        &self,
        members: Vec<TypeId>,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_literal_reduce_from_iter_with_work(members, work)
        })
    }

    pub(crate) fn union_from_sorted_vec_with_execution_checkpoint(
        &self,
        flat: Vec<TypeId>,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_from_sorted_vec_with_work(flat, work)
        })
    }

    fn union_from_sorted_vec_with_work(
        &self,
        flat: Vec<TypeId>,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        if flat.is_empty() {
            flush_union_execution_work(work)?;
            return Ok(TypeId::NEVER);
        }
        if flat.len() == 1 {
            if work.is_some() {
                tick_union_execution_work(work, 1)?;
            }
            flush_union_execution_work(work)?;
            return Ok(flat[0]);
        }

        if self.strict_null_checks() {
            if work.is_some() {
                for _ in &flat {
                    tick_union_execution_work(work, 1)?;
                }
            }
            flush_union_execution_work(work)?;
            let list_id = self.intern_type_list(flat);
            return Ok(self.intern(TypeData::Union(list_id)));
        }

        // Preserve the sorted Vec allocation for the strict fast path above. In
        // non-strict mode, re-use its heap allocation when it fits SmallVec's
        // spill representation, while still charging the consumed input.
        if work.is_some() {
            for _ in &flat {
                tick_union_execution_work(work, 1)?;
            }
        }
        let mut reduced = TypeListBuffer::from_vec(flat);
        if let Some(collapsed) = self.reduce_and_collapse_nonstrict_with_work(&mut reduced, work)? {
            flush_union_execution_work(work)?;
            Ok(collapsed)
        } else {
            flush_union_execution_work(work)?;
            let list_id = self.intern_type_list_from_slice(&reduced);
            Ok(self.intern(TypeData::Union(list_id)))
        }
    }

    pub(crate) fn union2_with_execution_checkpoint(
        &self,
        left: TypeId,
        right: TypeId,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| self.union2_with_work(left, right, work))
    }

    fn union2_with_work(
        &self,
        left: TypeId,
        right: TypeId,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        let result = if left == right {
            flush_union_execution_work(work)?;
            left
        } else if left == TypeId::NEVER {
            flush_union_execution_work(work)?;
            right
        } else if right == TypeId::NEVER {
            flush_union_execution_work(work)?;
            left
        } else if !self.strict_null_checks() {
            self.union_from_iter_with_work([left, right], work)?
        } else if right.is_nullable()
            && let Some(TypeData::Union(list_id)) = self.lookup(left)
        {
            let members = self.type_list(list_id);
            let mut found = false;
            for &member in members.iter() {
                tick_union_execution_work(work, 1)?;
                if member == right {
                    found = true;
                    break;
                }
            }
            if found {
                flush_union_execution_work(work)?;
                left
            } else if let Some(result) = self.try_union2_insert_with_work(work, left, right)? {
                flush_union_execution_work(work)?;
                result
            } else if let Some(result) = self.try_union2_insert_with_work(work, right, left)? {
                flush_union_execution_work(work)?;
                result
            } else {
                self.union_from_iter_with_work([left, right], work)?
            }
        } else if left.is_nullable()
            && let Some(TypeData::Union(list_id)) = self.lookup(right)
        {
            let members = self.type_list(list_id);
            let mut found = false;
            for &member in members.iter() {
                tick_union_execution_work(work, 1)?;
                if member == left {
                    found = true;
                    break;
                }
            }
            if found {
                flush_union_execution_work(work)?;
                right
            } else if let Some(result) = self.try_union2_insert_with_work(work, left, right)? {
                flush_union_execution_work(work)?;
                result
            } else if let Some(result) = self.try_union2_insert_with_work(work, right, left)? {
                flush_union_execution_work(work)?;
                result
            } else {
                self.union_from_iter_with_work([left, right], work)?
            }
        } else if let Some(result) = self.try_union2_insert_with_work(work, left, right)? {
            flush_union_execution_work(work)?;
            result
        } else if let Some(result) = self.try_union2_insert_with_work(work, right, left)? {
            flush_union_execution_work(work)?;
            result
        } else {
            self.union_from_iter_with_work([left, right], work)?
        };
        Ok(result)
    }

    fn try_union2_insert_with_work(
        &self,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
        single: TypeId,
        existing: TypeId,
    ) -> Result<Option<TypeId>, ProjectExecutionStop> {
        if single == TypeId::ANY
            || single == TypeId::UNKNOWN
            || single == TypeId::ERROR
            || single == TypeId::NEVER
        {
            return Ok(None);
        }
        let single_data = self.lookup(single);
        if matches!(&single_data, Some(TypeData::Union(_))) {
            return Ok(None);
        }
        let Some(TypeData::Union(list_id)) = self.lookup(existing) else {
            return Ok(None);
        };
        let members = self.type_list(list_id);
        let literal_primitive = match single_data {
            Some(TypeData::Literal(literal)) => Some(literal.primitive_type_id()),
            _ => None,
        };
        let mut has_literal_primitive = false;
        let mut has_single = false;
        for &member in members.iter() {
            tick_union_execution_work(work, 1)?;
            has_literal_primitive |= literal_primitive == Some(member);
            has_single |= member == single;
            if has_literal_primitive || has_single {
                break;
            }
        }
        Ok((has_literal_primitive || has_single).then_some(existing))
    }

    pub(crate) fn union3_with_execution_checkpoint(
        &self,
        first: TypeId,
        second: TypeId,
        third: TypeId,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_from_iter_with_work([first, second, third], work)
        })
    }

    pub(crate) fn union_preserve_members_with_execution_checkpoint(
        &self,
        members: Vec<TypeId>,
        checkpoint: &dyn ExecutionCheckpoint,
    ) -> Result<TypeId, ProjectExecutionStop> {
        self.with_union_execution_work(checkpoint, |work| {
            self.union_preserve_members_with_work(members, work)
        })
    }

    fn union_preserve_members_with_work(
        &self,
        members: Vec<TypeId>,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        let mut flat = self.collect_union_members_with_work(members, work)?;

        let mut has_any = false;
        let mut has_unknown = false;
        for &id in &flat {
            tick_union_execution_work(work, 1)?;
            if id == TypeId::ERROR {
                flush_union_execution_work(work)?;
                return Ok(TypeId::ERROR);
            }
            if id == TypeId::ANY {
                has_any = true;
            } else if id == TypeId::UNKNOWN {
                has_unknown = true;
            }
        }
        if has_any {
            flush_union_execution_work(work)?;
            return Ok(TypeId::ANY);
        }
        if has_unknown {
            flush_union_execution_work(work)?;
            return Ok(TypeId::UNKNOWN);
        }

        self.sort_union_members_with_work(&mut flat, work)?;
        self.dedup_union_members_with_work(&mut flat, work)?;
        let mut write = 0;
        for read in 0..flat.len() {
            tick_union_execution_work(work, 1)?;
            if flat[read] != TypeId::NEVER {
                flat[write] = flat[read];
                write += 1;
            }
        }
        flat.truncate(write);

        // Preserve distinct subtype members for narrowing while applying the
        // same universal non-strict nullish absorption used by ordinary union
        // construction.
        if let Some(collapsed) = self.reduce_and_collapse_nonstrict_with_work(&mut flat, work)? {
            flush_union_execution_work(work)?;
            return Ok(collapsed);
        }

        flush_union_execution_work(work)?;
        let list_id = self.intern_type_list_from_slice(&flat);
        Ok(self.intern(TypeData::Union(list_id)))
    }

    pub(crate) fn union_from_iter_with_work<I>(
        &self,
        members: I,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop>
    where
        I: IntoIterator<Item = TypeId>,
    {
        let flat = self.collect_union_members_with_work(members, work)?;
        match flat.len() {
            0 => {
                flush_union_execution_work(work)?;
                Ok(TypeId::NEVER)
            }
            1 => {
                flush_union_execution_work(work)?;
                Ok(flat[0])
            }
            _ => self.normalize_union_with_work(flat, work),
        }
    }

    fn union_literal_reduce_from_iter_with_work<I>(
        &self,
        members: I,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop>
    where
        I: IntoIterator<Item = TypeId>,
    {
        let flat = self.collect_union_members_with_work(members, work)?;
        match flat.len() {
            0 => {
                flush_union_execution_work(work)?;
                Ok(TypeId::NEVER)
            }
            1 => {
                flush_union_execution_work(work)?;
                Ok(flat[0])
            }
            _ => self.normalize_union_literal_only_with_work(flat, work),
        }
    }

    fn collect_union_members_with_work<I>(
        &self,
        members: I,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeListBuffer, ProjectExecutionStop>
    where
        I: IntoIterator<Item = TypeId>,
    {
        let mut flat: TypeListBuffer = SmallVec::new();
        for member in members {
            tick_union_execution_work(work, 1)?;
            if let Some(TypeData::Union(inner)) = self.lookup(member) {
                for &inner_member in self.type_list(inner).iter() {
                    tick_union_execution_work(work, 1)?;
                    flat.push(inner_member);
                }
            } else {
                flat.push(member);
            }
        }
        Ok(flat)
    }

    fn normalize_union_with_work(
        &self,
        mut flat: TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        tick_union_execution_work(work, flat.len() as u64)?;
        if let Some(collapsed) = self.reduce_and_collapse_nonstrict_with_work(&mut flat, work)? {
            flush_union_execution_work(work)?;
            return Ok(collapsed);
        }
        let literal_only = crate::intern::union_literal_default_enabled();
        if flat.len() > Self::UNION_NORMALIZE_CACHE_MAX_LEN {
            return self.normalize_union_uncached_with_work(flat, literal_only, work);
        }
        if let Some(hit) = self.union_normalize_cache.get(flat.as_slice()) {
            flush_union_execution_work(work)?;
            return Ok(*hit);
        }

        // Cache-key copying is linear in union width, so it shares the same
        // per-call work meter as collection and reduction.
        tick_union_execution_work(work, flat.len() as u64)?;
        let key: Box<[TypeId]> = flat.as_slice().into();
        let result = self.normalize_union_uncached_with_work(flat, literal_only, work)?;
        flush_union_execution_work(work)?;
        self.insert_union_normalize_cache(key, result);
        Ok(result)
    }

    fn normalize_union_uncached_with_work(
        &self,
        mut flat: TypeListBuffer,
        literal_only: bool,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        let preserve_callable_order =
            self.should_preserve_callable_union_order_with_work(&flat, work)?;
        if preserve_callable_order {
            let mut seen = FxHashSet::default();
            let mut write = 0;
            for read in 0..flat.len() {
                tick_union_execution_work(work, 1)?;
                let id = flat[read];
                if seen.insert(id) {
                    flat[write] = id;
                    write += 1;
                }
            }
            flat.truncate(write);
        } else {
            self.sort_union_members_with_work(&mut flat, work)?;
            let mut write = 0;
            for read in 0..flat.len() {
                tick_union_execution_work(work, 1)?;
                if write == 0 || flat[read] != flat[write - 1] {
                    flat[write] = flat[read];
                    write += 1;
                }
            }
            flat.truncate(write);
        }

        if let Some(collapsed) = self.apply_union_literal_ladder_with_work(&mut flat, true, work)? {
            flush_union_execution_work(work)?;
            return Ok(collapsed);
        }

        if flat.len() > 1000 {
            let mut has_object_types = false;
            for &id in flat.iter() {
                tick_union_execution_work(work, 1)?;
                has_object_types |= matches!(
                    self.lookup(id),
                    Some(
                        TypeData::Object(_)
                            | TypeData::ObjectWithIndex(_)
                            | TypeData::Intersection(_)
                    )
                );
            }
            if has_object_types {
                return self.normalize_union_literal_only_with_work(flat, work);
            }
        }

        let mut has_complex = false;
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            has_complex |= matches!(
                self.lookup(id),
                Some(TypeData::TypeParameter(_) | TypeData::Lazy(_))
            );
        }
        if !has_complex && !preserve_callable_order && !literal_only {
            self.reduce_union_subtypes_with_work(&mut flat, work)?;
        }

        if flat.is_empty() {
            flush_union_execution_work(work)?;
            return Ok(TypeId::NEVER);
        }
        if flat.len() == 1 {
            flush_union_execution_work(work)?;
            return Ok(flat[0]);
        }

        flush_union_execution_work(work)?;
        let list_id = self.intern_type_list_from_slice(&flat);
        Ok(self.intern(TypeData::Union(list_id)))
    }

    fn should_preserve_callable_union_order_with_work(
        &self,
        flat: &TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<bool, ProjectExecutionStop> {
        let mut callable_count = 0;
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            if id == TypeId::NULL || id == TypeId::UNDEFINED || id == TypeId::NEVER {
                continue;
            }
            match self.lookup(id) {
                Some(TypeData::Function(func_id)) => {
                    if !self.function_shape(func_id).type_params.is_empty() {
                        return Ok(false);
                    }
                    callable_count += 1;
                }
                Some(TypeData::Callable(callable_id)) => {
                    let shape = self.callable_shape(callable_id);
                    if shape.call_signatures.len() != 1
                        || !shape.construct_signatures.is_empty()
                        || !shape.call_signatures[0].type_params.is_empty()
                    {
                        return Ok(false);
                    }
                    callable_count += 1;
                }
                _ => return Ok(false),
            }
        }
        Ok(callable_count > 1)
    }

    fn normalize_union_literal_only_with_work(
        &self,
        mut flat: TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<TypeId, ProjectExecutionStop> {
        if let Some(collapsed) = self.reduce_and_collapse_nonstrict_with_work(&mut flat, work)? {
            flush_union_execution_work(work)?;
            return Ok(collapsed);
        }
        self.sort_union_members_with_work(&mut flat, work)?;
        let mut write = 0;
        for read in 0..flat.len() {
            tick_union_execution_work(work, 1)?;
            if write == 0 || flat[read] != flat[write - 1] {
                flat[write] = flat[read];
                write += 1;
            }
        }
        flat.truncate(write);

        let enriched = crate::intern::union_literal_default_enabled();
        if let Some(collapsed) =
            self.apply_union_literal_ladder_with_work(&mut flat, enriched, work)?
        {
            flush_union_execution_work(work)?;
            return Ok(collapsed);
        }

        flush_union_execution_work(work)?;
        let list_id = self.intern_type_list_from_slice(&flat);
        Ok(self.intern(TypeData::Union(list_id)))
    }

    fn apply_union_literal_ladder_with_work(
        &self,
        flat: &mut TypeListBuffer,
        enriched: bool,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<Option<TypeId>, ProjectExecutionStop> {
        let mut has_error = false;
        let mut has_any = false;
        let mut has_unknown = false;
        let mut has_never = false;
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            if id == TypeId::ERROR {
                has_error = true;
                break;
            }
            if id == TypeId::ANY {
                has_any = true;
            } else if id == TypeId::UNKNOWN {
                has_unknown = true;
            } else if id == TypeId::NEVER {
                has_never = true;
            }
        }
        if has_error {
            return Ok(Some(TypeId::ERROR));
        }
        if flat.is_empty() {
            return Ok(Some(TypeId::NEVER));
        }
        if flat.len() == 1 {
            return Ok(Some(flat[0]));
        }
        if has_any {
            return Ok(Some(TypeId::ANY));
        }
        if has_unknown {
            return Ok(Some(TypeId::UNKNOWN));
        }
        if has_never {
            let mut write = 0;
            for read in 0..flat.len() {
                tick_union_execution_work(work, 1)?;
                if flat[read] != TypeId::NEVER {
                    flat[write] = flat[read];
                    write += 1;
                }
            }
            flat.truncate(write);
        }
        if flat.is_empty() {
            return Ok(Some(TypeId::NEVER));
        }
        if flat.len() == 1 {
            return Ok(Some(flat[0]));
        }

        self.absorb_literals_into_primitives_with_work(flat, work)?;
        if enriched {
            self.merge_same_enum_parts_with_work(flat, work)?;
            self.absorb_intersections_with_union_constituents_with_work(flat, work)?;
        }
        if flat.is_empty() {
            Ok(Some(TypeId::NEVER))
        } else if flat.len() == 1 {
            Ok(Some(flat[0]))
        } else {
            Ok(None)
        }
    }

    fn absorb_intersections_with_union_constituents_with_work(
        &self,
        flat: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        if flat.len() <= 1 {
            return Ok(());
        }
        let mut present: FxHashSet<TypeId> = FxHashSet::default();
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            present.insert(id);
        }
        let mut write = 0;
        for read in 0..flat.len() {
            tick_union_execution_work(work, 1)?;
            let id = flat[read];
            let mut absorbed = false;
            if let Some(TypeData::Intersection(list_id)) = self.lookup(id) {
                for &part in self.type_list(list_id).iter() {
                    tick_union_execution_work(work, 1)?;
                    if present.contains(&part) {
                        absorbed = true;
                        break;
                    }
                }
            }
            if !absorbed {
                flat[write] = id;
                write += 1;
            }
        }
        flat.truncate(write);
        Ok(())
    }
}

#[cfg(test)]
mod sorted_vec_checkpoint_tests {
    use super::TypeInterner;
    use crate::types::{TypeData, TypeId};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};

    struct ContinueCheckpoint;

    impl ExecutionCheckpoint for ContinueCheckpoint {
        fn checkpoint(&self, _work_units: u64) -> Result<(), ProjectExecutionStop> {
            Ok(())
        }
    }

    struct StopAfterOneUnit(AtomicU64);

    impl ExecutionCheckpoint for StopAfterOneUnit {
        fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
            if self
                .0
                .fetch_add(work_units, Ordering::AcqRel)
                .saturating_add(work_units)
                > 1
            {
                Err(ProjectExecutionStop::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn sorted_vec_shared_path_preserves_strict_and_nonstrict_results() {
        for strict_null_checks in [true, false] {
            let interner = TypeInterner::new();
            interner.set_strict_null_checks(strict_null_checks);

            let sorted = vec![TypeId::NUMBER, TypeId::NULL];
            let ordinary = interner.union_from_sorted_vec(sorted.clone());
            if strict_null_checks {
                assert!(matches!(
                    interner.lookup(ordinary),
                    Some(TypeData::Union(_))
                ));
            } else {
                assert_eq!(ordinary, TypeId::NUMBER);
            }
            let checked = interner
                .union_from_sorted_vec_with_execution_checkpoint(sorted, &ContinueCheckpoint)
                .expect("continuing checkpoint preserves the ordinary result");
            assert_eq!(checked, ordinary);

            assert_eq!(
                interner.union_from_sorted_vec(Vec::new()),
                interner
                    .union_from_sorted_vec_with_execution_checkpoint(
                        Vec::new(),
                        &ContinueCheckpoint,
                    )
                    .expect("empty sorted input completes"),
            );
            for singleton in [TypeId::NULL, TypeId::UNDEFINED, TypeId::STRING] {
                let ordinary = interner.union_from_sorted_vec(vec![singleton]);
                let checked = interner
                    .union_from_sorted_vec_with_execution_checkpoint(
                        vec![singleton],
                        &ContinueCheckpoint,
                    )
                    .expect("singleton sorted input completes");
                assert_eq!(ordinary, singleton);
                assert_eq!(checked, ordinary);
            }
        }
    }

    #[test]
    fn sorted_vec_stop_flushes_before_interning_result() {
        let interner = TypeInterner::new();
        let before = interner.len();
        let checkpoint = StopAfterOneUnit(AtomicU64::new(0));

        assert_eq!(
            interner.union_from_sorted_vec_with_execution_checkpoint(
                vec![TypeId::NUMBER, TypeId::NULL],
                &checkpoint,
            ),
            Err(ProjectExecutionStop::Cancelled),
        );
        assert_eq!(interner.len(), before);
        assert!(!interner.is_union_too_complex());
    }
}
