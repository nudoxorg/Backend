//! Work bounds of the variance walk: a shared subtree is measured once per
//! visit context, and a stopped execution unwinds the walk promptly.

use super::*;
use crate::intern::TypeInterner;
use crate::types::{FunctionShape, ParamInfo, PropertyInfo};
use std::sync::atomic::{AtomicU64, Ordering};
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop, enter_ambient_checkpoint};

fn type_param(interner: &TypeInterner, name: &str) -> (TypeParamInfo, TypeId) {
    let info = TypeParamInfo {
        name: interner.intern_string(name),
        constraint: None,
        default: None,
        is_const: false,
        origin: crate::types::TypeParamOrigin::User,
    };
    (info.clone(), interner.type_param(info))
}

/// `<U>(arg: shape) => void`: the generic signature binds `U`, which is the
/// context the completed-visit memo used to refuse.
fn generic_function_taking(interner: &TypeInterner, shape: TypeId) -> TypeId {
    let (u, _) = type_param(interner, "U");
    interner.function(FunctionShape {
        params: vec![ParamInfo {
            name: Some(interner.intern_string("arg")),
            type_id: shape,
            optional: false,
            rest: false,
        }],
        this_type: None,
        return_type: TypeId::VOID,
        type_params: vec![u],
        type_predicate: None,
        is_constructor: false,
        is_method: false,
    })
}

/// A diamond ladder: level `k + 1` holds level `k` in two properties, so the
/// top is one interned node reached through `2^depth` distinct paths.
fn diamond_ladder(interner: &TypeInterner, leaf: TypeId, depth: usize) -> TypeId {
    let left = interner.intern_string("left");
    let right = interner.intern_string("right");
    let mut level = interner.object(vec![PropertyInfo::new(interner.intern_string("leaf"), leaf)]);
    for _ in 0..depth {
        level = interner.object(vec![
            PropertyInfo::new(left, level),
            PropertyInfo::new(right, level),
        ]);
    }
    level
}

#[test]
fn a_shared_subtree_inside_a_generic_signature_is_measured_once() {
    let interner = TypeInterner::new();
    let (target, t) = type_param(&interner, "T");
    let ladder = diamond_ladder(&interner, t, 40);
    let function = generic_function_taking(&interner, ladder);

    let mut computer = VarianceComputer::new(&interner, &interner);
    let mut visitor = VarianceVisitor::for_type_parameter(&mut computer, target.name);
    visitor.visit_with_polarity(function, true);

    assert!(
        visitor.result.is_contravariant(),
        "T sits under the signature's parameter position"
    );
    // Forty-one distinct levels plus the signature: one visit each. Without the
    // memo the walk re-enters the ladder through all 2^40 paths and only the
    // iteration cap (100k per walk) ends it.
    assert!(
        visitor.guard.iterations() < 200,
        "the ladder was walked {} times",
        visitor.guard.iterations()
    );
    assert!(!visitor.guard.is_exceeded());
}

#[test]
fn the_memo_distinguishes_the_type_parameters_bound_around_a_subtree() {
    // `<T>(arg: { x: T }) => void` binds the target's own name: its `T` is a
    // different parameter and must not count as an occurrence of the outer `T`,
    // even though the same `{ x: T }` node is an occurrence outside the signature.
    let interner = TypeInterner::new();
    let (target, t) = type_param(&interner, "T");
    let holder = interner.object(vec![PropertyInfo::new(interner.intern_string("x"), t)]);
    let binds_t = interner.function(FunctionShape {
        params: vec![ParamInfo {
            name: Some(interner.intern_string("arg")),
            type_id: holder,
            optional: false,
            rest: false,
        }],
        this_type: None,
        return_type: TypeId::VOID,
        type_params: vec![target.clone()],
        type_predicate: None,
        is_constructor: false,
        is_method: false,
    });
    let outer = interner.object(vec![
        PropertyInfo::new(interner.intern_string("bound"), binds_t),
        PropertyInfo::new(interner.intern_string("free"), holder),
    ]);

    let mut computer = VarianceComputer::new(&interner, &interner);
    let variance = VarianceVisitor::for_type_parameter(&mut computer, target.name).compute(outer);

    assert!(
        variance.is_covariant(),
        "the free occurrence is covariant: {variance:?}"
    );
    assert!(
        !variance.is_contravariant(),
        "the occurrence under the signature that binds `T` must not be counted: {variance:?}"
    );
}

/// Stops after a fixed number of work units, like a spent file slice.
struct StopAfter {
    spent: AtomicU64,
    limit: u64,
}

impl ExecutionCheckpoint for StopAfter {
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        let spent = self.spent.fetch_add(work_units, Ordering::Relaxed) + work_units;
        if spent > self.limit {
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_spent_slice_stops_a_wide_variance_walk_long_before_its_own_iteration_cap() {
    let interner = TypeInterner::new();
    let (target, t) = type_param(&interner, "T");
    // 50k distinct members: nothing to memoize, so only a poll can end the walk
    // before the per-walk cap of 100k visits.
    let members = (0..50_000)
        .map(|index| {
            interner.object(vec![
                PropertyInfo::new(interner.intern_string(&format!("p{index}")), t),
            ])
        })
        .collect::<Vec<_>>();
    let wide = interner.union(members);

    let slice = StopAfter {
        spent: AtomicU64::new(0),
        limit: 50,
    };
    let mut computer = VarianceComputer::new(&interner, &interner);
    let mut visitor = VarianceVisitor::for_type_parameter(&mut computer, target.name);
    {
        let _ambient = enter_ambient_checkpoint(&slice);
        visitor.visit_with_polarity(wide, true);
        assert!(tsz_common::ambient_stopped());
    }

    // 50 units at one unit per poll batch, plus one batch of slack.
    let polls_allowed = 50 * u64::from(tsz_common::AMBIENT_POLLS_PER_UNIT) + 128;
    assert!(
        u64::from(visitor.guard.iterations()) <= polls_allowed,
        "the walk ran {} visits after its slice was spent",
        visitor.guard.iterations()
    );
    assert!(visitor.guard.is_exceeded(), "a stopped walk reads as an exhausted budget");
}

#[test]
fn an_unstopped_wide_walk_is_unchanged_without_an_ambient_checkpoint() {
    let interner = TypeInterner::new();
    let (target, t) = type_param(&interner, "T");
    let members = (0..64)
        .map(|index| {
            interner.object(vec![
                PropertyInfo::new(interner.intern_string(&format!("p{index}")), t),
            ])
        })
        .collect::<Vec<_>>();
    let wide = interner.union(members);
    let mut computer = VarianceComputer::new(&interner, &interner);
    let variance = VarianceVisitor::for_type_parameter(&mut computer, target.name).compute(wide);
    assert!(variance.is_covariant());
}

/// One generic definition `Box<T> = { value: T }` that counts its body resolutions.
struct OneGeneric {
    def: DefId,
    params: Vec<TypeParamInfo>,
    body: TypeId,
    resolutions: std::cell::Cell<usize>,
}

impl crate::def::resolver::TypeResolver for OneGeneric {
    fn resolve_ref(
        &self,
        _symbol: crate::types::SymbolRef,
        _interner: &dyn crate::construction::TypeDatabase,
    ) -> Option<TypeId> {
        None
    }

    fn resolve_lazy(
        &self,
        def_id: DefId,
        _interner: &dyn crate::construction::TypeDatabase,
    ) -> Option<TypeId> {
        (def_id == self.def).then(|| {
            self.resolutions.set(self.resolutions.get() + 1);
            self.body
        })
    }

    fn get_lazy_type_params(&self, def_id: DefId) -> Option<Vec<TypeParamInfo>> {
        (def_id == self.def).then(|| self.params.clone())
    }
}

#[test]
fn effective_masks_are_shared_per_mode_and_replayed_without_walking_the_body_again() {
    let interner = TypeInterner::new();
    let (t_info, t) = type_param(&interner, "T");
    let resolver = OneGeneric {
        def: DefId(88_001),
        params: vec![t_info],
        body: interner.object(vec![PropertyInfo::new(interner.intern_string("value"), t)]),
        resolutions: std::cell::Cell::new(0),
    };
    let query = |strict: bool| {
        compute_effective_type_param_variances_with_resolver_cached(
            &interner, &resolver, None, resolver.def, strict, false,
        )
        .expect("the generic resolves")
    };

    let strict = query(true);
    assert!(strict.variances[0].is_covariant());
    assert_eq!(resolver.resolutions.get(), 1);
    let (strict_tag, loose_tag) = (
        VarianceMode::Effective {
            strict_function_types: true,
            disable_method_bivariance: false,
        }
        .store_tag(),
        VarianceMode::Effective {
            strict_function_types: false,
            disable_method_bivariance: false,
        }
        .store_tag(),
    );
    assert_ne!(strict_tag, loose_tag, "options that change the mask key it apart");
    assert!(interner.shared_def_variance(resolver.def, strict_tag.expect("shared")).is_some());
    assert!(
        interner.shared_def_variance(resolver.def, loose_tag.expect("shared")).is_none(),
        "a mask computed under one option set is never served for another"
    );
    assert!(
        interner.shared_def_variance(resolver.def, 0).is_none(),
        "an effective mask never occupies the declared slot"
    );

    // A fresh computer (a new relation query, even another file's checker) replays
    // the shared mask instead of resolving and walking the body again.
    let again = query(true);
    assert_eq!(again.variances, strict.variances);
    assert_eq!(resolver.resolutions.get(), 1, "the shared mask was not replayed");

    let loose = query(false);
    assert_eq!(loose.variances, strict.variances);
    assert_eq!(resolver.resolutions.get(), 2, "the other option set computes its own mask");
}
