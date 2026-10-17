//! These exercise the actual DataStore, task timer, pool landing and ownership
//! gates with a deterministic reader. They are not live owner/GUI acceptance.

use super::*;
use crate::core::{ErrorValue, FaultCode};
use crate::model::pages::{DeclRef, Gap, GapReason, Known, MatchReason, SearchRow};
use crate::runtime::reads::{PageReader, ReadContext, ReadLimits};
use gpui::TestAppContext;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

struct Reader {
    mode: Arc<AtomicU8>,
    requests: Arc<Mutex<Vec<ReadRequest>>>,
}

fn query() -> SearchQuery {
    SearchQuery::new("Document", 1).expect("query")
}
fn names_page() -> SearchPage {
    let declaration = DeclRef::from_label(
        "/workspace/docs::models.py:994::Document",
        Some(backend_library::symbol_key(
            "opaque actual selected fixture",
        )),
        None,
        None,
    )
    .expect("selected declaration");
    SearchPage {
        query: Arc::clone(&query().text),
        rows: vec![SearchRow {
            rank: 0,
            decl: declaration,
            package: Some(Arc::from("/workspace/docs")),
            score: Known::Known(1),
            signature: Known::Unknown(Gap::new(GapReason::NotCaptured, "not captured")),
            snippet: None,
            reason: MatchReason::ExactName,
        }]
        .into(),
        coverage: backend_present::CoverageLine::new(&[], Some(1)).with_published_names(
            backend_library::PublishedSearchReadiness {
                full_search: backend_library::FullSearchReadiness::Preparing,
                semantic: backend_library::PublishedSemanticReadiness::Pending,
            },
        ),
        next: Some(crate::model::pages::SearchContinuation {
            cursor: backend_library::PageContinuation::from_cursor(backend_library::Cursor::new()),
            worker: 0,
        }),
    }
}
fn find(page: SearchPage) -> BrowseValue {
    let answers = Known::Known(page);
    let prepared = Arc::new(crate::runtime::browse_views::prepare_find(
        "Document",
        &answers,
        &[],
        &Known::Known(()),
    ));
    BrowseValue::Find(Arc::new(crate::model::browse::FindModel {
        answers,
        packages: Arc::from([]),
        package_coverage: Known::Known(()),
        prepared,
    }))
}
impl PageReader for Reader {
    fn read(
        &mut self,
        request: &ReadRequest,
        _: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.mode.load(Ordering::Acquire) == 3 {
            assert!(Instant::now() < deadline, "test did not release the read");
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.mode.load(Ordering::Acquire) == 2 {
            return Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Protocol,
                "actual read refusal fixture",
            )));
        }
        let mut page = names_page();
        if self.mode.load(Ordering::Acquire) == 1 {
            page.coverage = backend_present::CoverageLine::new(&[], Some(1));
            page.next = None;
        }
        match request {
            ReadRequest::Search(_) => Ok(PageValue::Search(page)),
            ReadRequest::Browse(crate::model::browse::BrowseKey::Find(_)) => {
                Ok(PageValue::Browse(find(page)))
            }
            _ => panic!("refresh must use a fresh initial query, never SearchMore: {request:?}"),
        }
    }
}
struct Rig {
    store: Entity<DataStore>,
    mode: Arc<AtomicU8>,
    requests: Arc<Mutex<Vec<ReadRequest>>>,
}
fn new_rig(cx: &mut TestAppContext) -> Rig {
    cx.executor().allow_parking();
    let mode = Arc::new(AtomicU8::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let reader_mode = Arc::clone(&mode);
    let reader_requests = Arc::clone(&requests);
    let pool = ReadPool::start_with(1, ReadLimits::DEFAULT, move |_| Reader {
        mode: Arc::clone(&reader_mode),
        requests: Arc::clone(&reader_requests),
    })
    .expect("pool");
    let snapshot = Arc::new(AppSnapshot::empty(VersionedRoot::synthetic(
        backend_library::view_state_root(&[("source".to_owned(), "current".to_owned())]),
        1,
    )));
    let store = cx.update(|cx| {
        cx.new(|cx| {
            let mut store = DataStore::new(snapshot, Some(pool));
            store.start(cx);
            store
        })
    });
    Rig {
        store,
        mode,
        requests,
    }
}
impl Rig {
    fn until(&self, cx: &mut TestAppContext, predicate: impl Fn(&DataStore) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.store.read_with(cx, |store, _| predicate(store)) {
            cx.run_until_parked();
            assert!(Instant::now() < deadline, "DataStore did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
        cx.run_until_parked();
    }
    fn start_names(&self, cx: &mut TestAppContext, key: PageKey) {
        self.store.update(cx, |store, cx| {
            store.ensure(key.clone(), cx);
        });
        self.until(cx, |store| {
            store.search_revalidations.contains_key(&key) && store.pages.inflight(&key).is_none()
        });
    }
    fn advance(&self, cx: &mut TestAppContext, delay: Duration) {
        cx.executor().advance_clock(delay);
        cx.run_until_parked();
    }
    fn count(&self) -> usize {
        self.requests.lock().expect("requests").len()
    }
}

#[gpui::test]
fn current_search_and_find_upgrade_without_publication_or_cursor_promotion(
    cx: &mut TestAppContext,
) {
    let rig = new_rig(cx);
    let search = PageKey::Search(query()); // Ask's auxiliary key is not focused.
    let browse = PageKey::Browse(crate::model::browse::BrowseKey::Find(query()));
    rig.start_names(cx, search.clone());
    rig.start_names(cx, browse.clone());
    let (root, selected, continuation) = rig.store.read_with(cx, |store, _| {
        let page = store.search(&query());
        let page = page.loaded_value().expect("useful partial");
        assert!(page.coverage.published_names().is_some());
        assert_eq!(store.search_revalidations.len(), 2);
        (store.snapshot.key(), page.rows[0].decl.clone(), page.next)
    });
    assert!(
        continuation.is_some(),
        "existing names cursor remains available until replacement"
    );
    assert_eq!(rig.count(), 2);
    rig.mode.store(3, Ordering::Release); // Hold the fresh initial read.
    rig.advance(cx, FIRST_REVALIDATION);
    rig.until(cx, |store| store.pages.inflight(&search).is_some());
    rig.store.read_with(cx, |store, _| {
        let resource = store.search(&query());
        assert_eq!(
            resource
                .loaded_value()
                .expect("partial rows stay visible")
                .rows[0]
                .decl,
            selected
        );
        assert_eq!(store.search_revalidations.len(), 2);
    });
    rig.mode.store(1, Ordering::Release);
    rig.until(cx, |store| {
        store.search_revalidations.is_empty() && store.pool_activity().is_idle()
    });
    rig.store.read_with(cx, |store, _| {
        assert_eq!(
            store.snapshot.key(),
            root,
            "no invented producer publication"
        );
        let resource = store.search(&query());
        let page = resource.loaded_value().expect("fresh full reply");
        assert_eq!(page.rows[0].decl, selected);
        assert!(page.coverage.published_names().is_none());
        assert!(page.next.is_none());
        let resource = store
            .pages
            .browse(&crate::model::browse::BrowseKey::Find(query()));
        let BrowseValue::Find(find) = resource.loaded_value().expect("Find") else {
            panic!("Find")
        };
        assert!(
            find.answers
                .known()
                .expect("answers")
                .coverage
                .published_names()
                .is_none()
        );
    });
    assert_eq!(rig.count(), 4);
    rig.advance(cx, Duration::from_secs(20));
    assert_eq!(rig.count(), 4, "completed reads do not poll");
}

#[gpui::test]
fn dismissed_auxiliary_search_and_navigation_cancel_even_settled_timers(cx: &mut TestAppContext) {
    let rig = new_rig(cx);
    let key = PageKey::Search(query());
    rig.start_names(cx, key.clone());
    let generation = rig
        .store
        .read_with(cx, |store, _| store.search_revalidations[&key].generation);
    rig.store.update(cx, |store, cx| {
        assert!(store.pages.inflight(&key).is_none());
        store.cancel_unfocused_search(&query(), cx);
        assert!(store.search_revalidations.is_empty());
        store.revalidate_search(&key, generation, cx); // Late cancelled callback.
    });
    rig.advance(cx, Duration::from_secs(20));
    assert_eq!(rig.count(), 1);
    // Reopening the same auxiliary query revalidates its retained partial page.
    rig.start_names(cx, key.clone());
    assert_eq!(rig.count(), 2);
    rig.store.update(cx, |store, cx| {
        store.focus(vec![key.clone()], cx);
        store.focus(Vec::new(), cx);
        assert!(store.search_revalidations.is_empty());
    });
    rig.advance(cx, Duration::from_secs(20));
    assert_eq!(rig.count(), 2);
}

#[gpui::test]
fn stale_root_owner_stamp_and_generation_cannot_restart_a_search(cx: &mut TestAppContext) {
    for change in ["root", "owner", "stamp", "generation"] {
        let rig = new_rig(cx);
        let key = PageKey::Search(query());
        rig.start_names(cx, key.clone());
        rig.store.update(cx, |store, cx| {
            let generation = store.search_revalidations[&key].generation;
            match change {
                "root" => {
                    store.snapshot = Arc::new(AppSnapshot::empty(VersionedRoot::synthetic(
                        backend_library::view_state_root(&[("source".into(), "changed".into())]),
                        2,
                    )))
                }
                "owner" => {
                    store.owner = OwnerLink::behind(crate::runtime::owner::OwnerGate::ready(
                        store.snapshot.key(),
                        crate::model::ServiceMode::Attached,
                    ))
                }
                "stamp" | "generation" => {
                    let next = store
                        .pages
                        .begin_forced(&key, store.snapshot.key())
                        .expect("admitted")
                        .expect("generation");
                    let mut page = names_page();
                    if change == "stamp" {
                        page.coverage = backend_present::CoverageLine::new(&[], Some(1));
                    }
                    assert_eq!(
                        store.pages.land(&key, next, Ok(PageValue::Search(page))),
                        Landing::Applied
                    );
                    if change == "generation" {
                        store.schedule_search_revalidation(key.clone(), next, true, cx);
                        store.revalidate_search(&key, generation, cx);
                        assert_eq!(
                            store.search_revalidations[&key].generation, next,
                            "old callback cannot remove new timer"
                        );
                        store.cancel_key(&key, cx);
                        return;
                    }
                }
                _ => unreachable!(),
            }
            store.revalidate_search(&key, generation, cx);
            assert!(store.search_revalidations.is_empty(), "stale {change}");
        });
        rig.advance(cx, Duration::from_secs(20));
        assert_eq!(rig.count(), 1, "stale {change} issued no fresh query");
    }
}

#[gpui::test]
fn pause_resumes_one_timer_and_close_or_fault_stops_revalidation(cx: &mut TestAppContext) {
    let rig = new_rig(cx);
    let key = PageKey::Search(query());
    rig.start_names(cx, key.clone());
    rig.store.update(cx, |store, cx| {
        store.set_close_paused(true, cx);
        assert!(store.search_revalidations[&key].task.is_none());
    });
    rig.advance(cx, Duration::from_secs(20));
    assert_eq!(rig.count(), 1);
    rig.mode.store(2, Ordering::Release);
    rig.store.update(cx, |store, cx| {
        store.set_close_paused(false, cx);
    });
    rig.advance(cx, FIRST_REVALIDATION);
    rig.until(cx, |store| {
        store.search_revalidations.is_empty() && store.pool_activity().is_idle()
    });
    assert_eq!(rig.count(), 2);
    rig.store.read_with(cx, |store, _| {
        let resource = store.search(&query());
        assert!(
            resource.loaded_value().is_some(),
            "partial rows retained after fault"
        );
        assert!(matches!(resource.terminal(), ResourceTerminal::Fault(_)));
    });
    rig.advance(cx, Duration::from_secs(20));
    assert_eq!(rig.count(), 2, "fault is terminal");

    let closed = new_rig(cx);
    closed.start_names(cx, key);
    closed.store.update(cx, |store, _| {
        let _ = store.commit_close();
    });
    closed.advance(cx, Duration::from_secs(20));
    assert_eq!(closed.count(), 1);
    closed.store.read_with(cx, |store, _| {
        assert!(store.search_revalidations.is_empty())
    });
}

#[gpui::test]
fn repeated_pending_replies_use_one_timer_and_capped_backoff(cx: &mut TestAppContext) {
    let rig = new_rig(cx);
    let key = PageKey::Search(query());
    rig.start_names(cx, key.clone());
    let mut count = 1;
    for millis in [200, 400, 800, 1600, 2000, 2000] {
        rig.store.update(cx, |store, cx| {
            assert_eq!(store.search_revalidations.len(), 1);
            assert_eq!(
                store.search_revalidations[&key].delay,
                Duration::from_millis(millis)
            );
            assert!(store.search_revalidations[&key].task.is_some());
            store.ensure(key.clone(), cx);
            store.ensure(key.clone(), cx);
        });
        assert_eq!(
            rig.count(),
            count,
            "ordinary redraws do not duplicate the lease"
        );
        rig.advance(cx, Duration::from_millis(millis));
        count += 1;
        rig.until(cx, |store| {
            rig.count() == count
                && store.pool_activity().is_idle()
                && store
                    .search_revalidations
                    .get(&key)
                    .is_some_and(|lease| lease.task.is_some())
        });
    }
    rig.mode.store(1, Ordering::Release);
    rig.advance(cx, MAX_REVALIDATION);
    rig.until(cx, |store| {
        store.search_revalidations.is_empty() && store.pool_activity().is_idle()
    });
    assert_eq!(rig.count(), count + 1);
    assert!(
        rig.requests
            .lock()
            .expect("requests")
            .iter()
            .all(|request| matches!(request, ReadRequest::Search(current) if current == &query()))
    );
}
