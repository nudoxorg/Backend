//! The browsing pages' bodies: the product's words, drawn by
//! `facet::browse`. Your tree is the Library page.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf, Pages};
use crate::core::{ResourceAdmission, ResourceTerminal, admit_resource};
use crate::runtime::store::{DataStore, RouteDependencies};
use crate::model::browse::{
    BrowseKey, BrowseValue, TreeDestination, TreeInventoryLink, TreeModel, TreeRoleLinks,
};
use crate::model::pages::{PackageRef, PageKey, SearchQuery, SymbolRef};
use crate::navigation::{BrowseRoute, CompareSet, Intent, OrbitRoute, Route, View};
use crate::shell::kit::{package_route, symbol_route, symbol_view_route};
use crate::shell::reader::Reader;
use facet::browse::library::{InventoryHandle, ReleaseHandle};
use facet::browse::{LibraryActions, LibraryModel, library};
use gpui::{App, AppContext as _, Context, InteractiveElement, ParentElement, SharedString, Styled, div};
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
        let value = match shown(&resource) {
            Shown::Ready(BrowseValue::Find(value)) => Some(value.as_ref()),
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
                loading: matches!(shown(&resource), Shown::Pending),
            }),
        };
        // Find's reveal state lives in its native component. Its painted text
        // probes are the authority for what is visible in this frame.
        let actions = find_actions(route, ctx, cx);
        let mut leaves = vec![Leaf::new(facet::browse::find::find(
            "find",
            model,
            actions,
            &ctx.measure,
        ))];
        if matches!(shown(&resource), Shown::Fault(_) | Shown::Unavailable(_, _)) {
            leaves.extend(not_ready(
                &shown(&resource),
                &PageKey::Browse(key),
                "Find",
                ctx,
                cx,
            ));
        }
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

fn open_symbol_action(
    ctx: &Ctx<'_>,
    code: bool,
) -> Rc<dyn Fn(SharedString, &mut gpui::Window, &mut gpui::App)> {
    let links = ctx.links.clone();
    Rc::new(move |key, _, cx| {
        if let Ok(symbol) = SymbolRef::new(&key)
            && let Some(package) = symbol.package()
        {
            let route = if code {
                symbol_view_route(package.as_str(), &symbol, View::Code, None)
            } else {
                symbol_route(package.as_str(), &symbol)
            };
            if let Some(route) = route {
                links.dispatch(Intent::Navigate(route), cx);
            }
        }
    })
}

fn open_package_action(
    ctx: &Ctx<'_>,
) -> Rc<dyn Fn(SharedString, &mut gpui::Window, &mut gpui::App)> {
    let links = ctx.links.clone();
    Rc::new(move |key, _, cx| {
        if let Ok(package) = PackageRef::parse(&key)
            && let Some(route) = package_route(&package)
        {
            links.dispatch(Intent::Navigate(route), cx);
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
    route: &BrowseRoute,
    ctx: &Ctx<'_>,
    cx: &mut Context<Reader>,
) -> facet::browse::find::Actions {
    let expected = match route {
        BrowseRoute::Find(query) => Some(query.clone()),
        _ => None,
    };
    let refine_links = ctx.links.clone();
    let compare_links = ctx.links.clone();
    let reader = cx.weak_entity();
    let acquire = Some(crate::shell::acquire::add_actions(
        &ctx.links,
        cx.entity_id(),
    ));
    facet::browse::find::Actions {
        acquire,
        scroll: ctx.reader_scroll.clone(),
        initial_held: ctx.find_held.clone(),
        persist_held: Rc::new(move |held, cx| {
            let _ = reader.update(cx, |reader, cx| reader.set_find_held(held, cx));
        }),
        refine: Rc::new(move |text, cx| {
            let query = SearchQuery::new(&text, SearchQuery::DEFAULT_LIMIT).ok();
            refine_links.dispatch(
                Intent::RefineFind {
                    expected: expected.clone(),
                    query,
                },
                cx,
            );
        }),
        open_symbol: open_symbol_action(ctx, false),
        open_code: open_symbol_action(ctx, true),
        open_package: open_package_action(ctx),
        compare: Rc::new(move |keys, _, cx| {
            let packages = keys
                .iter()
                .map(|key| PackageRef::parse(key))
                .collect::<Result<Vec<_>, _>>();
            if let Ok(packages) = packages
                && let Ok(selection) = CompareSet::new(packages)
            {
                compare_links.dispatch(
                    Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(
                        selection,
                    )))),
                    cx,
                );
            }
        }),
    }
}

fn compare_actions(
    route: &BrowseRoute,
    ctx: &Ctx<'_>,
    cx: &mut Context<Reader>,
) -> facet::browse::compare::Actions {
    let packages = match route {
        BrowseRoute::Compare(selection) => selection.packages().to_vec(),
        _ => vec![],
    };
    let links = ctx.links.clone();
    let reader = cx.weak_entity();
    let held = ctx.find_held.clone();
    facet::browse::compare::Actions {
        active: ctx.active && ctx.links.snapshot(cx).overlay().is_none(),
        scroll: ctx.reader_scroll.clone(),
        open_package: open_package_action(ctx),
        open_symbol: open_symbol_action(ctx, false),
        open_code: open_symbol_action(ctx, true),
        remove_package: Rc::new(move |key, _, cx| {
            if let Ok(selection) = CompareSet::new(
                packages
                    .iter()
                    .filter(|package| package.as_str() != key.as_ref())
                    .cloned(),
            ) {
                let updated = held
                    .iter()
                    .filter(|package| package.key != key)
                    .cloned()
                    .collect();
                let _ = reader.update(cx, |reader, cx| reader.set_find_held(updated, cx));
                links.dispatch(
                    Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(
                        selection,
                    )))),
                    cx,
                );
            }
        }),
    }
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
