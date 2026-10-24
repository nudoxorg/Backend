//! Production-path protocol controls. This worker is a fixture, never a pretrained-model proof.
use super::*;
use crate::builtin::query::{LocalEmbeddingTreatment, LocalQuery, LocalQueryRecipeContract};
use backend_engine::application::EmbeddingRuntimeProvision;
#[cfg(unix)]
use backend_engine::application::{
    EmbeddingRequirement, EmbeddingRuntimeInstall, EmbeddingRuntimeLimits,
    install_embedding_runtime,
};
#[cfg(unix)]
use std::num::{NonZeroU16, NonZeroU32};
#[cfg(unix)]
use std::sync::{Arc, atomic::AtomicBool};
#[cfg(unix)]
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn local_semantic_complete_projection_admission_uses_configured_credits_not_project_caps()
-> TestResult {
    // Both historical ceilings are exceeded; this exercises the exact admission
    // calculation used by production without allocating that artificial corpus.
    let count = 131_073;
    let envelope = ProjectionEnvelope::new(count, 1024)?;
    assert!(count > 65_536);
    assert!(envelope.limits.max_total_payload_bytes > 512 * 1024 * 1024);
    assert_eq!(envelope.limits.max_candidates, count);
    assert_eq!(
        envelope.limits.max_total_payload_bytes,
        count * (1024 * 4 + 4)
    );
    let budget = backend_compile::EmbeddingMemoryBudget::new(envelope.credits);
    let held = budget.reserve(17)?;
    assert_eq!(
        budget.reserve(envelope.credits).err(),
        Some(EmbeddingMemoryError {
            requested: envelope.credits,
            available: envelope.credits - 17,
        })
    );
    drop(held);
    let admitted = budget.reserve(envelope.credits)?;
    assert_eq!(budget.used(), envelope.credits);
    drop(admitted);
    assert_eq!(budget.used(), 0);
    let refused = ProjectionEnvelope::new(usize::MAX, 384);
    assert!(matches!(refused, Err(LocalSemanticError::Representation {
        allocation: ProjectionAllocation::EntryDescriptors,
        required,
        maximum,
    }) if required > maximum && maximum == isize::MAX as u128));
    assert!(
        !LocalSemanticError::CorpusEnvelope
            .setup_instruction()
            .contains("Reduce")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_alternating_warm_queries_retain_exact_answers_and_credits() -> TestResult {
    let (coordinator, library, view) = selected_many(30, false)?;
    let fixture = fixture_with_mode(4096, "plain")?;
    let runtime = fixture
        .provision
        .runtime()
        .ok_or("activated protocol fixture")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let first = backend_engine::Query::new(
        "first intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let second = backend_engine::Query::new(
        "second intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let (first_page, first_status) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &first,
        Arc::clone(&flag),
    )?;
    let (second_page, second_status) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &second,
        Arc::clone(&flag),
    )?;
    assert_eq!(first_status, SemanticSearchStatus::Available);
    assert_eq!(second_status, SemanticSearchStatus::Available);
    for text in ["third intent", "fourth intent", "fifth intent"] {
        let request = backend_engine::Query::new(
            text,
            view.root(),
            backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
        );
        assert_eq!(
            settle_page(
                &coordinator,
                &library,
                &mut provider,
                &request,
                Arc::clone(&flag)
            )?
            .1,
            SemanticSearchStatus::Available
        );
    }
    let log = std::fs::read_to_string(&fixture.log)?;
    let held = runtime.memory_budget().used();
    for request in [&first, &second, &first, &second] {
        let (page, status) = super::super::search_page_with_cancellation(
            &coordinator,
            &library,
            &mut provider,
            crate::builtin::admitted_coverage()?,
            request,
            Arc::clone(&flag),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            status,
            SemanticSearchStatus::Available,
            "warm alternating query never starts another producer wait"
        );
        let expected = if request.text() == first.text() {
            &first_page
        } else {
            &second_page
        };
        assert_eq!(page.root.rows(), expected.root.rows());
        assert!(provider.local_preparation().is_none());
        assert_eq!(
            runtime.memory_budget().used(),
            held,
            "temporary output copy charge is released after projection"
        );
    }
    assert_eq!(std::fs::read_to_string(&fixture.log)?, log);
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - held)?;
    let (_, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &first,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        status,
        SemanticSearchStatus::Available,
        "optional copy/cache leases yield to the exact owed answer under pressure"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        log,
        "credit pressure does not rescore an already complete answer"
    );
    drop(pressure);
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &mut provider {
        owner.set_completion_policy(super::super::QueryCompletionPolicy { maximum_bytes: 1 });
    }
    assert!(
        runtime.memory_budget().used() < held,
        "evicted completion releases its query/output leases"
    );
    let (_, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &first,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        status,
        SemanticSearchStatus::Available,
        "most recently used exact answer survived byte-target eviction"
    );
    flag.store(true, Ordering::Release);
    provider.poll_local(coordinator.workspace_root(), view.root());
    assert!(provider.local_preparation().is_none());
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        log,
        "retirement never reruns inference"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_transient_query_failure_is_one_shot_and_preserves_selected_projection()
-> TestResult {
    let (coordinator, library, view) = selected_many(30, false)?;
    let fixture = fixture_with_mode(4096, "fail-query-once")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let warm = backend_engine::Query::new(
        "warm intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &warm, flag.clone())?.1,
        SemanticSearchStatus::Available
    );
    let calls_before = std::fs::read_to_string(&fixture.log)?;
    std::fs::write(&fixture.release, b"fail next query once")?;
    let retry = backend_engine::Query::new(
        "retry intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &retry, flag.clone())?.1,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable
        }
    );
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert!(
        owner.core().ok_or("finished failure")?.has_projection(),
        "query terminal does not destroy complete document relation"
    );
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &retry, flag.clone())?.1,
        SemanticSearchStatus::Available,
        "same exact key retries after its one owed failure"
    );
    let calls_after = std::fs::read_to_string(&fixture.log)?;
    let purpose_count = |log: &str, purpose: u64| -> Result<usize, Box<dyn std::error::Error>> {
        let mut count = 0;
        for line in log.lines() {
            let row: serde_json::Value = serde_json::from_str(line)?;
            count += usize::from(row["purpose"].as_u64() == Some(purpose));
        }
        Ok(count)
    };
    assert_eq!(
        purpose_count(&calls_after, 2)?,
        purpose_count(&calls_before, 2)?,
        "document projection is reused"
    );
    assert_eq!(
        purpose_count(&calls_after, 1)?,
        purpose_count(&calls_before, 1)? + 2,
        "one actual failed process then successful process"
    );
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &retry, flag)?.1,
        SemanticSearchStatus::Available
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        calls_after,
        "successful repeated key stays warm"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_owner_spawn_refusal_preserves_projection_selection_and_warm_answers() -> TestResult
{
    let (coordinator, library, view) = selected_many(30, false)?;
    let fixture = fixture(4096)?;
    let runtime = fixture.provision.runtime().ok_or("activated runtime")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let coverage = crate::builtin::admitted_coverage()?;
    let warm = backend_engine::Query::new(
        "warm intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let (warm_page, status) =
        settle_page(&coordinator, &library, &mut provider, &warm, flag.clone())?;
    assert_eq!(status, SemanticSearchStatus::Available);
    let before_calls = std::fs::read_to_string(&fixture.log)?;
    let before_credits = runtime.memory_budget().used();
    let crate::builtin::query::RemoteSemantic::Local(owner) = &mut provider else {
        return Err("local owner".into());
    };
    let retained = owner.core().ok_or("warm core")?;
    let before_binding = retained
        .active
        .as_ref()
        .ok_or("complete projection")?
        .index
        .binding();
    let before_admitted = retained.admitted();
    let before_cache = owner.cached_completions();
    owner.refuse_next_worker_spawn();
    let retry = backend_engine::Query::new(
        "retry intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let expected_lexical = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        coverage,
        &retry,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?
    .0;
    let (page, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        coverage,
        &retry,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        page, expected_lexical,
        "spawn refusal returns the exact lexical page"
    );
    assert_eq!(
        status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable,
        }
    );
    assert!(
        provider.local_preparation().is_none(),
        "no phantom worker wait"
    );
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    let retained = owner.core().ok_or("reclaimed original core")?;
    assert!(matches!(
        retained.failure(),
        Some(LocalSemanticError::Worker(LocalSemanticWorkerError::Spawn(
            _
        )))
    ));
    assert_eq!(
        retained
            .active
            .as_ref()
            .ok_or("retained projection")?
            .index
            .binding(),
        before_binding
    );
    assert!(
        Arc::ptr_eq(&retained.admitted(), &before_admitted),
        "same admitted runtime owner"
    );
    assert_eq!(owner.cached_completions(), before_cache);
    assert_eq!(
        runtime.memory_budget().used(),
        before_credits,
        "unstarted seed/key leases released"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        before_calls,
        "no producer on spawn refusal"
    );
    let (page, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        coverage,
        &warm,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        status,
        SemanticSearchStatus::Available,
        "original selection admits warm answer"
    );
    assert_eq!(page.root.rows(), warm_page.root.rows());
    assert_eq!(std::fs::read_to_string(&fixture.log)?, before_calls);
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &retry, flag.clone(),)?.1,
        SemanticSearchStatus::Available,
        "same refused key can retry normally"
    );
    let after_calls = std::fs::read_to_string(&fixture.log)?;
    let purposes = |log: &str| -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        log.lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line)?;
                row["purpose"]
                    .as_u64()
                    .ok_or_else(|| "protocol purpose".into())
            })
            .collect()
    };
    let before = purposes(&before_calls)?;
    let after = purposes(&after_calls)?;
    assert_eq!(
        after.iter().filter(|&&purpose| purpose == 2).count(),
        before.iter().filter(|&&purpose| purpose == 2).count(),
        "complete document projection never rebuilds"
    );
    assert_eq!(
        after.iter().filter(|&&purpose| purpose == 1).count(),
        before.iter().filter(|&&purpose| purpose == 1).count() + 1,
        "only successful retry invokes query producer"
    );
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &retry, flag.clone(),)?.1,
        SemanticSearchStatus::Available
    );
    assert_eq!(std::fs::read_to_string(&fixture.log)?, after_calls);
    flag.store(true, Ordering::Release);
    provider.poll_local(coordinator.workspace_root(), view.root());
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert!(
        !owner.core().ok_or("retired core")?.has_projection(),
        "real cancellation still retires the reclaimed selected core"
    );
    assert_eq!(owner.cached_completions(), 0);
    assert!(runtime.memory_budget().used() < before_credits);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_first_completion_is_owed_not_optional_before_actual_delivery() -> TestResult {
    let (coordinator, library, view) = selected_many(30, false)?;
    let fixture = fixture(4096)?;
    let runtime = fixture.provision.runtime().ok_or("runtime")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let request = backend_engine::Query::new(
        "first owed intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let (_, pending) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        pending,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        }
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while provider.local_preparation().is_some() {
        provider.poll_local(coordinator.workspace_root(), view.root());
        assert!(
            Instant::now() < deadline,
            "actual first completion did not arrive"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert!(
        owner.owes_completion(),
        "first result retained for owed delivery"
    );
    assert_eq!(
        owner.cached_completions(),
        0,
        "undelivered result is not optional cache"
    );
    let calls = std::fs::read_to_string(&fixture.log)?;
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())?;
    assert_eq!(
        runtime.memory_budget().reserve(1).err(),
        Some(EmbeddingMemoryError {
            requested: 1,
            available: 0
        }),
        "mandatory pressure cannot discard an undelivered first completion"
    );
    let (page, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        flag,
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        status,
        SemanticSearchStatus::Available,
        "owed original moves when no copy fits"
    );
    assert!(!page.root.rows().is_empty());
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert!(!owner.owes_completion());
    assert_eq!(
        owner.cached_completions(),
        0,
        "no uncharged retained copy after transfer"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        calls,
        "first delivery never recomputes completed work"
    );
    drop(pressure);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_shared_admission_reclaims_at_seed_query_and_owned_output() -> TestResult {
    let (coordinator, library, view) = selected_many(30, false)?;
    let fixture = fixture(4096)?;
    let runtime = fixture.provision.runtime().ok_or("runtime")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    for at in 0..12 {
        let request = backend_engine::Query::new(
            format!("warm intent {at}"),
            view.root(),
            backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
        );
        assert_eq!(
            settle_page(
                &coordinator,
                &library,
                &mut provider,
                &request,
                flag.clone()
            )?
            .1,
            SemanticSearchStatus::Available
        );
    }
    let before = std::fs::read_to_string(&fixture.log)?;
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())?;
    let crate::builtin::query::RemoteSemantic::Local(owner) = &mut provider else {
        return Err("local owner".into());
    };
    let cached = owner.cached_completions();
    let mut local = coordinator.search_local(LocalQuery::prefix("fixture", 3)?)?;
    let seed = local.take_lexical_seed(runtime.memory_budget(), &mut || Ok(()))?;
    assert!(
        owner.cached_completions() < cached,
        "seed's actual reserve releases optional cache"
    );
    drop(seed);
    let cached = owner.cached_completions();
    let core = owner.core_mut().ok_or("ready projection")?;
    let (_, query_memory, _) = core.prepare_and_query(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        "new pressured intent",
        &flag,
        3,
        runtime.inference_deadline()?,
    )?;
    assert!(
        owner.cached_completions() < cached,
        "query scratch/inference reserve follows same reclamation law"
    );
    drop(query_memory);
    assert!(owner.core().ok_or("ready projection")?.has_projection());
    drop(pressure);
    let after = std::fs::read_to_string(&fixture.log)?;
    let documents = |log: &str| {
        log.lines()
            .filter(|line| line.contains("\"purpose\": 2"))
            .count()
    };
    assert_eq!(
        documents(&before),
        documents(&after),
        "seed/query credit pressure does not rebuild document projection"
    );

    // Retain exactly one completion. An owed hit still succeeds when no duplicate
    // output lease fits, by moving the original result and lease to the projector.
    owner.set_completion_policy(super::super::QueryCompletionPolicy { maximum_bytes: 1 });
    let request = backend_engine::Query::new(
        "single owed intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let (expected, status) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &request,
        flag.clone(),
    )?;
    assert_eq!(status, SemanticSearchStatus::Available);
    let before_output = std::fs::read_to_string(&fixture.log)?;
    let pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())?;
    let (owed, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(status, SemanticSearchStatus::Available);
    assert_eq!(owed.root.rows(), expected.root.rows());
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        before_output,
        "owned-output transfer invokes no producer"
    );
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert_eq!(
        owner.cached_completions(),
        0,
        "original cache allocation moved, no uncharged retained copy"
    );
    assert!(owner.core().ok_or("ready projection")?.has_projection());
    let required_pressure = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())?;
    let (_, refused) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        flag.clone(),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        refused,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable
        },
        "mandatory occupied credits still refuse truthfully after all optional storage is gone"
    );
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("local owner".into());
    };
    assert!(
        owner.core().ok_or("ready projection")?.has_projection(),
        "transient query-key refusal preserves exact selected projection"
    );
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Memory(_))
    ));
    drop(required_pressure);
    drop(pressure);
    assert_eq!(
        settle_page(&coordinator, &library, &mut provider, &request, flag)?.1,
        SemanticSearchStatus::Available
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        before_output,
        "warm input coordinates and relation survive pressure recovery"
    );
    Ok(())
}

#[cfg(unix)]
fn selected() -> Result<
    (
        QueryCoordinator,
        backend_library::Library,
        backend_engine::ViewRoot,
    ),
    Box<dyn std::error::Error>,
> {
    let (workspace, view) = super::super::tests::selected_view();
    let evidence = super::super::tests::semantic_evidence(workspace, &view);
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().ok_or("view capability")?,
        crate::builtin::admitted_coverage()?,
        evidence,
    )?;
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )?;
    Ok((coordinator, library, view))
}

#[cfg(unix)]
fn selected_many(
    count: usize,
    oversized_last: bool,
) -> Result<
    (
        QueryCoordinator,
        backend_library::Library,
        backend_engine::ViewRoot,
    ),
    Box<dyn std::error::Error>,
> {
    let head = crate::builtin::genesis().map_err(|error| error.to_string())?;
    let (base, _) = crate::builtin::initial_view().map_err(|error| error.to_string())?;
    let capability =
        crate::builtin::test_builtin_view_capability().map_err(|error| error.to_string())?;
    let package = backend_engine::package_key("pkg");
    let mut rows = vec![backend_engine::Row::new(
        backend_engine::RowId::Package(package),
        base.basis(),
        "pkg",
    )];
    for index in 0..count {
        let label = if index + 1 == count {
            "meaning related winner".to_owned()
        } else {
            format!("fixture declaration {index}")
        };
        rows.push(backend_engine::Row::in_package(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&label)),
            base.basis(),
            package,
            label,
        ));
    }
    if oversized_last {
        let row = rows.iter_mut().max_by_key(|row| row.id).ok_or("last row")?;
        *row = row
            .clone()
            .with_document(vec![backend_engine::Fragment::Text("x".repeat(8192))]);
    }
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows,
        vec![backend_engine::ViewCoverage::Complete],
        capability.clone(),
    )
    .map_err(|error| format!("fixture view admission refused: {error:?}"))?;
    let evidence = super::super::tests::semantic_evidence(head.root(), &view);
    let coordinator = QueryCoordinator::new(
        head.root(),
        view.clone(),
        capability,
        crate::builtin::admitted_coverage().map_err(|error| error.to_string())?,
        evidence,
    )?;
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )?;
    Ok((coordinator, library, view))
}

#[cfg(unix)]
pub(crate) struct ProtocolFixture {
    _directory: tempfile::TempDir,
    pub(crate) provision: EmbeddingRuntimeProvision,
    pub(crate) descriptor: std::path::PathBuf,
    pub(crate) log: std::path::PathBuf,
    pub(crate) entered: std::path::PathBuf,
    pub(crate) release: std::path::PathBuf,
}

#[cfg(unix)]
pub(crate) fn fixture_owner(fixture: &ProtocolFixture) -> crate::builtin::query::RemoteSemantic {
    crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    )
}

#[cfg(unix)]
fn fixture(maximum_text: u32) -> Result<ProtocolFixture, Box<dyn std::error::Error>> {
    fixture_with_mode_and_winner(maximum_text, "plain", "")
}

#[cfg(unix)]
pub(crate) fn fixture_with_mode(
    maximum_text: u32,
    mode: &str,
) -> Result<ProtocolFixture, Box<dyn std::error::Error>> {
    fixture_with_mode_and_winner(maximum_text, mode, "")
}

#[cfg(unix)]
pub(crate) fn gated_owner_for_shutdown() -> Result<
    (
        ProtocolFixture,
        crate::builtin::query::RemoteSemantic,
        Arc<AtomicBool>,
    ),
    Box<dyn std::error::Error>,
> {
    let fixture = fixture_with_mode(4096, "gate-query")?;
    let (coordinator, _, _) = selected_many(3, false)?;
    let mut owner = fixture_owner(&fixture);
    let cancelled = Arc::new(AtomicBool::new(false));
    let query = LocalQuery::prefix("fixture", 3)?;
    let _ = owner.search_local_with_cancellation(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        coordinator.search_local(query)?,
        "shutdown intent",
        Some(Arc::clone(&cancelled)),
    );
    wait_for_fixture_marker(&fixture.entered)?;
    Ok((fixture, owner, cancelled))
}

#[cfg(unix)]
#[test]
fn local_semantic_joined_shutdown_is_quiescent_and_cannot_restart() -> TestResult {
    let (fixture, mut provider, cancelled) = gated_owner_for_shutdown()?;
    // If a pre-join assertion fails, release the fixture before provider Drop
    // performs its final join. The normal path never releases the gate.
    struct ReleaseGateOnFailure(Option<std::path::PathBuf>);
    impl Drop for ReleaseGateOnFailure {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::write(path, b"failure-safe shutdown fixture release");
            }
        }
    }
    let mut release_on_failure = ReleaseGateOnFailure(Some(fixture.release.clone()));
    let runtime = fixture
        .provision
        .runtime()
        .ok_or("activated protocol fixture")?;
    let held = runtime.memory_budget().used();
    assert_eq!(
        provider.local_preparation(),
        Some(backend_library::QueryPreparationState::Preparing)
    );
    provider.request_close_local();
    assert!(cancelled.load(Ordering::Acquire));
    assert_eq!(
        provider.local_preparation(),
        Some(backend_library::QueryPreparationState::Retiring),
        "signalling cancellation still owes the actual worker join"
    );
    // The supervisor must retire the real gated child on cancellation; do not
    // manufacture readiness by releasing its inference gate or polling it away.
    provider.close_local();
    release_on_failure.0 = None;
    assert!(provider.local_preparation().is_none());
    assert!(!fixture.release.exists());
    assert!(runtime.memory_budget().used() < held);
    let crate::builtin::query::RemoteSemantic::Local(owner) = &provider else {
        return Err("shutdown must preserve the closed local owner identity".into());
    };
    assert!(!owner.has_retained_projection());
    assert!(!owner.owes_completion());
    assert_eq!(owner.cached_completions(), 0);
    let retired_credits = runtime.memory_budget().used();
    let calls = std::fs::read_to_string(&fixture.log)?;
    let (coordinator, _, _) = selected_many(3, false)?;
    let fresh_cancellation = Arc::new(AtomicBool::new(false));
    let query = LocalQuery::prefix("fixture", 3)?;
    let answer = provider.search_local_with_cancellation(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        coordinator.search_local(query)?,
        "fresh intent after joined shutdown",
        Some(Arc::clone(&fresh_cancellation)),
    );
    assert_eq!(
        answer.status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        },
        "closed admission cannot start a replacement inference worker"
    );
    drop(answer);
    assert!(!fresh_cancellation.load(Ordering::Acquire));
    assert!(provider.local_preparation().is_none());
    assert_eq!(runtime.memory_budget().used(), retired_credits);
    assert_eq!(std::fs::read_to_string(&fixture.log)?, calls);
    provider.close_local();
    assert!(
        provider.local_preparation().is_none(),
        "joined close is idempotent"
    );
    // Closing an owner that has never started inference is equally quiescent.
    let mut idle = fixture_owner(&fixture);
    assert!(idle.local_preparation().is_none());
    idle.close_local();
    idle.close_local();
    assert!(idle.local_preparation().is_none());
    Ok(())
}

#[cfg(unix)]
pub(crate) fn protocol_fixture_python() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    // Match the existing native-tooling/engine test authority convention. An
    // explicit selection fails closed instead of falling through to another tool.
    let selected = std::env::var_os("COMPILER_PYTHON_COMPILER")
        .or_else(|| std::env::var_os("NUDOX_PYTHON"))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).map(|directory| directory.join("python3"))
                .find(|path| path.is_file())
        })).ok_or("BEM2 protocol controls require a real Python executable on PATH or COMPILER_PYTHON_COMPILER")?;
    let program = selected.canonicalize()?;
    let metadata = std::fs::metadata(&program)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("Selected protocol fixture Python is not an executable regular file".into());
    }
    Ok(program)
}

#[cfg(unix)]
#[test]
fn local_semantic_protocol_fixture_binds_actual_interpreter_and_script_dependency() -> TestResult {
    let fixture = fixture(4096)?;
    let interpreter = protocol_fixture_python()?;
    let script = fixture
        ._directory
        .path()
        .join("protocol-fixture.py")
        .canonicalize()?;
    let executable = backend_compile::ExecutableIdentity::from_path(&interpreter)?;
    let dependency = backend_compile::ExecutableIdentity::from_path(&script)?;
    let closure = backend_compile::ToolchainArtifact::new(executable, vec![dependency.digest()]);
    let config = fixture.descriptor.with_file_name("embedding.config");
    let inspected = backend_engine::application::inspect_embedding_runtime(&config)?
        .ok_or("installed protocol fixture configuration is missing")?;
    assert_eq!(inspected.spec.executable(), closure.identity().to_bytes());
    assert!(
        fixture.provision.runtime().is_some(),
        "actual interpreter admitted by normal activation"
    );
    let mut changed = std::fs::read(&script)?;
    changed.extend_from_slice(b"\n# changed protocol bytes\n");
    std::fs::write(&script, changed)?;
    let reopened = EmbeddingRuntimeProvision::open(&config)?;
    assert!(
        reopened.runtime().is_none(),
        "changed protocol dependency cannot reactivate under the previous exact closure"
    );
    Ok(())
}

#[cfg(unix)]
fn fixture_with_mode_and_winner(
    maximum_text: u32,
    mode: &str,
    winner_text: &str,
) -> Result<ProtocolFixture, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir()?;
    crate::test_support::make_private(directory.path())?;
    let root = directory.path().canonicalize()?;
    let interpreter = protocol_fixture_python()?;
    let program = root.join("protocol-fixture.py");
    let log = root.join("input.jsonl");
    let entered = root.join("second-page-entered");
    let release = root.join("second-page-release");
    let armed = root.join("query-gate-armed");
    std::fs::write(
        &program,
        br"import json, os, struct, sys, time
frame = sys.stdin.buffer.read()
if frame[:4] != b'BEM2' or len(frame) < 108: sys.exit(3)
purpose = frame[4]
count = struct.unpack('>I', frame[104:108])[0]
offset = 108
entries = []
for _ in range(count):
    identity = frame[offset:offset+32]
    length = struct.unpack('>I', frame[offset+32:offset+36])[0]
    offset += 36
    text = frame[offset:offset+length].decode('utf8')
    offset += length
    entries.append((identity,text))
if offset != len(frame): sys.exit(4)
with open(os.environ['FIXTURE_LOG'], 'a') as log:
    log.write(json.dumps({'purpose':purpose,'texts':[text for _,text in entries]}) + '\n')
with open(os.environ['FIXTURE_LOG']) as log:
    documents = sum(json.loads(line)['purpose'] == 2 for line in log)
mode = os.environ['FIXTURE_MODE']
if purpose == 1 and mode == 'fail-query-once' and os.path.exists(os.environ['FIXTURE_RELEASE']):
    os.unlink(os.environ['FIXTURE_RELEASE'])
    sys.exit(9)
if purpose == 1 and mode == 'gate-query' and os.path.exists(os.environ['FIXTURE_ARMED']):
    with open(os.environ['FIXTURE_ENTERED'], 'w') as marker: marker.write('query entered')
    while not os.path.exists(os.environ['FIXTURE_RELEASE']): time.sleep(0.005)
if purpose == 2 and documents == 2 and mode in ('gate-second', 'fail-second'):
    with open(os.environ['FIXTURE_ENTERED'], 'w') as marker: marker.write('entered')
    if mode == 'fail-second': sys.exit(9)
    while not os.path.exists(os.environ['FIXTURE_RELEASE']): time.sleep(0.005)
response = bytearray(b'BEC2' + struct.pack('>HI',2,count))
for identity,text in entries:
    winner_text = os.environ['FIXTURE_WINNER_TEXT']
    winner = purpose == 1 or (text == winner_text if winner_text else 'meaning related' in text)
    response.extend(identity)
    response.extend(struct.pack('<ff', 0.0 if winner else 1.0, 1.0 if winner else 0.0))
sys.stdout.buffer.write(response)",
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    let contract = LocalQueryRecipeContract {
        metric: vector::Metric::CosineDistance,
        pooling: vector::EmbeddingPooling::Mean,
        encoding: vector::EmbeddingEncoding::Float32,
        query_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
        document_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
        provider_options: *blake3::hash(b"explicit fixture tokenizer/pool options").as_bytes(),
    };
    let compiler = root.join("compiler");
    backend_platform::durable::ensure_private_directory(&compiler)?;
    let config = compiler.join("embedding.config");
    let spec = install_embedding_runtime(
        &config,
        EmbeddingRuntimeInstall {
            requirement: EmbeddingRequirement::Optional,
            program: interpreter,
            arguments: vec![
                "-I".to_owned(),
                program
                    .to_str()
                    .ok_or("UTF-8 protocol script path")?
                    .to_owned(),
            ],
            dependencies: vec![program],
            environment: vec![
                ("FIXTURE_LOG".into(), log.to_string_lossy().into_owned()),
                ("FIXTURE_MODE".into(), mode.to_owned()),
                ("FIXTURE_ARMED".into(), armed.to_string_lossy().into_owned()),
                ("FIXTURE_WINNER_TEXT".into(), winner_text.to_owned()),
                (
                    "FIXTURE_ENTERED".into(),
                    entered.to_string_lossy().into_owned(),
                ),
                (
                    "FIXTURE_RELEASE".into(),
                    release.to_string_lossy().into_owned(),
                ),
            ],
            limits: EmbeddingRuntimeLimits {
                stdout_bytes: 128 * 1024,
                stderr_bytes: 1024,
                output_bytes: 129 * 1024,
                input_bytes: 1024 * 1024,
                wall_time_ms: 5000,
                workspace_bytes: 1024 * 1024,
                process_count: 8,
                memory_bytes: None,
                cpu_time_ms: None,
                resident_credit_bytes: 128 * 1024 * 1024,
            },
            model_bytes: b"fixture model bytes".to_vec(),
            tokenizer_bytes: b"fixture tokenizer bytes".to_vec(),
            dimension: NonZeroU16::new(2).ok_or("dimension")?,
            normalization: backend_compile::EmbeddingNormalization::L2,
            maximum_text_bytes: NonZeroU32::new(maximum_text).ok_or("text bound")?,
            options_digest: contract.options_digest(),
        },
    )?;
    let descriptor = compiler.join("embedding-query.config");
    backend_platform::durable::write_private_atomic(
        &descriptor,
        &LocalQueryRecipeDescriptor::new(spec, contract)?.canonical_bytes(),
    )?;
    let provision = EmbeddingRuntimeProvision::open(&config)?;
    if provision.runtime().is_none() {
        return Err(format!(
            "fixture activation must actually succeed: status={:?}, provisioning_failure={:?}",
            provision.status(),
            provision.provisioning_failure(),
        )
        .into());
    }
    // Activation executes the real BEM2 query self-test. A held serving query
    // is a later event under the same admitted runtime; arming before activation
    // would block that mandatory probe and leave all ticket/cache assertions
    // unreachable. Do not replace the probe, renew its deadline, or fake Ready.
    if mode == "gate-query" {
        assert!(
            !entered.exists(),
            "activation cannot enter the serving-query gate"
        );
        std::fs::write(&armed, b"real runtime activation succeeded")?;
    }
    Ok(ProtocolFixture {
        _directory: directory,
        provision,
        descriptor,
        log,
        entered,
        release,
    })
}

#[cfg(unix)]
fn settle_page(
    coordinator: &QueryCoordinator,
    library: &backend_library::Library,
    provider: &mut crate::builtin::query::RemoteSemantic,
    request: &backend_engine::Query,
    flag: Arc<AtomicBool>,
) -> Result<(backend_engine::ViewSnapshot, SemanticSearchStatus), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let page = super::super::search_page_with_cancellation(
            coordinator,
            library,
            provider,
            crate::builtin::admitted_coverage()?,
            request,
            Arc::clone(&flag),
        )
        .map_err(|error| error.to_string())?;
        if !matches!(
            page.1,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::NoActiveProjection
            }
        ) {
            return Ok(page);
        }
        if Instant::now() >= deadline {
            return Err("fixture semantic worker did not retire".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
fn wait_for_fixture_marker(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err("fixture did not enter second page".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[test]
fn local_semantic_missing_setup_preserves_cause_and_explicit_remote_choice() -> TestResult {
    let directory = tempfile::tempdir()?;
    crate::test_support::make_private(directory.path())?;
    let provision = EmbeddingRuntimeProvision::open(directory.path().join("embedding.config"))?;
    let path = directory.path().join("embedding-query.config");
    let missing = crate::builtin::query::RemoteSemantic::select_owner(
        &provision,
        &path,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let cause = missing
        .local_failure()
        .ok_or("retained local setup cause")?;
    assert!(matches!(
        cause,
        LocalSemanticError::RuntimeUnavailable(
            backend_engine::application::EmbeddingRuntimeStatus::NotConfigured
        )
    ));
    assert!(missing.is_unconfigured());
    assert!(cause.setup_instruction().contains("nudox embedding-setup"));
    let explicit = crate::builtin::query::RemoteSemantic::select_owner(
        &provision,
        &path,
        crate::builtin::query::RemoteSemantic::Unavailable(Box::new(
            crate::builtin::query::RemoteConfigError::MissingValue(
                crate::builtin::query::QDRANT_COLLECTION_ENV,
            ),
        )),
    );
    assert!(matches!(
        explicit,
        crate::builtin::query::RemoteSemantic::Unavailable(_)
    ));
    assert!(explicit.local_failure().is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_real_protocol_fixture_contributes_exact_canonical_rows_and_shared_credits()
-> TestResult {
    let (coordinator, library, view) = selected_many(300, false)?;
    let documents = coordinator
        .semantic_document_pages(NonZeroUsize::new(256).ok_or("page")?)?
        .flatten()
        .collect::<Vec<_>>();
    let winner = documents
        .iter()
        .max_by_key(|document| coordinator.semantic_candidate(document.row))
        .ok_or("winner")?;
    let winner_row = winner.row;
    let fixture = fixture_with_mode_and_winner(4096, "plain", &winner.text)?;
    let winner_id = coordinator
        .semantic_candidate(winner_row)
        .ok_or("winner id")?;
    assert!(
        documents
            .iter()
            .filter_map(|document| coordinator.semantic_candidate(document.row))
            .filter(|id| *id < winner_id)
            .count()
            > 4 * 3,
        "winner beyond any 4*limit candidate prefix"
    );
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let runtime = fixture.provision.runtime().ok_or("shared runtime")?;
    if let crate::builtin::query::RemoteSemantic::Local(local) = &provider {
        assert!(
            Arc::ptr_eq(local.admitted_runtime(), &runtime),
            "factory must retain exact compiler owner, not equivalent activation"
        );
        assert_eq!(
            local.inventory_status().lifecycle(),
            backend_engine::CapabilityLifecycle::Installed
        );
    } else {
        return Err("local recipe admission".into());
    }
    let baseline = runtime.memory_budget().used();
    let request = backend_engine::Query::new(
        "intent",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let flag = Arc::new(AtomicBool::new(false)); // Actual fixture selection lifetime; no transport claim.
    let before = std::fs::read_to_string(&fixture.log)?.lines().count();
    let (page, status) = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        }
    );
    assert!(page.root.rows().is_empty());
    let (page, status) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &request,
        Arc::clone(&flag),
    )?;
    assert_eq!(status, SemanticSearchStatus::Available);
    assert!(page.root.rows().iter().any(|row| row.id == winner_row));
    let log = std::fs::read_to_string(&fixture.log)?;
    let invocations = log
        .lines()
        .skip(before)
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let observed = invocations
        .iter()
        .filter(|entry| entry["purpose"] == 2)
        .flat_map(|entry| entry["texts"].as_array().into_iter().flatten())
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        documents
            .iter()
            .map(|document| document.text.as_str())
            .collect::<Vec<_>>()
    );
    let page_counts = invocations
        .iter()
        .filter(|entry| entry["purpose"] == 2)
        .map(|entry| entry["texts"].as_array().map_or(0, Vec::len))
        .collect::<Vec<_>>();
    assert_eq!(page_counts, [256, 45]);
    if let crate::builtin::query::RemoteSemantic::Local(local) = &provider {
        let projection = local
            .core()
            .ok_or("idle semantic owner")?
            .active
            .as_ref()
            .ok_or("complete projection")?;
        let mut oracle = documents
            .iter()
            .map(|document| {
                let id = coordinator
                    .semantic_candidate(document.row)
                    .ok_or("candidate")?;
                let score = if document.row == winner_row {
                    0.0f32
                } else {
                    1.0f32
                };
                Ok((score, id))
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        oracle.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        let query = vector::QueryVector::new(local.recipe(), vec![0.0, 1.0])?;
        let exact = projection.index.search_exact_embedding(
            projection.index.binding(),
            &query,
            3,
            projection.limits,
        )?;
        assert_eq!(
            exact
                .candidates
                .iter()
                .map(|candidate| (candidate.score, candidate.id))
                .collect::<Vec<_>>(),
            oracle[..3]
        );
        assert_eq!(exact.quality, vector::SearchQuality::Exact);
    } else {
        return Err("configured local owner required".into());
    }
    assert!(
        invocations
            .iter()
            .any(|entry| entry["purpose"] == 1 && entry["texts"][0] == "intent")
    );
    assert!(runtime.memory_budget().used() > baseline);
    let projection_credits = runtime.memory_budget().used();
    let (_, warm) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &request,
        Arc::clone(&flag),
    )?;
    assert_eq!(warm, SemanticSearchStatus::Available);
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?.lines().count(),
        log.lines().count(),
        "same actual inputs must reuse runtime coordinates"
    );
    flag.store(true, Ordering::Release);
    let scan_count = coordinator.corpus.lexical.rank_evaluations();
    let cancelled = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        Arc::clone(&flag),
    );
    assert!(matches!(
        cancelled,
        Err(super::super::SearchPageError::Local(
            QueryError::SelectedViewRetired
        ))
    ));
    assert_eq!(
        coordinator.corpus.lexical.rank_evaluations(),
        scan_count,
        "retired selection must not begin another lexical scan"
    );
    // The direct semantic fallback contract still retains a previously complete
    // lexical answer; initial selected execution now refuses before that boundary.
    let fallback = provider.search_local_with_cancellation(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        coordinator.search_local_page(&request)?,
        request.text(),
        Some(Arc::clone(&flag)),
    );
    assert!(matches!(
        fallback.status,
        SemanticSearchStatus::Unavailable { .. }
    ));
    assert!(
        fallback.result.rows.is_empty(),
        "cancelled projection retains the original empty lexical answer"
    );
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
        ))
    ));
    assert!(
        runtime.memory_budget().used() < projection_credits,
        "whole projection lease must release"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_complete_corpus_text_and_memory_refusals_invoke_no_producer() -> TestResult {
    let limited = fixture(4096)?;
    let (coordinator, _, _) = selected_many(300, true)?;
    let mut local = LocalSemantic::open(&limited.provision, &limited.descriptor)?;
    let before = std::fs::read_to_string(&limited.log)?;
    let answer = coordinator.search_local(LocalQuery::prefix("alpha", 3)?)?;
    let flag = AtomicBool::new(false);
    let (_, status) = local.search(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        answer,
        "alpha",
        &flag,
    );
    assert!(matches!(status, SemanticSearchStatus::Unavailable { .. }));
    assert!(matches!(
        local.failure(),
        Some(LocalSemanticError::Corpus(
            QueryError::SemanticDocumentTooLarge { .. }
        ))
    ));
    assert!(!local.has_projection());
    assert_eq!(std::fs::read_to_string(&limited.log)?, before);

    let sufficient = fixture(4096)?;
    let (coordinator, _, _) = selected()?;
    let runtime = sufficient.provision.runtime().ok_or("runtime")?;
    let mut local = LocalSemantic::open(&sufficient.provision, &sufficient.descriptor)?;
    let retained = runtime
        .memory_budget()
        .reserve(runtime.memory_budget().capacity() - runtime.memory_budget().used())?;
    let before = std::fs::read_to_string(&sufficient.log)?;
    let answer = coordinator.search_local(LocalQuery::prefix("alpha", 3)?)?;
    let (_, status) = local.search(
        &coordinator,
        crate::builtin::admitted_coverage()?,
        answer,
        "alpha",
        &flag,
    );
    assert!(matches!(status, SemanticSearchStatus::Unavailable { .. }));
    assert!(matches!(
        local.failure(),
        Some(LocalSemanticError::Memory(_))
    ));
    assert!(!local.has_projection());
    assert_eq!(std::fs::read_to_string(&sufficient.log)?, before);
    drop(retained);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_descriptor_absence_mismatch_and_symlink_never_activate_a_second_runtime()
-> TestResult {
    let fixture = fixture(4096)?;
    let before = std::fs::read_to_string(&fixture.log)?;
    std::fs::remove_file(&fixture.descriptor)?;
    assert!(matches!(
        LocalSemantic::open(&fixture.provision, &fixture.descriptor),
        Err(LocalSemanticError::RecipeNotConfigured)
    ));
    let malformed = vec![0u8; 214];
    backend_platform::durable::write_private_atomic(&fixture.descriptor, &malformed)?;
    assert!(matches!(
        LocalSemantic::open(&fixture.provision, &fixture.descriptor),
        Err(LocalSemanticError::Recipe(
            LocalQueryRecipeError::DescriptorFormat
        ))
    ));
    std::fs::remove_file(&fixture.descriptor)?;
    let target = fixture._directory.path().join("foreign-query.config");
    std::fs::write(&target, malformed)?;
    std::os::unix::fs::symlink(&target, &fixture.descriptor)?;
    assert!(matches!(
        LocalSemantic::open(&fixture.provision, &fixture.descriptor),
        Err(LocalSemanticError::DescriptorIo(_))
    ));
    assert_eq!(std::fs::read_to_string(&fixture.log)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn recipe_addressed_descriptor_selects_exact_runtime_and_never_falls_back_on_corruption()
-> TestResult {
    let fixture = fixture(4096)?;
    let runtime = fixture
        .provision
        .runtime()
        .ok_or("activated protocol fixture")?;
    let before = std::fs::read_to_string(&fixture.log)?;
    let correct = std::fs::read(&fixture.descriptor)?;
    let selected = super::super::local_recipe::recipe_descriptor_path(
        fixture.descriptor.parent().ok_or("parent")?,
        runtime.execution_identity().recipe(),
    );
    backend_platform::durable::write_private_atomic(&selected, &correct)?;
    backend_platform::durable::write_private_atomic(&fixture.descriptor, &[0; 214])?;
    let owner = LocalSemantic::open(&fixture.provision, &fixture.descriptor)?;
    assert!(Arc::ptr_eq(owner.admitted().runtime(), &runtime));
    backend_platform::durable::write_private_atomic(&fixture.descriptor, &correct)?;
    backend_platform::durable::write_private_atomic(&selected, &[0; 214])?;
    assert!(matches!(
        LocalSemantic::open(&fixture.provision, &fixture.descriptor),
        Err(LocalSemanticError::Recipe(
            LocalQueryRecipeError::DescriptorFormat
        ))
    ));
    assert_eq!(std::fs::read_to_string(&fixture.log)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_gated_worker_serves_complete_lexical_pages_and_retires_actual_selection()
-> TestResult {
    let fixture = fixture_with_mode(4096, "gate-second")?;
    let (coordinator, library, view) = selected_many(300, false)?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let runtime = fixture.provision.runtime().ok_or("runtime")?;
    let baseline = runtime.memory_budget().used();
    let request = backend_engine::Query::new(
        "fixture",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let expected = super::super::search_page(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        crate::builtin::admitted_coverage()?,
        &request,
    )
    .map_err(|error| error.to_string())?
    .0;
    let flag = Arc::new(AtomicBool::new(false));
    let first = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        first.1,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        }
    );
    assert_eq!(first.0, expected);
    wait_for_fixture_marker(&fixture.entered)?;
    let calls = std::fs::read_to_string(&fixture.log)?;
    assert_eq!(
        provider.local_preparation(),
        Some(backend_library::QueryPreparationState::Preparing)
    );
    let held = runtime.memory_budget().used();
    for _ in 0..8 {
        let waiting = super::super::search_page_with_cancellation(
            &coordinator,
            &library,
            &mut provider,
            crate::builtin::admitted_coverage()?,
            &request,
            Arc::clone(&flag),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            waiting.1,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::NoActiveProjection
            }
        );
        assert_eq!(waiting.0, expected);
    }
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        calls,
        "one worker, no inference queue"
    );
    let (changed, _, changed_view) = selected_many(301, false)?;
    provider.poll_local(changed.workspace_root(), changed_view.root());
    assert!(flag.load(Ordering::Acquire));
    assert_eq!(
        provider.local_preparation(),
        Some(backend_library::QueryPreparationState::Retiring)
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        provider.poll_local(changed.workspace_root(), changed_view.root());
        if provider.local_preparation().is_none() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("cancelled fixture did not retire".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
        ))
    ));
    assert!(
        runtime.memory_budget().used() < held,
        "projection/page/query/dispatch leases released"
    );
    assert!(
        runtime.memory_budget().used() >= baseline,
        "validated runtime cache may retain first-page coordinates"
    );
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
        assert!(!owner.core().ok_or("retired owner")?.has_projection());
    }
    assert_eq!(std::fs::read_to_string(&fixture.log)?, calls);
    assert!(
        !fixture.release.exists(),
        "supervisor cancelled actual gated child without fixture release"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_later_page_failure_discards_entire_projection_and_never_runs_query() -> TestResult
{
    let fixture = fixture_with_mode(4096, "fail-second")?;
    let (coordinator, library, view) = selected_many(300, false)?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let request = backend_engine::Query::new(
        "fixture",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let before = std::fs::read_to_string(&fixture.log)?.lines().count();
    let flag = Arc::new(AtomicBool::new(false));
    let (page, status) = settle_page(&coordinator, &library, &mut provider, &request, flag)?;
    assert_eq!(
        status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable
        }
    );
    let expected = super::super::search_page(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        crate::builtin::admitted_coverage()?,
        &request,
    )
    .map_err(|error| error.to_string())?
    .0;
    assert_eq!(page, expected);
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Inference(
            EmbeddingExecutableError::Terminal(backend_compile::ProcessTerminal::Exit)
        ))
    ));
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
        assert!(!owner.core().ok_or("refused owner")?.has_projection());
    }
    let log = std::fs::read_to_string(&fixture.log)?;
    let calls = log
        .lines()
        .skip(before)
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["purpose"] == 2));
    assert_eq!(calls[0]["texts"].as_array().ok_or("page")?.len(), 256);
    assert_eq!(calls[1]["texts"].as_array().ok_or("page")?.len(), 45);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_partial_corpus_refuses_before_any_document_or_query_inference() -> TestResult {
    let fixture = fixture(4096)?;
    let (coordinator, _, _) = selected()?;
    let mut core = LocalSemantic::open(&fixture.provision, &fixture.descriptor)?;
    let before = std::fs::read_to_string(&fixture.log)?;
    let local = coordinator.search_local(LocalQuery::prefix("alpha", 3)?)?;
    let (_, status) = core.search(
        &coordinator,
        CoverageWitness::Partial(backend_version::partial_coverage(19)),
        local,
        "alpha",
        &AtomicBool::new(false),
    );
    assert_eq!(
        status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable
        }
    );
    assert!(matches!(
        core.failure(),
        Some(LocalSemanticError::CorpusEnvelope)
    ));
    assert!(!core.has_projection());
    assert_eq!(std::fs::read_to_string(&fixture.log)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_semantic_whole_operation_deadline_discards_gated_projection_and_retains_cause()
-> TestResult {
    let fixture = fixture_with_mode(4096, "gate-second")?;
    let (coordinator, library, view) = selected_many(300, false)?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let request = backend_engine::Query::new(
        "fixture",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let (page, status) = settle_page(
        &coordinator,
        &library,
        &mut provider,
        &request,
        Arc::new(AtomicBool::new(false)),
    )?;
    assert_eq!(
        status,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::ProviderUnavailable
        }
    );
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Deadline)
        ))
    ));
    let expected = super::super::search_page(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        crate::builtin::admitted_coverage()?,
        &request,
    )
    .map_err(|error| error.to_string())?
    .0;
    assert_eq!(page, expected);
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
        assert!(!owner.core().ok_or("deadline owner")?.has_projection());
    }
    assert!(fixture.entered.exists());
    assert!(!fixture.release.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn every_projection_cpu_phase_discards_cancelled_or_expired_complete_work() -> TestResult {
    let (coordinator, _, _) = selected_many(30, false)?;
    let fixture = fixture(4096)?;
    let runtime = fixture.provision.runtime().ok_or("runtime")?;
    let core = LocalSemantic::open(&fixture.provision, &fixture.descriptor)?;
    let coverage = crate::builtin::admitted_coverage()?;
    let warm_flag = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    // This protocol fixture warms actual cache ownership, not a pretrained model.
    let warm = build_projection(&coordinator, coverage, &core.admitted, &warm_flag, deadline)?;
    drop(warm);
    let retained_cache_credit = runtime.memory_budget().used();
    let producer_calls = std::fs::read_to_string(&fixture.log)?.lines().count();
    let phases = [
        ProjectionPhase::TextBounds,
        ProjectionPhase::RowCloneExtent,
        ProjectionPhase::DocumentPage,
        ProjectionPhase::Payload,
        ProjectionPhase::OrderEntries,
        ProjectionPhase::Relation,
        ProjectionPhase::CandidateState,
        ProjectionPhase::Facts,
        ProjectionPhase::Base,
        ProjectionPhase::Source,
        ProjectionPhase::Index,
    ];
    for phase in phases {
        for expired in [false, true] {
            let flag = AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut visits = 0;
            let stop_at = if matches!(
                phase,
                ProjectionPhase::DocumentPage | ProjectionPhase::Index
            ) {
                1
            } else {
                3
            };
            let result = build_projection_checked(
                &coordinator,
                coverage,
                &core.admitted,
                &flag,
                deadline,
                &mut |observed| {
                    if observed == phase {
                        visits += 1;
                        if visits == stop_at {
                            if expired {
                                return checkpoint(&flag, Instant::now());
                            }
                            flag.store(true, Ordering::Release);
                        }
                    }
                    checkpoint(&flag, deadline)
                },
            );
            let error = result.err().ok_or("CPU refusal returned a projection")?;
            match (expired, error) {
                (
                    false,
                    LocalSemanticError::Inference(EmbeddingExecutableError::Process(
                        backend_compile::ProcessError::Cancelled,
                    )),
                )
                | (
                    true,
                    LocalSemanticError::Inference(EmbeddingExecutableError::Process(
                        backend_compile::ProcessError::Deadline,
                    )),
                ) => {}
                (_, error) => return Err(error.into()),
            }
            assert_eq!(visits, stop_at, "{phase:?}: no continuation after refusal");
            assert_eq!(
                runtime.memory_budget().used(),
                retained_cache_credit,
                "{phase:?}: caller projection/page/bookkeeping leases released"
            );
            assert_eq!(
                std::fs::read_to_string(&fixture.log)?.lines().count(),
                producer_calls,
                "warm fixture should invoke no new inference after CPU refusal"
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn controlled_worker_lexical_scan_preserves_answer_and_stops_inside_callbacks() -> TestResult {
    let (coordinator, _, _) = selected_many(30, false)?;
    let query = LocalQuery::prefix("fixture", 20)?;
    let ordinary = coordinator.search_local(query.clone())?;
    let mut visits = 0;
    let checked = coordinator.search_local_controlled(query.clone(), &mut || {
        visits += 1;
        Ok::<(), LocalSemanticError>(())
    })?;
    assert_eq!(checked.rows, ordinary.rows);
    assert_eq!(checked.matches, ordinary.matches);
    assert_eq!(checked.total_matches, ordinary.total_matches);
    assert_eq!(checked.lanes, ordinary.lanes);
    assert!(visits > 30);
    for expired in [false, true] {
        let flag = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut observed = 0;
        let result = coordinator.search_local_controlled(query.clone(), &mut || {
            observed += 1;
            if observed == 12 {
                if expired {
                    return checkpoint(&flag, Instant::now());
                }
                flag.store(true, Ordering::Release);
            }
            checkpoint(&flag, deadline)
        });
        assert!(
            matches!(&result, Err(LocalSemanticError::Inference(EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled))) if !expired)
                || matches!(&result, Err(LocalSemanticError::Inference(EmbeddingExecutableError::Process(backend_compile::ProcessError::Deadline))) if expired)
        );
        assert_eq!(observed, 12);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn same_roots_with_changed_exact_selection_retires_and_releases_the_only_worker() -> TestResult {
    #[derive(Clone, Copy, Debug)]
    enum Change {
        Corpus,
        Cancellation,
        Coverage,
    }
    for change in [Change::Corpus, Change::Cancellation, Change::Coverage] {
        let fixture = fixture_with_mode(4096, "gate-second")?;
        let (coordinator, _, view) = selected_many(300, false)?;
        let coverage = crate::builtin::admitted_coverage()?;
        let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
            &fixture.provision,
            &fixture.descriptor,
            crate::builtin::query::RemoteSemantic::Unconfigured,
        );
        let runtime = fixture.provision.runtime().ok_or("runtime")?;
        let flag = Arc::new(AtomicBool::new(false));
        let query = LocalQuery::prefix("fixture", 3)?;
        let first = provider.search_local_with_cancellation(
            &coordinator,
            coverage,
            coordinator.search_local(query.clone())?,
            "fixture",
            Some(Arc::clone(&flag)),
        );
        assert_eq!(
            first.status,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::NoActiveProjection
            }
        );
        wait_for_fixture_marker(&fixture.entered)?;
        let held = runtime.memory_budget().used();
        let calls = std::fs::read_to_string(&fixture.log)?;
        let replacement = if matches!(change, Change::Corpus) {
            selected_many(300, false)?.0
        } else {
            coordinator.clone()
        };
        assert_eq!(replacement.workspace_root(), coordinator.workspace_root());
        assert_eq!(replacement.corpus.view.root(), view.root());
        let replacement_flag = if matches!(change, Change::Cancellation) {
            Arc::new(AtomicBool::new(false))
        } else {
            Arc::clone(&flag)
        };
        let replacement_coverage = if matches!(change, Change::Coverage) {
            CoverageWitness::Partial(backend_version::partial_coverage(19))
        } else {
            coverage
        };
        let retiring = provider.search_local_with_cancellation(
            &replacement,
            replacement_coverage,
            replacement.search_local(query)?,
            "fixture",
            Some(Arc::clone(&replacement_flag)),
        );
        assert_eq!(
            retiring.status,
            SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::NoActiveProjection
            },
            "{change:?}"
        );
        assert!(
            flag.load(Ordering::Acquire),
            "actual old inference flag must be cancelled"
        );
        if matches!(change, Change::Cancellation) {
            assert!(
                !replacement_flag.load(Ordering::Acquire),
                "replacement lifetime is independent"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while provider.local_preparation().is_some() {
            provider.poll_local(replacement.workspace_root(), view.root());
            if Instant::now() >= deadline {
                return Err("same-root stale worker did not join".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            provider.local_failure(),
            Some(LocalSemanticError::Inference(
                EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
            ))
        ));
        assert!(
            runtime.memory_budget().used() < held,
            "{change:?}: projection/page/query/key credits released"
        );
        if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
            assert!(!owner.core().ok_or("retired owner")?.has_projection());
        }
        assert_eq!(
            std::fs::read_to_string(&fixture.log)?,
            calls,
            "no replacement inference queued"
        );
        assert!(
            !fixture.release.exists(),
            "actual supervised child stopped without gate release"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn exclusive_worker_stops_in_relation_cpu_phase_then_poll_joins_and_releases() -> TestResult {
    let fixture = fixture(4096)?;
    let (coordinator, library, view) = selected_many(30, false)?;
    let runtime = fixture.provision.runtime().ok_or("runtime")?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let (entered, entered_receive) = std::sync::mpsc::sync_channel(1);
    let (release, release_receive) = std::sync::mpsc::sync_channel(1);
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &mut provider {
        owner.core_mut().ok_or("idle core")?.cpu_control = Some(CpuControl {
            phase: ProjectionPhase::Relation,
            stop_at: 3,
            visits: 0,
            entered,
            release: release_receive,
        });
    } else {
        return Err("local owner".into());
    }
    let flag = Arc::new(AtomicBool::new(false));
    let coverage = crate::builtin::admitted_coverage()?;
    let request = backend_engine::Query::new(
        "fixture",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let expected = super::super::search_page(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        coverage,
        &request,
    )
    .map_err(|error| error.to_string())?
    .0;
    let first = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        coverage,
        &request,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        first.1,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        }
    );
    assert_eq!(first.0, expected);
    entered_receive.recv_timeout(Duration::from_secs(5))?;
    let held = runtime.memory_budget().used();
    let calls = std::fs::read_to_string(&fixture.log)?;
    assert!(
        !calls.is_empty(),
        "actual protocol documents preceded relation admission"
    );
    // This is the actual selected-owner flag. Client-disconnect authority is not invented.
    flag.store(true, Ordering::Release);
    provider.poll_local(coordinator.workspace_root(), view.root());
    assert_eq!(
        provider.local_preparation(),
        Some(backend_library::QueryPreparationState::Retiring)
    );
    release.send(())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while provider.local_preparation().is_some() {
        provider.poll_local(coordinator.workspace_root(), view.root());
        if Instant::now() >= deadline {
            return Err("CPU worker failed to join after actual cancellation".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        provider.local_failure(),
        Some(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
        ))
    ));
    assert!(
        runtime.memory_budget().used() < held,
        "projection/key leases released after actual worker joined"
    );
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
        let core = owner.core().ok_or("joined core")?;
        assert!(!core.has_projection());
        assert_eq!(
            core.cpu_control.as_ref().ok_or("CPU gate")?.visits,
            3,
            "no relation input or later CPU phase traversed after refused checkpoint"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&fixture.log)?,
        calls,
        "no query inference or partial answer after cancellation"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn exact_lexical_seed_moves_scores_preserves_page_and_releases_on_refusal() -> TestResult {
    let (coordinator, _, _) = selected_many(30, false)?;
    let query = LocalQuery::prefix("fixture", 20)?;
    let budget = backend_compile::EmbeddingMemoryBudget::new(8 * 1024 * 1024);
    // Fixture supplies the caller-owned composition charge before canonical row reconstruction.
    let row_memory = budget.reserve(1024 * 1024)?;
    let baseline = budget.used();
    let mut local = coordinator.search_local(query.clone())?;
    let expected = local.clone();
    let score_allocation = local.matches.as_ptr();
    let scans = coordinator.corpus.lexical.rank_evaluations();
    let seed = local.take_lexical_seed(&budget, &mut || Ok(()))?;
    assert!(
        local.matches.is_empty(),
        "score allocation moved, never duplicated"
    );
    assert_eq!(
        local.rows, expected.rows,
        "immediate lexical page unchanged"
    );
    assert!(budget.used() > baseline);
    let (restored, seed_memory) = seed.restore(&coordinator, &mut || Ok(()))?;
    assert_eq!(restored.matches.as_ptr(), score_allocation);
    assert_eq!(restored.matches, expected.matches);
    assert_eq!(restored.rows, expected.rows);
    assert_eq!(restored.query, expected.query);
    assert_eq!(restored.total_matches, expected.total_matches);
    assert_eq!(restored.lanes, expected.lanes);
    assert_eq!(
        coordinator.corpus.lexical.rank_evaluations(),
        scans,
        "no duplicate lexical scan"
    );
    drop(restored);
    drop(seed_memory);
    assert_eq!(budget.used(), baseline);
    let mut local = coordinator.search_local(query.clone())?;
    let seed = local.take_lexical_seed(&budget, &mut || Ok(()))?;
    let replacement = selected_many(30, false)?.0;
    assert_eq!(replacement.workspace_root(), coordinator.workspace_root());
    assert_eq!(
        replacement.corpus.view.root(),
        coordinator.corpus.view.root()
    );
    let stale = seed.restore(&replacement, &mut || Ok(()));
    assert!(matches!(
        stale,
        Err(LocalSemanticError::Corpus(QueryError::StaleViewBinding))
    ));
    assert_eq!(budget.used(), baseline);
    let mut local = coordinator.search_local(query.clone())?;
    let seed = local.take_lexical_seed(&budget, &mut || Ok(()))?;
    let flag = AtomicBool::new(false);
    let mut visits = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    let refused = seed.restore(&coordinator, &mut || {
        visits += 1;
        if visits == 3 {
            flag.store(true, Ordering::Release);
        }
        checkpoint(&flag, deadline)
    });
    assert!(matches!(
        refused,
        Err(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
        ))
    ));
    assert_eq!(visits, 3);
    assert_eq!(budget.used(), baseline);
    let mut local = coordinator.search_local(query)?;
    let expected_scores = local.matches.clone();
    let before = local.rows.clone();
    let refused = local
        .take_lexical_seed(&backend_compile::EmbeddingMemoryBudget::new(0), &mut || {
            Ok(())
        });
    assert!(matches!(refused, Err(LocalSemanticError::Memory(_))));
    assert_eq!(
        local.matches, expected_scores,
        "account refusal precedes transfer"
    );
    assert_eq!(local.rows, before);
    let cancellation = AtomicBool::new(false);
    let mut observed = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    let refused = local.take_lexical_seed(&budget, &mut || {
        observed += 1;
        if observed == 3 {
            cancellation.store(true, Ordering::Release);
        }
        checkpoint(&cancellation, deadline)
    });
    assert!(matches!(
        refused,
        Err(LocalSemanticError::Inference(
            EmbeddingExecutableError::Process(backend_compile::ProcessError::Cancelled)
        ))
    ));
    assert_eq!(observed, 3);
    assert_eq!(
        local.matches, expected_scores,
        "partial descriptor construction never transfers scores"
    );
    assert_eq!(budget.used(), baseline, "partial seed descriptors released");
    drop(row_memory);
    assert_eq!(budget.used(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn actual_exclusive_worker_consumes_initial_seed_without_a_second_rank_scan() -> TestResult {
    let fixture = fixture(4096)?;
    let (coordinator, library, view) = selected_many(30, false)?;
    let mut provider = crate::builtin::query::RemoteSemantic::select_owner(
        &fixture.provision,
        &fixture.descriptor,
        crate::builtin::query::RemoteSemantic::Unconfigured,
    );
    let flag = Arc::new(AtomicBool::new(false));
    let request = backend_engine::Query::new(
        "fixture",
        view.root(),
        backend_engine::QueryLimit::new(3).expect("positive fixture query limit"),
    );
    let before = coordinator.corpus.lexical.rank_evaluations();
    let first = super::super::search_page_with_cancellation(
        &coordinator,
        &library,
        &mut provider,
        crate::builtin::admitted_coverage()?,
        &request,
        Arc::clone(&flag),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        first.1,
        SemanticSearchStatus::Unavailable {
            reason: SemanticSearchReason::NoActiveProjection
        }
    );
    assert_eq!(coordinator.corpus.lexical.rank_evaluations(), before + 1);
    let deadline = Instant::now() + Duration::from_secs(8);
    while provider.local_preparation().is_some() {
        provider.poll_local(coordinator.workspace_root(), view.root());
        if Instant::now() >= deadline {
            return Err("seed worker did not join".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if let crate::builtin::query::RemoteSemantic::Local(owner) = &provider {
        assert!(owner.core().ok_or("joined owner")?.has_projection());
        assert!(owner.failure().is_none());
    } else {
        return Err("actual local owner".into());
    }
    assert_eq!(
        coordinator.corpus.lexical.rank_evaluations(),
        before + 1,
        "worker consumed exactly the initial scan seed"
    );
    let invocations = std::fs::read_to_string(&fixture.log)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        invocations.iter().any(|call| call["purpose"] == 1),
        "actual protocol query invoked (fixture, not model proof)"
    );
    Ok(())
}
