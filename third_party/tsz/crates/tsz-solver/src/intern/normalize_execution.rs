// Checkpoint-aware union reduction implementation included by normalize.rs.

impl TypeInterner {
    pub(crate) fn reduce_union_subtypes_with_work(
        &self,
        flat: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        let len = flat.len();
        if len <= 1 {
            return Ok(());
        }

        let mut deferred_count = 0;
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            if self.is_shallow_inert_member(id) {
                deferred_count += 1;
            }
        }
        if deferred_count > 0 {
            if deferred_count == len {
                return Ok(());
            }
            let mut deferred: TypeListBuffer = SmallVec::new();
            let mut reducible: TypeListBuffer = SmallVec::new();
            for &id in flat.iter() {
                tick_union_execution_work(work, 1)?;
                if self.is_shallow_inert_member(id) {
                    deferred.push(id);
                } else {
                    reducible.push(id);
                }
            }
            self.reduce_union_subtypes_with_work(&mut reducible, work)?;
            for id in deferred {
                tick_union_execution_work(work, 1)?;
                reducible.push(id);
            }
            self.sort_union_members_with_work(&mut reducible, work)?;
            self.dedup_union_members_with_work(&mut reducible, work)?;
            *flat = reducible;
            return Ok(());
        }

        let len = flat.len();

        // Mirror the no-budget fast path that avoids treating identity-only
        // unions as TS2590 candidates. Template absorption is bounded by the
        // same 8-span / 128-byte shallow matcher used by pairwise comparison.
        let mut has_primitive = false;
        let mut string_literals = Vec::new();
        let mut templates = Vec::new();
        let mut all_non_reducible = true;
        for (index, &id) in flat.iter().enumerate() {
            tick_union_execution_work(work, 1)?;
            if self.is_identity_comparable_type(id) {
                if matches!(
                    id,
                    TypeId::STRING
                        | TypeId::NUMBER
                        | TypeId::BOOLEAN
                        | TypeId::BIGINT
                        | TypeId::SYMBOL
                ) {
                    has_primitive = true;
                }
                if let Some(TypeData::Literal(LiteralValue::String(atom))) = self.lookup(id) {
                    string_literals.push((index, atom));
                }
                continue;
            }
            match self.lookup(id) {
                Some(TypeData::TemplateLiteral(template_id)) => templates.push(template_id),
                Some(
                    TypeData::Array(_)
                    | TypeData::Tuple(_)
                    | TypeData::Object(_)
                    | TypeData::ObjectWithIndex(_)
                    | TypeData::Enum(_, _)
                    | TypeData::Lazy(_)
                    | TypeData::Application(_)
                    | TypeData::Callable(_)
                    | TypeData::Literal(_),
                ) => {}
                _ => {
                    all_non_reducible = false;
                    break;
                }
            }
        }
        if all_non_reducible && !has_primitive {
            if !templates.is_empty() && !string_literals.is_empty() {
                self.remove_string_literals_matched_by_templates_with_work(
                    flat,
                    &string_literals,
                    &templates,
                    work,
                )?;
            }
            return Ok(());
        }

        let pairwise = (len as u64) * (len as u64 - 1);
        tsz_common::perf_counters::record_union_subtype_reduction(len as u64, pairwise);
        tracing::trace!(len, "reduce_union_subtypes: entry");
        if pairwise >= Self::UNION_SUBTYPE_PAIRWISE_LIMIT {
            // The stop must win over TS2590 if cancellation/deadline arrived
            // during the work already performed by the normalizer.
            flush_union_execution_work(work)?;
            self.set_union_too_complex();
            return Ok(());
        }
        if len > 16
            && let Some(partitioned) = self.try_partition_union_reduction_with_work(flat, work)?
        {
            *flat = partitioned;
            return Ok(());
        }
        self.reduce_union_subtypes_sized_with_work(flat, work)
    }

    fn reduce_union_subtypes_sized_with_work(
        &self,
        flat: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        if flat.len() <= 64 {
            self.reduce_union_subtypes_quadratic_with_work(flat, work)
        } else {
            self.reduce_union_subtypes_large_row_with_work(flat, work)
        }
    }

    fn reduce_union_subtypes_quadratic_with_work(
        &self,
        flat: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        let len = flat.len();
        if len <= 1 {
            return Ok(());
        }
        let mut keep = if len == 64 {
            u64::MAX
        } else {
            (1u64 << len) - 1
        };
        let kinds: SmallVec<[ShallowReduceKind; 64]> = flat
            .iter()
            .map(|&id| self.shallow_reduce_kind(id))
            .collect();
        let shallow_checks = tsz_common::perf_counters::enabled_fast()
            .then(|| &tsz_common::perf_counters::counters().union_subtype_reduction_shallow_checks);
        for i in 0..len {
            tick_union_execution_work(work, 1)?;
            if keep & (1u64 << i) == 0 {
                continue;
            }
            for j in 0..len {
                tick_union_execution_work(work, 1)?;
                if i == j || keep & (1u64 << j) == 0 {
                    continue;
                }
                if !ShallowReduceKind::may_relate(kinds[i], kinds[j]) {
                    debug_assert!(
                        !self.is_subtype_shallow_with_work(flat[i], flat[j], work)?,
                        "shallow_reduce_kind skipped a relating pair: {:?} <: {:?}",
                        kinds[i],
                        kinds[j],
                    );
                    continue;
                }
                if let Some(counter) = shallow_checks {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if self.is_subtype_shallow_with_work(flat[i], flat[j], work)? {
                    keep &= !(1u64 << i);
                    break;
                }
            }
        }
        let mut write = 0;
        for read in 0..len {
            tick_union_execution_work(work, 1)?;
            if keep & (1u64 << read) != 0 {
                flat[write] = flat[read];
                write += 1;
            }
        }
        flat.truncate(write);
        Ok(())
    }

    pub(crate) fn dedup_union_members_with_work(
        &self,
        members: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        if members.len() < 2 {
            return Ok(());
        }
        let mut write = 1;
        for read in 1..members.len() {
            tick_union_execution_work(work, 1)?;
            if members[read] != members[write - 1] {
                members[write] = members[read];
                write += 1;
            }
        }
        members.truncate(write);
        Ok(())
    }

    fn reduce_union_subtypes_large_row_with_work(
        &self,
        flat: &mut TypeListBuffer,
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<(), ProjectExecutionStop> {
        let len = flat.len();
        if len <= 1 {
            return Ok(());
        }
        let mut keep = Vec::with_capacity(len);
        for _ in 0..len {
            tick_union_execution_work(work, 1)?;
            keep.push(true);
        }
        let mut kinds = Vec::with_capacity(len);
        for &id in flat.iter() {
            tick_union_execution_work(work, 1)?;
            kinds.push(self.shallow_reduce_kind(id));
        }
        let shallow_checks = tsz_common::perf_counters::enabled_fast()
            .then(|| &tsz_common::perf_counters::counters().union_subtype_reduction_shallow_checks);
        for i in 0..len {
            tick_union_execution_work(work, 1)?;
            if !keep[i] {
                continue;
            }
            for j in 0..len {
                tick_union_execution_work(work, 1)?;
                if i == j || !keep[j] {
                    continue;
                }
                if !ShallowReduceKind::may_relate(kinds[i], kinds[j]) {
                    debug_assert!(
                        !self.is_subtype_shallow_with_work(flat[i], flat[j], work)?,
                        "shallow_reduce_kind skipped a relating pair: {:?} <: {:?}",
                        kinds[i],
                        kinds[j],
                    );
                    continue;
                }
                if let Some(counter) = shallow_checks {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if self.is_subtype_shallow_with_work(flat[i], flat[j], work)? {
                    keep[i] = false;
                    break;
                }
            }
        }
        let mut write = 0;
        for read in 0..len {
            tick_union_execution_work(work, 1)?;
            if keep[read] {
                flat[write] = flat[read];
                write += 1;
            }
        }
        flat.truncate(write);
        Ok(())
    }

    pub(crate) fn try_partition_union_reduction_with_work(
        &self,
        members: &[TypeId],
        work: &mut Option<&mut UnionExecutionWork<'_>>,
    ) -> Result<Option<TypeListBuffer>, ProjectExecutionStop> {
        let mut prop_counts: FxHashMap<Atom, usize> = FxHashMap::default();
        let mut object_count = 0usize;
        for &member in members {
            tick_union_execution_work(work, 1)?;
            if let Some(shape_id) = crate::visitor::object_shape_id(self, member)
                .or_else(|| crate::visitor::object_with_index_shape_id(self, member))
            {
                object_count += 1;
                for property in &self.object_shape(shape_id).properties {
                    tick_union_execution_work(work, 1)?;
                    *prop_counts.entry(property.name).or_insert(0) += 1;
                }
            }
        }
        if object_count < 8 {
            return Ok(None);
        }
        let mut discriminant = None;
        let mut highest_count = 0;
        for (name, count) in prop_counts {
            tick_union_execution_work(work, 1)?;
            if count >= object_count / 2 && (discriminant.is_none() || count >= highest_count) {
                // `Iterator::max_by_key` keeps the last item on ties; retain
                // that choice while polling every candidate in the map scan.
                discriminant = Some(name);
                highest_count = count;
            }
        }
        let Some(discriminant) = discriminant else {
            return Ok(None);
        };

        let mut partitions: FxHashMap<TypeId, Vec<TypeId>> = FxHashMap::default();
        let mut fallback = Vec::new();
        for &member in members {
            tick_union_execution_work(work, 1)?;
            let shape_id = crate::visitor::object_shape_id(self, member)
                .or_else(|| crate::visitor::object_with_index_shape_id(self, member));
            let value = if let Some(shape_id) = shape_id {
                let shape = self.object_shape(shape_id);
                let mut found = None;
                for property in &shape.properties {
                    tick_union_execution_work(work, 1)?;
                    if property.name == discriminant {
                        found = Some(property.type_id);
                        break;
                    }
                }
                found
            } else {
                None
            };
            if let Some(value) = value {
                partitions.entry(value).or_default().push(member);
            } else {
                fallback.push(member);
            }
        }

        let mut result: TypeListBuffer = SmallVec::new();
        for group in partitions.into_values() {
            tick_union_execution_work(work, 1)?;
            let mut group = TypeListBuffer::from_vec(group);
            self.reduce_union_subtypes_sized_with_work(&mut group, work)?;
            for member in group {
                tick_union_execution_work(work, 1)?;
                result.push(member);
            }
        }
        if !fallback.is_empty() {
            let mut fallback = TypeListBuffer::from_vec(fallback);
            self.reduce_union_subtypes_sized_with_work(&mut fallback, work)?;
            for member in fallback {
                tick_union_execution_work(work, 1)?;
                result.push(member);
            }
        }
        if result.len() < members.len() {
            self.reduce_union_subtypes_sized_with_work(&mut result, work)?;
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }
}
