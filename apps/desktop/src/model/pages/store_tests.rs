//! The page store's landing rules, one family at a time (R-Open3, phase 2).
//!
//! `PageStore::land` used to unpack each family's value in a closure with a
//! wildcard arm; it now asks the value for the family it is
//! (`PageValue::symbol`, `::search`, …). These tests pin what a landing does
//! with real fixture reads: the value that was read for a key is the value the
//! slot shows, a result that belongs to another family never lands in the
//! slot, and a stale generation lands nowhere.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{FaultCode, ResourceTerminal};
use crate::model::pages::SearchQuery;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, symbol};

fn root() -> VersionedRoot {
    VersionedRoot::synthetic(backend_library::view_state_root(&[("store".to_owned(), "tests".to_owned())]), 1)
}

/// What the fixture owner answers for `request`.
fn read(request: &ReadRequest) -> PageValue {
    let cancel = crate::runtime::CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines, progress: None };
    Fixture.read(request, &context).unwrap_or_else(|failure| panic!("the fixture read {request:?}: {failure:?}"))
}

/// Asks for `key` at the test root (forced, as a retry or "load more" is, so a
/// slot that is already current fetches again) and lands `value` in the slot
/// that asked.
fn land(store: &mut PageStore, key: &PageKey, value: PageValue) -> Landing {
    let generation = store.begin_forced(key, root()).expect("the slot starts a fetch");
    store.land(key, generation, Ok(value))
}

#[test]
fn a_partial_page_is_visible_while_its_generation_reads_and_stale_stages_are_dropped() {
    let mut store = PageStore::default();
    let key = PageKey::Symbol(symbol("RelationLabel"));
    let PageValue::Symbol(page) = read(&ReadRequest::for_key(&key)) else { panic!("symbol page") };
    let first = store.begin(&key, root()).expect("first read");
    assert_eq!(store.stage(&key, first, PageValue::Symbol(page.clone())), Landing::Applied);
    assert_eq!(store.symbol(&symbol("RelationLabel")).loaded_value(), Some(&page));
    assert_eq!(store.inflight(&key), Some(first), "the worker still owns the read");
    assert_eq!(store.stage(&key, first, PageValue::Symbol(page.clone())), Landing::Unchanged);
    assert_eq!(store.cancel(&key), Some(first));
    let second = store.begin(&key, root()).expect("return to page");
    assert_ne!(first, second);
    assert_eq!(store.stage(&key, first, PageValue::Symbol(page.clone())), Landing::Superseded);
    assert_eq!(store.land(&key, second, Ok(PageValue::Symbol(page))), Landing::Applied);
    assert!(store.symbol(&symbol("RelationLabel")).is_loaded());
}

#[test]
fn a_value_read_for_a_key_is_the_value_its_slot_shows() {
    let mut store = PageStore::default();

    let symbol_key = PageKey::Symbol(symbol("RelationLabel"));
    let PageValue::Symbol(page) = read(&ReadRequest::for_key(&symbol_key)) else { panic!("a symbol read answers a symbol page") };
    assert_eq!(land(&mut store, &symbol_key, PageValue::Symbol(page.clone())), Landing::Applied);
    assert_eq!(store.symbol(&symbol("RelationLabel")).loaded_value(), Some(&page), "the page the slot shows is the one read");

    let package = PackageRef::parse(PACKAGE).expect("package");
    let package_key = PageKey::Package(package.clone());
    let PageValue::Package(dossier) = read(&ReadRequest::for_key(&package_key)) else { panic!("a package read answers a dossier") };
    assert_eq!(land(&mut store, &package_key, PageValue::Package(dossier.clone())), Landing::Applied);
    assert_eq!(store.package(&package).loaded_value(), Some(&dossier));

    let PageValue::Orbit(orbit) = read(&ReadRequest::Orbit) else { panic!("an orbit read answers the orbit model") };
    assert_eq!(land(&mut store, &PageKey::Orbit, PageValue::Orbit(orbit.clone())), Landing::Applied);
    assert_eq!(store.orbit().loaded_value(), Some(&orbit));

    let PageValue::Health(health) = read(&ReadRequest::Health) else { panic!("a health read answers the health model") };
    assert_eq!(land(&mut store, &PageKey::Health, PageValue::Health(health.clone())), Landing::Applied);
    assert_eq!(store.health().loaded_value(), Some(&health));
}

#[test]
fn a_search_continuation_is_appended_to_the_page_before_it_and_a_first_page_replaces() {
    let mut store = PageStore::default();
    let query = SearchQuery::new("RelationLabel", 10).expect("query");
    let key = PageKey::Search(query.clone());
    let PageValue::Search(first) = read(&ReadRequest::Search(query.clone())) else { panic!("a search read answers a page") };
    assert_eq!(land(&mut store, &key, PageValue::Search(first.clone())), Landing::Applied);
    assert_eq!(store.search(&query).loaded_value().map(|page| page.rows.len()), Some(first.rows.len()));

    // "Load more": the same rows come back as a continuation; they are appended, ranked after the first page.
    assert_eq!(land(&mut store, &key, PageValue::SearchMore(first.clone())), Landing::Applied);
    let shown = store.search(&query);
    let ranks = shown.loaded_value().map(|page| page.rows.iter().map(|row| row.rank).collect::<Vec<_>>());
    assert_eq!(ranks, Some(vec![0, 1]), "the continuation's rows follow the first page's, in order");

    // A fresh first page (a new search of the same text) replaces what was there.
    assert_eq!(land(&mut store, &key, PageValue::Search(first.clone())), Landing::Applied);
    assert_eq!(store.search(&query).loaded_value().map(|page| page.rows.len()), Some(first.rows.len()), "a first page replaces, it does not append");
}

#[test]
fn a_result_from_another_family_is_a_typed_fault_and_never_lands_in_the_slot() {
    let mut store = PageStore::default();
    let key = PageKey::Symbol(symbol("RelationLabel"));
    let PageValue::Orbit(orbit) = read(&ReadRequest::Orbit) else { panic!("an orbit read answers the orbit model") };
    let generation = store.begin(&key, root()).expect("the slot starts a fetch");
    // The orbit model lands in a declaration's slot: the store must refuse it in words, not show it as a page.
    assert_eq!(store.land(&key, generation, Ok(PageValue::Orbit(orbit))), Landing::Applied, "the failure is the landing");
    let resource = store.symbol(&symbol("RelationLabel"));
    assert!(resource.loaded_value().is_none(), "no page was invented for the slot");
    assert_eq!(
        resource.terminal(),
        &ResourceTerminal::Fault(ErrorValue::new(FaultCode::Protocol, "a read result landed in a slot of another page family")),
        "the slot says what happened"
    );
}

#[test]
fn a_result_for_a_generation_that_was_superseded_lands_nowhere() {
    let mut store = PageStore::default();
    let key = PageKey::Symbol(symbol("RelationLabel"));
    let PageValue::Symbol(page) = read(&ReadRequest::for_key(&key)) else { panic!("a symbol read answers a symbol page") };
    let old = store.begin(&key, root()).expect("first fetch");
    // A retry supersedes the running fetch.
    let new = store.begin_forced(&key, root()).expect("forced fetch");
    assert_ne!(old, new, "a new generation owns the slot");
    assert_eq!(store.land(&key, old, Ok(PageValue::Symbol(page.clone()))), Landing::Superseded, "the old fetch's answer is dropped");
    assert!(store.symbol(&symbol("RelationLabel")).loaded_value().is_none(), "and shows nothing");
    assert_eq!(store.land(&key, new, Ok(PageValue::Symbol(page.clone()))), Landing::Applied);
    assert_eq!(store.symbol(&symbol("RelationLabel")).loaded_value(), Some(&page));
}
