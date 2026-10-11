use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};

#[derive(Default)]
struct ControlledCheckpoint {
    armed: AtomicBool,
    stop_after_units: Option<u64>,
    charged_units: AtomicU64,
}

impl ControlledCheckpoint {
    fn armed_immediately() -> Self {
        Self {
            armed: AtomicBool::new(true),
            stop_after_units: Some(0),
            charged_units: AtomicU64::new(0),
        }
    }

    fn stop_after(units: u64) -> Self {
        Self {
            armed: AtomicBool::new(true),
            stop_after_units: Some(units),
            charged_units: AtomicU64::new(0),
        }
    }
}

impl ExecutionCheckpoint for ControlledCheckpoint {
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        let total = self
            .charged_units
            .fetch_add(work_units, Ordering::AcqRel)
            .saturating_add(work_units);
        if self.armed.load(Ordering::Acquire)
            && self.stop_after_units.is_some_and(|limit| total > limit)
        {
            Err(ProjectExecutionStop::Cancelled)
        } else {
            Ok(())
        }
    }
}

struct ContinueCheckpoint;

impl ExecutionCheckpoint for ContinueCheckpoint {
    fn checkpoint(&self, _work_units: u64) -> Result<(), ProjectExecutionStop> {
        Ok(())
    }
}

fn property(interner: &TypeInterner, name: &str, ty: TypeId) -> PropertyInfo {
    PropertyInfo {
        name: interner.intern_string(name),
        type_id: ty,
        ..PropertyInfo::default()
    }
}

#[test]
fn checked_union_matches_uncancelled_union_reduction() {
    let ordinary = TypeInterner::new();
    let narrow = ordinary.object(vec![property(&ordinary, "a", TypeId::NUMBER)]);
    let broad = ordinary.object(vec![
        property(&ordinary, "a", TypeId::NUMBER),
        property(&ordinary, "b", TypeId::NUMBER),
    ]);
    assert_eq!(ordinary.union(vec![narrow, broad]), narrow);

    let checked = TypeInterner::new();
    let narrow = checked.object(vec![property(&checked, "a", TypeId::NUMBER)]);
    let broad = checked.object(vec![
        property(&checked, "a", TypeId::NUMBER),
        property(&checked, "b", TypeId::NUMBER),
    ]);
    assert_eq!(
        checked.union_with_execution_checkpoint(vec![narrow, broad], &ContinueCheckpoint,),
        Ok(narrow),
    );
}

#[test]
fn checked_preserving_union_matches_uncancelled_members_and_collapse() {
    let interner = TypeInterner::new();
    let nested = interner.union_preserve_members(vec![TypeId::STRING, TypeId::NUMBER]);
    let members = vec![TypeId::NEVER, nested, TypeId::STRING, TypeId::NUMBER];
    let expected = interner.union_preserve_members(members.clone());
    assert_eq!(
        interner.union_preserve_members_with_execution_checkpoint(members, &ContinueCheckpoint),
        Ok(expected),
    );
    assert_eq!(
        interner.union_preserve_members_with_execution_checkpoint(
            vec![TypeId::NEVER],
            &ContinueCheckpoint,
        ),
        Ok(TypeId::NEVER),
    );
}

#[test]
fn stopped_preserving_union_does_not_intern_partial_result() {
    let interner = TypeInterner::new();
    let members: Vec<TypeId> = (0..128)
        .map(|index| interner.literal_string(&format!("preserved_{index:03}")))
        .collect();
    let type_count_before = interner.len();
    let checkpoint = ControlledCheckpoint::stop_after(320);

    assert_eq!(
        interner.union_preserve_members_with_execution_checkpoint(members, &checkpoint),
        Err(ProjectExecutionStop::Cancelled),
    );
    let charged = checkpoint.charged_units.load(Ordering::Acquire);
    assert!(charged > 320 && charged <= 384, "charged {charged} units");
    assert_eq!(interner.len(), type_count_before);
    assert!(!interner.is_union_too_complex());
}

#[test]
fn stopped_union_does_not_intern_or_memoize_partial_result() {
    let interner = TypeInterner::new();
    let left = interner.object(vec![property(&interner, "left", TypeId::NUMBER)]);
    let right = interner.object(vec![property(&interner, "right", TypeId::NUMBER)]);
    let type_count_before = interner.len();
    let cache_entries_before = interner
        .type_predicate_cache_statistics()
        .union_normalize_cache_entries;
    let checkpoint = ControlledCheckpoint::armed_immediately();

    assert_eq!(
        interner.union_with_execution_checkpoint(vec![left, right], &checkpoint),
        Err(ProjectExecutionStop::Cancelled),
    );
    assert_eq!(interner.len(), type_count_before);
    assert_eq!(
        interner
            .type_predicate_cache_statistics()
            .union_normalize_cache_entries,
        cache_entries_before,
    );
    assert!(!interner.is_union_too_complex());
}

#[test]
fn wide_union_reduction_polls_inside_partition_property_scan_before_ts2590() {
    let interner = TypeInterner::new();
    let common_properties: Vec<PropertyInfo> = (0..120)
        .map(|index| property(&interner, &format!("shared_{index:03}"), TypeId::NUMBER))
        .collect();
    let mut members = Vec::with_capacity(1000);
    for index in 0..999 {
        let mut properties = common_properties.clone();
        properties.push(property(
            &interner,
            &format!("unique_{index:04}"),
            TypeId::BOOLEAN,
        ));
        members.push(interner.object(properties));
    }
    members.push(TypeId::NUMBER);
    assert_eq!(members.len(), 1000);
    assert_eq!(
        (members.len() as u64) * (members.len() as u64 - 1),
        999_000,
        "the fixture stays just below the existing TS2590 pairwise threshold"
    );

    let checkpoint = ControlledCheckpoint::stop_after(2050);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut flat: TypeListBuffer = members.into();
    let mut metered_work = Some(&mut work);
    let result = interner.reduce_union_subtypes_with_work(&mut flat, &mut metered_work);

    assert_eq!(result, Err(ProjectExecutionStop::Cancelled));
    let charged = checkpoint.charged_units.load(Ordering::Acquire);
    assert!(charged > 2050 && charged < 2200, "charged {charged} units");
    assert!(
        !interner.is_union_too_complex(),
        "a cooperative stop must not become a TS2590 diagnostic"
    );
}

#[test]
fn shallow_object_property_scan_observes_stop_with_bounded_polling() {
    let interner = TypeInterner::new();
    let source = interner.object(
        (0..127)
            .map(|index| property(&interner, &format!("p_{index:03}"), TypeId::NUMBER))
            .collect(),
    );
    let target = interner.object(
        (0..128)
            .map(|index| property(&interner, &format!("p_{index:03}"), TypeId::NUMBER))
            .collect(),
    );
    let checkpoint = ControlledCheckpoint::armed_immediately();
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);

    assert_eq!(
        interner.is_subtype_shallow_with_work(source, target, &mut metered_work),
        Err(ProjectExecutionStop::Cancelled),
    );
    assert_eq!(checkpoint.charged_units.load(Ordering::Acquire), 64);
}

#[test]
fn nested_function_subtyping_uses_a_metered_worklist() {
    let interner = TypeInterner::new();
    let mut source = TypeId::NUMBER;
    let mut target = TypeId::ANY;
    for _ in 0..20_000 {
        source = interner.function(crate::types::FunctionShape::new(Vec::new(), source));
        target = interner.function(crate::types::FunctionShape::new(Vec::new(), target));
    }

    assert!(interner.is_subtype_shallow(source, target));

    let type_count_before_stop = interner.len();
    let checkpoint = ControlledCheckpoint::stop_after(256);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.is_subtype_shallow_with_work(source, target, &mut metered_work),
        Err(ProjectExecutionStop::Cancelled),
    );
    let charged = checkpoint.charged_units.load(Ordering::Acquire);
    assert!(charged > 256 && charged <= 320, "charged {charged} units");
    assert_eq!(interner.len(), type_count_before_stop);

    // Function parameters reverse the comparison direction. Optional
    // parameters use the same iterative path while preserving the optionality
    // gate; even nesting restores the number <: any relation at the leaf.
    let parameter_name = interner.intern_string("nested");
    let mut source_parameter = TypeId::NUMBER;
    let mut target_parameter = TypeId::ANY;
    for _ in 0..20_000 {
        source_parameter = interner.function(crate::types::FunctionShape::new(
            vec![crate::types::ParamInfo::optional(
                parameter_name,
                source_parameter,
            )],
            TypeId::NUMBER,
        ));
        target_parameter = interner.function(crate::types::FunctionShape::new(
            vec![crate::types::ParamInfo::optional(
                parameter_name,
                target_parameter,
            )],
            TypeId::NUMBER,
        ));
    }
    assert!(interner.is_subtype_shallow(source_parameter, target_parameter));
    let continue_checkpoint = ContinueCheckpoint;
    let mut work = UnionExecutionWork::new(&continue_checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.is_subtype_shallow_with_work(
            source_parameter,
            target_parameter,
            &mut metered_work,
        ),
        Ok(true),
    );

    let type_count_before_stop = interner.len();
    let checkpoint = ControlledCheckpoint::stop_after(256);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.is_subtype_shallow_with_work(
            source_parameter,
            target_parameter,
            &mut metered_work
        ),
        Err(ProjectExecutionStop::Cancelled),
    );
    let charged = checkpoint.charged_units.load(Ordering::Acquire);
    assert!(charged > 256 && charged <= 320, "charged {charged} units");
    assert_eq!(interner.len(), type_count_before_stop);
}

#[test]
fn literal_source_scans_wide_target_union_with_cancellable_work() {
    let interner = TypeInterner::new();
    let source = interner.literal_string("match-last");
    let mut members: Vec<TypeId> = (0..100)
        .map(|index| interner.literal_number(f64::from(index)))
        .collect();
    members.push(source);
    let target = interner.union_preserve_members(members);
    let Some(TypeData::Union(list_id)) = interner.lookup(target) else {
        panic!("wide fixture should remain a union");
    };
    assert_eq!(interner.type_list(list_id).len(), 101);
    assert_eq!(interner.type_list(list_id).last(), Some(&source));

    // The pre-unification ordinary and checked branches both scanned every
    // target arm here; the exact string literal is deliberately last.
    assert!(interner.is_subtype_shallow(source, target));
    let continue_checkpoint = ContinueCheckpoint;
    let mut work = UnionExecutionWork::new(&continue_checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.is_subtype_shallow_with_work(source, target, &mut metered_work),
        Ok(true),
    );

    let checkpoint = ControlledCheckpoint::stop_after(63);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.is_subtype_shallow_with_work(source, target, &mut metered_work),
        Err(ProjectExecutionStop::Cancelled),
    );
    assert_eq!(checkpoint.charged_units.load(Ordering::Acquire), 64);
}

#[test]
fn discriminant_candidate_scan_observes_stop_after_property_counting() {
    let interner = TypeInterner::new();
    let common_properties: Vec<PropertyInfo> = (0..120)
        .map(|index| property(&interner, &format!("shared_{index:03}"), TypeId::NUMBER))
        .collect();
    let mut members = Vec::with_capacity(999);
    for index in 0..999 {
        let mut properties = common_properties.clone();
        properties.push(property(
            &interner,
            &format!("unique_{index:04}"),
            TypeId::BOOLEAN,
        ));
        members.push(interner.object(properties));
    }

    // Building prop_counts charges one unit per object and property. The next
    // checkpoint occurs during the distinct-property candidate selection pass.
    let property_count_work = 999 + 999 * 121;
    let checkpoint = ControlledCheckpoint::stop_after(property_count_work as u64);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.try_partition_union_reduction_with_work(&members, &mut metered_work),
        Err(ProjectExecutionStop::Cancelled),
    );
    let charged = checkpoint.charged_units.load(Ordering::Acquire);
    assert!(
        charged > property_count_work as u64 && charged <= property_count_work as u64 + 64,
        "candidate scan should stop at the next bounded poll, charged {charged}"
    );
}

#[test]
fn discriminant_partition_copy_is_metered_after_all_groups_reduce() {
    let interner = TypeInterner::new();
    let members: Vec<TypeId> = (0..80)
        .map(|index| {
            let tag = interner.literal_number(f64::from(index));
            interner.object(vec![property(&interner, "tag", tag)])
        })
        .collect();

    let continue_checkpoint = ContinueCheckpoint;
    let mut work = UnionExecutionWork::new(&continue_checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.try_partition_union_reduction_with_work(&members, &mut metered_work),
        Ok(None),
        "disjoint singleton groups preserve the original union"
    );

    // Counting, candidate selection, partitioning, and group iteration charge
    // 480 units. The next bounded poll occurs after 32 of the 80 reduced
    // members are copied into the result; the copy must not be an unmetered
    // tail.
    let checkpoint = ControlledCheckpoint::stop_after(480);
    let mut work = UnionExecutionWork::new(&checkpoint).expect("initial checkpoint");
    let mut metered_work = Some(&mut work);
    assert_eq!(
        interner.try_partition_union_reduction_with_work(&members, &mut metered_work),
        Err(ProjectExecutionStop::Cancelled),
    );
    assert_eq!(checkpoint.charged_units.load(Ordering::Acquire), 512);
}

#[test]
fn mid_normalization_stop_does_not_publish_union_or_complexity_memo() {
    let interner = TypeInterner::new();
    let members: Vec<TypeId> = (0..128)
        .map(|index| interner.literal_string(&format!("member_{index:03}")))
        .collect();
    let type_count_before = interner.len();
    let cache_entries_before = interner
        .type_predicate_cache_statistics()
        .union_normalize_cache_entries;
    let checkpoint = ControlledCheckpoint::stop_after(128);

    assert_eq!(
        interner.union_with_execution_checkpoint(members, &checkpoint),
        Err(ProjectExecutionStop::Cancelled),
    );
    assert!(checkpoint.charged_units.load(Ordering::Acquire) > 128);
    assert_eq!(interner.len(), type_count_before);
    assert_eq!(
        interner
            .type_predicate_cache_statistics()
            .union_normalize_cache_entries,
        cache_entries_before,
    );
    assert!(!interner.is_union_too_complex());
}
