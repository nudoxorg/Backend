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
            && self
                .stop_after_units
                .is_some_and(|limit| total > limit)
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
        checked.union_with_execution_checkpoint(
            vec![narrow, broad],
            &ContinueCheckpoint,
        ),
        Ok(narrow),
    );
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
