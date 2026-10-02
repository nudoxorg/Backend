//! The browsing pages' bodies: the product's words, drawn by
//! `facet::browse`. Your tree is the Library page.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf, Pages};
use crate::core::{
    ReadHoldReason, ResourceAdmission, ResourceTerminal, UnavailableReason, VersionedRoot,
    admit_resource,
};
use crate::runtime::store::{DataStore, RouteDependencies};
use crate::model::browse::{
    BrowseKey, BrowseValue, CompareModel, FindModel, TreeDestination, TreeInventoryLink, TreeModel, TreeRoleLinks,
};
use crate::model::pages::{PackageRef, PageKey, SearchQuery, SymbolRef};
use crate::navigation::{BrowseRoute, CompareSet, Intent, OrbitRoute, Route, View};
use crate::shell::kit::{package_route, symbol_route, symbol_view_route};
use crate::shell::reader::Reader;
use facet::browse::library::{InventoryHandle, ReleaseHandle};
use facet::browse::{LibraryActions, LibraryModel, library};
use gpui::{App, AppContext as _, Context, InteractiveElement, ParentElement, SharedString, Styled, Window, div};
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn body(
    route: &BrowseRoute,
    store: &Pages,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let key = BrowseKey::from(route);
    let resource = store.browse(&key);
    if matches!(route, BrowseRoute::Tree(_)) { return tree_body(route, ctx, cx); }
    // Find owns a live editing engine, including while a newly admitted
    // query is being read. Replacing it with a generic loading page loses
    // IME composition, focus, the debounce and the package hand.
    if matches!(route, BrowseRoute::Find(_) | BrowseRoute::FindHome) {
        let (resource, owner_serving) = {
            let live = ctx.links.store.read(cx);
            (live.pages().browse(&key), live.owner_serving())
        };
        let reading = admit_resource(&resource, ctx.links.snapshot(cx).key(), owner_serving);
        let value = match reading.current_value().or_else(|| reading.retained_value()) {
            Some(BrowseValue::Find(value)) => Some(value.as_ref()),
            _ => None,
        };
        let model = match value {
            Some(value) => Arc::clone(&value.prepared),
            None => Arc::new(facet::browse::find::Model {
                query: match route {
                    BrowseRoute::Find(query) => query.text.to_string().into(),
                    _ => "".into(),
                },
                candidates: vec![],
                loose: vec![],
                coverage: vec![],
                more_answers: false,
                loading: true,
            }),
        };
        // Find's reveal state lives in its native component. Its painted text
        // probes are the authority for what is visible in this frame.
        let admission = find_admission(&reading);
        let source = FindActionSource::new(route, ctx, cx);
        let mut actions = find_actions(ctx, cx, &source);
        if matches!(resource.terminal(), ResourceTerminal::Fault(_)) {
            actions.retry = Some(Rc::new(move |window, cx| {
                if let Err(reason) = source.failed_retry(cx) {
                    find_action_notice(reason, window, cx);
                    return;
                }
                source.links.retry(PageKey::Browse(source.key.clone()), cx);
            }));
        }
        let leaves = vec![Leaf::new(
            facet::browse::find::find("find", model, actions, &ctx.measure)
                .admission(admission)
                .active(ctx.native_input_active && ctx.links.snapshot(cx).overlay().is_none()),
        )];
        return leaves;
    }
    match shown(&resource) {
        Shown::Ready(BrowseValue::Tree(_)) => tree_body(route, ctx, cx),
        Shown::Ready(BrowseValue::Compare(compare)) => {
            let model = Arc::clone(&compare.prepared);
            let actions = compare_actions(route, ctx, cx);
            vec![Leaf::new(facet::browse::compare::compare(
                "compare",
                model,
                actions,
                &ctx.measure,
            ))]
        }
        other => {
            let (name, _) = route.here();
            not_ready(&other, &PageKey::Browse(key), &name, ctx, cx)
        }
    }
}

/// A retained Resource value is visual evidence, never current authority.
fn find_admission<T>(reading: &ResourceAdmission<'_, T>) -> facet::browse::find::ReadAdmission {
    use facet::browse::find::ReadAdmission;
    match reading {
        ResourceAdmission::Current(_) => ReadAdmission::Current,
        ResourceAdmission::Retained { reason, .. } | ResourceAdmission::Pending(reason) => {
            ReadAdmission::Retained(match reason {
                ReadHoldReason::OwnerUnavailable => "The index owner is not serving this reading. Previous results, if shown, are read-only.",
                ReadHoldReason::AuthorityChanged => "Previous index reading; waiting for the current producer authority. Result actions are unavailable.",
                ReadHoldReason::Reading | ReadHoldReason::NotReady => "Waiting for the current query. Previous results, if shown, are read-only.",
            }.into())
        }
        ResourceAdmission::Failed { terminal, .. } => ReadAdmission::Failed(match terminal {
            ResourceTerminal::Fault(error) => format!("Find stopped: {}. Previous results, if shown, are read-only.", error.message()).into(),
            ResourceTerminal::Unavailable(UnavailableReason::Unsupported) => "Find is not served by this owner. Previous results, if shown, are read-only.".into(),
            ResourceTerminal::Unavailable(UnavailableReason::OutOfScope) => "Find is outside this owner's scope. Previous results, if shown, are read-only.".into(),
            ResourceTerminal::Complete | ResourceTerminal::Partial => unreachable!("shared admission only classifies terminal failures as Failed"),
        }),
    }
}

fn query_input(text: &str) -> facet::browse::find::QueryInput {
    use facet::browse::find::QueryInput;
    if text.trim().is_empty() {
        return QueryInput::Blank;
    }
    match SearchQuery::new(text, SearchQuery::DEFAULT_LIMIT) {
        Ok(_) => QueryInput::Valid,
        Err(error) => QueryInput::Invalid(format!("This query was not admitted: {error}. Remove the invalid character to search; the current route has not changed.").into()),
    }
}

fn symbol_routability(key: &SharedString) -> facet::browse::find::Routability {
    use facet::browse::find::Routability;
    match SymbolRef::new(key) {
        Ok(symbol) if symbol.package().is_some() => Routability::Available,
        Ok(_) => Routability::Unavailable("This declaration has no addressable package in the owner reply. Its recorded facts can be read, but its page and code are unavailable.".into()),
        Err(error) => Routability::Unavailable(format!("This declaration's address was not admitted: {error}.").into()),
    }
}

/// A painted control carries the route and producer authority that supplied
/// it. Every source action borrows the *current* store value again at the
/// event boundary, after a refresh or route change may have replaced it.
#[derive(Clone)]
struct FindActionSource {
    links: super::super::region::Links,
    route: BrowseRoute,
    key: BrowseKey,
    root: VersionedRoot,
}

impl FindActionSource {
    fn new(route: &BrowseRoute, ctx: &Ctx<'_>, cx: &App) -> Self {
        Self {
            links: ctx.links.clone(),
            route: route.clone(),
            key: route.into(),
            root: ctx.links.snapshot(cx).key(),
        }
    }

    fn resource<R>(
        &self,
        cx: &App,
        use_resource: impl FnOnce(
            &crate::core::Resource<BrowseValue>,
            VersionedRoot,
            bool,
        ) -> Result<R, &'static str>,
    ) -> Result<R, &'static str> {
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        if snapshot.route() != &Route::Orbit(OrbitRoute::Browse(self.route.clone()))
            || snapshot.overlay().is_some()
            || !snapshot.key().same_authority(self.root)
        {
            return Err(
                "Find changed before that action. Choose a result from the current reading.",
            );
        }
        let resource = store.pages().browse(&self.key);
        use_resource(&resource, snapshot.key(), store.owner_serving())
    }

    fn current<R>(
        &self,
        cx: &App,
        member: impl FnOnce(&FindModel) -> Option<R>,
    ) -> Result<R, &'static str> {
        self.resource(cx, |resource, root, serving| {
            let ResourceAdmission::Current(BrowseValue::Find(find)) =
                admit_resource(resource, root, serving)
            else {
                return Err(
                    "Find results changed before that action. Wait for the current reading.",
                );
            };
            let expected = match &self.route {
                BrowseRoute::Find(query) => query.text.as_ref(),
                BrowseRoute::FindHome => "",
                _ => return Err("Find is no longer the current page."),
            };
            if find.prepared.query.as_ref() != expected {
                return Err("Find results belong to another query. Wait for the current reading.");
            }
            member(find).ok_or("That result is no longer in the current Find reading.")
        })
    }

    fn failed_retry(&self, cx: &App) -> Result<(), &'static str> {
        self.resource(cx, |resource, root, serving| {
            if matches!(
                admit_resource(resource, root, serving),
                ResourceAdmission::Failed {
                    terminal: ResourceTerminal::Fault(_),
                    ..
                }
            ) {
                Ok(())
            } else {
                Err("That Find failure is no longer current. Retry the failure now shown.")
            }
        })
    }
}

fn find_action_notice(reason: &str, window: &mut Window, cx: &mut App) {
    facet::overlay::toast::show(
        facet::overlay::toast::Toast::new(reason).voice(facet::tokens::Voice::Coral),
        window,
        cx,
    );
}

fn find_symbol_action(
    source: FindActionSource,
    code: bool,
) -> Rc<dyn Fn(SharedString, &mut Window, &mut App)> {
    Rc::new(move |key, window, cx| {
        let route = source.current(cx, |find| {
            let present = find
                .prepared
                .loose
                .iter()
                .chain(
                    find.prepared
                        .candidates
                        .iter()
                        .flat_map(|candidate| candidate.answers.iter()),
                )
                .any(|answer| answer.key == key && (!code || answer.source_available));
            if !present {
                return None;
            }
            let symbol = SymbolRef::new(&key).ok()?;
            let package = symbol.package()?;
            if code {
                symbol_view_route(package.as_str(), &symbol, View::Code, None)
            } else {
                symbol_route(package.as_str(), &symbol)
            }
        });
        match route {
            Ok(route) => source.links.dispatch(Intent::Navigate(route), cx),
            Err(reason) => find_action_notice(reason, window, cx),
        }
    })
}

fn find_package_action(
    source: FindActionSource,
) -> Rc<dyn Fn(SharedString, &mut Window, &mut App)> {
    Rc::new(move |key, window, cx| {
        let route = source.current(cx, |find| {
            find.prepared
                .candidates
                .iter()
                .any(|candidate| candidate.key == key)
                .then(|| {
                    PackageRef::parse(&key)
                        .ok()
                        .and_then(|package| package_route(&package))
                })
                .flatten()
        });
        match route {
            Ok(route) => source.links.dispatch(Intent::Navigate(route), cx),
            Err(reason) => find_action_notice(reason, window, cx),
        }
    })
}

fn tree_body(route: &BrowseRoute, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let place = Route::Orbit(OrbitRoute::Browse(route.clone()));
    let plan = RouteDependencies::new(&place, None);
    let live = ctx.links.store.read(cx);
    let key = BrowseKey::from(route);
    let resource = live.pages().browse(&key);
    let root = live.snapshot().key();
    let serving = live.owner_serving();
    let current = plan.current_tree(live);
    if let Some(tree) = current {
        let model = library_model(tree.model(), ctx);
        let dependency = tree.native_dependency();
        return vec![Leaf::new(library(
            "library", model,
            LibraryActions {
                open_package: open_library_package_action(plan.clone(), dependency.clone(), ctx, cx),
                open_inventory: open_library_inventory_action(plan, dependency, ctx, cx),
                return_focus: ctx.native_return_focus(cx),
            },
            &ctx.measure, Rc::clone(&ctx.library_state), ctx.place_key, ctx.active,
        ))];
    }
    let admission = admit_resource(&resource, root, serving);
    match &admission {
        ResourceAdmission::Retained { value, .. } | ResourceAdmission::Failed { retained: Some(value), .. } => {
            let words = ctx.say("Earlier Library observations are retained while the current owner checks this tree. Their controls are unavailable.");
            let mut leaves = vec![Leaf::new(crate::shell::kit::quiet(words, &ctx.measure, ctx.palette))];
            if let BrowseValue::Tree(tree) = value {
                let mut names = div().flex().flex_col();
                for row in tree.inventory_links.iter().take(8) {
                    let name = ctx.say(format!("{} {} · earlier observation", row.name, row.version));
                    names = names.child(div().role(gpui::Role::Label).aria_label(name.clone())
                        .child(crate::shell::kit::text(facet::tokens::ty::BODY, &ctx.measure, ctx.palette.ink2).child(name)));
                }
                leaves.push(Leaf::new(names));
            }
            if let ResourceAdmission::Failed { terminal, .. } = &admission {
                match terminal {
                    ResourceTerminal::Fault(error) => leaves.extend(not_ready::<BrowseValue>(&Shown::Fault(error), &PageKey::Browse(key), "Library", ctx, cx)),
                    ResourceTerminal::Unavailable(reason) => leaves.extend(not_ready::<BrowseValue>(&Shown::Unavailable(reason, None), &PageKey::Browse(key), "Library", ctx, cx)),
                    ResourceTerminal::Complete | ResourceTerminal::Partial => {}
                }
            }
            leaves
        }
        ResourceAdmission::Failed { terminal: ResourceTerminal::Fault(error), .. } => not_ready::<BrowseValue>(&Shown::Fault(error), &PageKey::Browse(key), "Library", ctx, cx),
        ResourceAdmission::Failed { terminal: ResourceTerminal::Unavailable(reason), .. } => not_ready::<BrowseValue>(&Shown::Unavailable(reason, None), &PageKey::Browse(key), "Library", ctx, cx),
        ResourceAdmission::Current(_) => vec![Leaf::new(crate::shell::kit::quiet("The Library reply changed shape.", &ctx.measure, ctx.palette))],
        ResourceAdmission::Pending(_) | ResourceAdmission::Failed { .. } => not_ready::<BrowseValue>(&Shown::Pending, &PageKey::Browse(key), "Library", ctx, cx),
    }
}

fn observed_package_route(plan: &RouteDependencies, store: &DataStore, package: &PackageRef) -> Option<Route> {
    let mut route = package_route(package)?;
    if let Route::Package(target) = &mut route {
        if crate::navigation::CargoSourceRoute::supports_package(&target.package) {
            target.cargo = Some(plan.current_cargo_package(store, package)?.context().clone());
        }
    }
    Some(route)
}

fn open_library_package_action(
    plan: RouteDependencies,
    dependency: (PageKey, crate::model::pages::Stamp),
    ctx: &Ctx<'_>, cx: &mut Context<Reader>,
) -> Rc<dyn Fn(ReleaseHandle, &mut gpui::Window, &mut App)> {
    let links = ctx.links.clone();
    let state = Rc::clone(&ctx.library_state);
    let place_key = ctx.place_key;
    let guard = ctx.native_dependency_guard(dependency, cx);
    Rc::new(move |handle, _, app| {
        if !guard(app) { return; }
        let store = links.store.read(app);
        let Some(tree) = plan.current_tree(store) else { return; };
        let Some(package) = typed_library_release(&tree.model().links, &handle) else { return; };
        let Some(route) = observed_package_route(&plan, store, package) else { return; };
        state.borrow_mut().remember_open(handle, place_key);
        links.dispatch(Intent::Navigate(route), app);
    })
}

fn open_library_inventory_action(
    plan: RouteDependencies,
    dependency: (PageKey, crate::model::pages::Stamp),
    ctx: &Ctx<'_>, cx: &mut Context<Reader>,
) -> Rc<dyn Fn(InventoryHandle, &mut gpui::Window, &mut App)> {
    let links = ctx.links.clone();
    let state = Rc::clone(&ctx.library_state);
    let place_key = ctx.place_key;
    let guard = ctx.native_dependency_guard(dependency, cx);
    Rc::new(move |handle, _, app| {
        if !guard(app) { return; }
        let store = links.store.read(app);
        let Some(tree) = plan.current_tree(store) else { return; };
        let Some(package) = typed_library_inventory(&tree.model().inventory_links, &handle) else { return; };
        let Some(route) = observed_package_route(&plan, store, package) else { return; };
        state.borrow_mut().remember_inventory_open(handle, place_key);
        links.dispatch(Intent::Navigate(route), app);
    })
}

fn typed_library_inventory<'a>(
    rows: &'a [TreeInventoryLink],
    handle: &InventoryHandle,
) -> Option<&'a PackageRef> {
    let (at, key) = handle.address();
    let row = rows.get(at).filter(|row| row.key.as_ref() == key)?;
    match &row.destination {
        TreeDestination::Open(package) => Some(package),
        TreeDestination::Unavailable(_) => None,
    }
}

fn typed_library_release(roles: &[TreeRoleLinks], handle: &ReleaseHandle) -> Option<&PackageRef> {
    let (role, row, release) = handle.positions();
    let (role_key, row_key, release_key) = handle.keys();
    let role = roles
        .get(role)
        .filter(|role| role.role.as_str() == role_key)?;
    let row = role
        .rows
        .get(row)
        .filter(|row| row.key.as_ref() == row_key)?;
    let release = row
        .releases
        .get(release)
        .filter(|release| release.key.as_ref() == release_key)?;
    match &release.destination {
        TreeDestination::Open(package) => Some(package),
        TreeDestination::Unavailable(_) => None,
    }
}

fn find_actions(
    ctx: &Ctx<'_>,
    cx: &mut Context<Reader>,
    source: &FindActionSource,
) -> facet::browse::find::Actions {
    let expected = match &source.route {
        BrowseRoute::Find(query) => Some(query.clone()),
        _ => None,
    };
    let refine_links = ctx.links.clone();
    let compare_source = source.clone();
    let compare_reader = cx.weak_entity();
    let reader = cx.weak_entity();
    let acquire = Some(crate::shell::acquire::add_actions(
        &ctx.links,
        cx.entity_id(),
    ));
    facet::browse::find::Actions {
        acquire,
        retry: None,
        scroll: ctx.reader_scroll.clone(),
        initial_held: ctx.find_held.clone(),
        persist_held: Rc::new(move |held, cx| {
            let _ = reader.update(cx, |reader, cx| reader.set_find_held(held, cx));
        }),
        query_input: Rc::new(query_input),
        refine: Rc::new(move |text, cx| {
            let query = if text.trim().is_empty() {
                None
            } else {
                match SearchQuery::new(&text, SearchQuery::DEFAULT_LIMIT) {
                    Ok(query) => Some(query),
                    // The Find field exposes this same invalid-input state;
                    // an admission error must never mean clearing the route.
                    Err(_) => return,
                }
            };
            refine_links.dispatch(
                Intent::RefineFind {
                    expected: expected.clone(),
                    query,
                },
                cx,
            );
        }),
        symbol_routability: Rc::new(symbol_routability),
        open_symbol: find_symbol_action(source.clone(), false),
        open_code: find_symbol_action(source.clone(), true),
        open_package: find_package_action(source.clone()),
        compare: Rc::new(move |keys, window, cx| {
            if !compare_reader.upgrade().is_some_and(|reader| {
                reader
                    .read(cx)
                    .holds_find_packages_at(&keys, compare_source.root)
            }) {
                find_action_notice(
                    "The held packages changed with the Find reading. Choose them again to compare.",
                    window,
                    cx,
                );
                return;
            }
            let selection = compare_source.current(cx, |_| {
                let packages = keys
                    .iter()
                    .map(|key| PackageRef::parse(key))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                CompareSet::new(packages).ok()
            });
            match selection {
                Ok(selection) => compare_source.links.dispatch(
                    Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(
                        selection,
                    )))),
                    cx,
                ),
                Err(reason) => find_action_notice(reason, window, cx),
            }
        }),
    }
}

/// A Compare callback is a painted choice, not a lease on its original
/// reading. Route, owner, selected packages and the current member all have
/// to agree again when the user acts.
#[derive(Clone)]
struct CompareActionSource {
    links: super::super::region::Links,
    selection: CompareSet,
    root: VersionedRoot,
}

impl CompareActionSource {
    fn new(selection: &CompareSet, ctx: &Ctx<'_>, cx: &App) -> Self {
        Self {
            links: ctx.links.clone(),
            selection: selection.clone(),
            root: ctx.links.snapshot(cx).key(),
        }
    }

    fn current<R>(
        &self,
        cx: &App,
        member: impl FnOnce(&CompareModel) -> Option<R>,
    ) -> Result<R, &'static str> {
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        if snapshot.route()
            != &Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(
                self.selection.clone(),
            )))
            || snapshot.overlay().is_some()
            || !snapshot.key().same_authority(self.root)
        {
            return Err("Compare changed before that action. Choose from the current reading.");
        }
        let resource = store
            .pages()
            .browse(&BrowseKey::Compare(self.selection.clone()));
        let ResourceAdmission::Current(BrowseValue::Compare(compare)) =
            admit_resource(&resource, snapshot.key(), store.owner_serving())
        else {
            return Err(
                "Compare results changed before that action. Wait for the current reading.",
            );
        };
        if !compare
            .packages
            .iter()
            .map(|package| &package.package)
            .eq(self.selection.packages())
            || compare.apis.len() != self.selection.packages().len()
            || !compare
                .apis
                .iter()
                .zip(self.selection.packages())
                .all(|(api, package)| api.known().is_none_or(|api| &api.package == package))
            || !compare
                .prepared
                .candidates
                .iter()
                .map(|candidate| candidate.key.as_ref())
                .eq(self.selection.packages().iter().map(PackageRef::as_str))
        {
            return Err(
                "Compare results belong to another package selection. Wait for the current reading.",
            );
        }
        member(compare).ok_or("That choice is no longer in the current Compare reading.")
    }
}

fn compare_package_action(
    source: CompareActionSource,
) -> Rc<dyn Fn(SharedString, &mut Window, &mut App)> {
    Rc::new(move |key, window, cx| {
        let route = source.current(cx, |compare| {
            let package = PackageRef::parse(&key).ok()?;
            compare
                .packages
                .iter()
                .any(|candidate| candidate.package == package)
                .then(|| package_route(&package))
                .flatten()
        });
        match route {
            Ok(route) => source.links.dispatch(Intent::Navigate(route), cx),
            Err(reason) => find_action_notice(reason, window, cx),
        }
    })
}

fn compare_symbol_action(
    source: CompareActionSource,
    code: bool,
) -> Rc<dyn Fn(SharedString, &mut Window, &mut App)> {
    Rc::new(move |key, window, cx| {
        let route = source.current(cx, |compare| {
            let symbol = SymbolRef::new(&key).ok()?;
            let package = symbol.package()?;
            let present = compare.prepared.candidates.iter().any(|candidate| {
                candidate.key.as_ref() == package.as_str()
                    && candidate.operations.as_ref().is_some_and(|operations| {
                        operations.iter().any(|operation| {
                            operation.answer.key == key
                                && (!code || operation.answer.source_available)
                        })
                    })
            });
            if !present {
                return None;
            }
            if code {
                symbol_view_route(package.as_str(), &symbol, View::Code, None)
            } else {
                symbol_route(package.as_str(), &symbol)
            }
        });
        match route {
            Ok(route) => source.links.dispatch(Intent::Navigate(route), cx),
            Err(reason) => find_action_notice(reason, window, cx),
        }
    })
}

fn compare_actions(
    route: &BrowseRoute,
    ctx: &Ctx<'_>,
    cx: &mut Context<Reader>,
) -> facet::browse::compare::Actions {
    let BrowseRoute::Compare(selection) = route else {
        unreachable!("Compare actions require a Compare route")
    };
    let source = CompareActionSource::new(selection, ctx, cx);
    facet::browse::compare::Actions {
        active: ctx.native_input_active && source.current(cx, |_| Some(())).is_ok(),
        scroll: ctx.reader_scroll.clone(),
        open_package: compare_package_action(source.clone()),
        open_symbol: compare_symbol_action(source.clone(), false),
        open_code: compare_symbol_action(source, true),
        remove_package: compare_remove_action(
            CompareActionSource::new(selection, ctx, cx),
            cx.weak_entity(),
        ),
    }
}

fn compare_remove_action(
    source: CompareActionSource,
    reader: gpui::WeakEntity<Reader>,
) -> Rc<dyn Fn(SharedString, &mut Window, &mut App)> {
    Rc::new(move |key, window, cx| {
        let next = source.current(cx, |compare| {
            let package = PackageRef::parse(&key).ok()?;
            if !compare
                .packages
                .iter()
                .any(|candidate| candidate.package == package)
            {
                return None;
            }
            CompareSet::new(
                source
                    .selection
                    .packages()
                    .iter()
                    .filter(|candidate| *candidate != &package)
                    .cloned(),
            )
            .ok()
        });
        match next {
            Ok(selection) => {
                let _ = reader.update(cx, |reader, cx| {
                    reader.remove_find_held_package(&key, source.root, cx)
                });
                source.links.dispatch(
                    Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(
                        selection,
                    )))),
                    cx,
                );
            }
            Err(reason) => find_action_notice(reason, window, cx),
        }
    })
}

/// Record the small set of words actually in the initial Library viewport.
/// The full prepared model is immutable and was built by the read worker.
fn library_model(tree: &TreeModel, ctx: &mut Ctx<'_>) -> Arc<LibraryModel> {
    let model = &tree.prepared;
    ctx.say(model.name.clone());
    ctx.say(model.lede.clone());
    if let Some(note) = &model.note {
        ctx.say(note.clone());
    }
    for alert in model.alerts.iter().take(8) {
        ctx.say(alert.title.clone());
    }
    for fact in &model.facts {
        ctx.say(fact.clone());
    }
    for role in &model.roles {
        ctx.say(role.label.clone());
        if let Some(serving) = &role.serving {
            ctx.say(serving.clone());
        }
        for row in role.rows.iter().take(6) {
            ctx.say(row.name.clone());
            if let Some(rest) = &row.at_rest {
                ctx.say(rest.clone());
            }
        }
    }
    if let Some((heading, caption)) = &model.twice_heading {
        ctx.say(heading.clone());
        ctx.say(caption.clone());
    }
    Arc::clone(model)
}

#[cfg(test)]
#[path = "../browse_tests.rs"]
mod tests;
