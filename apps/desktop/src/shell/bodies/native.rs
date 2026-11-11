//! The SQL, CSS and Impact pages, plain: a name, what the analysis says about
//! it, and rows that each open their source. Every list says what it cannot
//! see, next to the list, in words: nothing here lets an empty answer read as
//! "nothing there" unless the owner proved it.
//!
//! The words are prepared on the read worker (`model::native`); this module
//! only lays them out and gives each row the keyboard, the native
//! accessibility node and the guard every other Reader control has.

use super::state::{DisplayEvidence, display_evidence, earlier_notice, not_ready};
use super::{Ctx, Leaf, Pages};
use crate::core::LocalProjectId;
use crate::model::browse::{BrowseKey, BrowseValue};
use crate::model::native::{
    CssModel, CssState, DomainStateView, FreshnessView, ImpactModel, ImpactRowView, ListEnd, NativeFind, NodeView,
    SeedView, Site, SqlModel, SqlObject, SqlSite, Strength, UsersState, UsersView, Verified, Verdict,
};
use crate::model::pages::PageKey;
use crate::navigation::{BrowseRoute, ImpactRoute, Intent, NativeKind, NativeSubject, OrbitRoute, Route};
use crate::shell::focus::{Act, Target, TargetAction, native_control};
use crate::shell::kit::{Said, text};
use crate::shell::reader::Reader;
use facet::controls::button::button;
use facet::tokens::ty;
use facet::{Set as _, Space};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, div,
};
use std::rc::Rc;
use std::sync::Arc;

/// The key the page-level "what changes" control answers to.
pub(crate) const IMPACT_KEY: &str = "I";

pub(super) fn body(route: &BrowseRoute, store: &Pages, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let key = BrowseKey::from(route);
    let resource = store.browse(&key);
    let (root, serving) = {
        let live = ctx.links.store.read(cx);
        (live.snapshot().key(), live.owner_serving())
    };
    let evidence = display_evidence(&resource, root, serving, |value| answers_route(value, route));
    let (value, earlier) = match &evidence {
        DisplayEvidence::Current(value) => (*value, None),
        DisplayEvidence::Earlier { value, .. } => (*value, earlier_notice(&evidence)),
        DisplayEvidence::WrongIdentity => {
            return vec![caption(ctx, "This reading belongs to another question; its rows cannot be shown here.")];
        }
        DisplayEvidence::Missing(other) => {
            let (name, _) = route.here();
            return not_ready(other, &PageKey::Browse(key), &name, ctx, cx);
        }
    };
    let mut leaves = Vec::new();
    if let Some(earlier) = earlier {
        leaves.push(notice(ctx, Voice::Amber, "earlier-reading", earlier));
    }
    let refresh = Refresh::new(&key, ctx, cx);
    match value {
        BrowseValue::Impact(model) => impact_page(model, &refresh, &mut leaves, ctx, cx),
        BrowseValue::Sql(model) => sql_page(model, &refresh, &mut leaves, ctx, cx),
        BrowseValue::Css(model) => css_page(model, &refresh, &mut leaves, ctx, cx),
        _ => {
            leaves.push(caption(ctx, "This reply is not a page of this kind."));
        }
    }
    leaves
}

/// Whether a stored value answers exactly this route's question.
fn answers_route(value: &BrowseValue, route: &BrowseRoute) -> bool {
    match (value, route) {
        (BrowseValue::Impact(model), BrowseRoute::Impact(route)) => model.subject == route.subject && model.pages == route.pages,
        (BrowseValue::Sql(model), BrowseRoute::Sql(subject)) => model.subject == *subject,
        (BrowseValue::Css(model), BrowseRoute::Css(subject)) => model.subject == *subject,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Voice {
    Amber,
    Coral,
    Quiet,
}

/// One run of words, spoken to the native tree as a reading node of its own:
/// everything this page paints is also in the accessibility tree.
fn said(ctx: &mut Ctx<'_>, role: facet::tokens::TypeRole, ink: impl Into<gpui::Hsla>, words: impl Into<SharedString>) -> Said {
    let n = ctx.said.len();
    let words = ctx.say(words);
    text(role, &ctx.measure, ink)
        .role(gpui::Role::Label)
        .aria_label(words.clone())
        .keyed(SharedString::from(format!("native-text-{n}")))
        .child(words)
}

/// Said once, in a plate with a rule down its side, and a status node.
fn notice(ctx: &mut Ctx<'_>, voice: Voice, key: &str, words: impl Into<SharedString>) -> Leaf {
    let n = ctx.said.len();
    let words = ctx.say(words);
    let palette = ctx.palette;
    let rule = match voice {
        Voice::Amber => palette.amber.base,
        Voice::Coral => palette.coral.base,
        Voice::Quiet => palette.ink4,
    };
    let ink = match voice {
        Voice::Quiet => palette.ink2,
        Voice::Amber | Voice::Coral => palette.ink1,
    };
    Leaf::new(
        div()
            .border_l_2()
            .border_color(rule.hsla())
            .pl(ctx.measure.space(Space::Roomy))
            .child(
                text(ty::BODY, &ctx.measure, ink)
                    .role(gpui::Role::Status)
                    .aria_label(words.clone())
                    .keyed(SharedString::from(format!("native-notice-{n}-{key}")))
                    .child(words),
            ),
    )
}

fn heading(ctx: &mut Ctx<'_>, level: usize, words: impl Into<SharedString>) -> Leaf {
    let n = ctx.said.len();
    let words = ctx.say(words);
    let role = if level == 1 { ty::DISPLAY } else { ty::HEAD };
    Leaf::new(
        text(role, &ctx.measure, ctx.palette.ink0)
            .role(gpui::Role::Heading)
            .aria_level(level)
            .aria_label(words.clone())
            .keyed(SharedString::from(format!("native-heading-{n}")))
            .child(words),
    )
}

fn plain(ctx: &mut Ctx<'_>, words: impl Into<SharedString>) -> Leaf {
    let ink = ctx.palette.ink1;
    Leaf::new(said(ctx, ty::BODY, ink, words))
}

fn caption(ctx: &mut Ctx<'_>, words: impl Into<SharedString>) -> Leaf {
    let ink = ctx.palette.ink3;
    Leaf::new(said(ctx, ty::CAPTION, ink, words))
}

/// The page's name block: what it is and where, then its name.
fn header(subject: &NativeSubject, kind_line: &str, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let ink = ctx.palette.ink3;
    let small = Leaf::new(said(ctx, ty::SMALL, ink, format!("{kind_line} in {}", subject.project_name())));
    vec![small, heading(ctx, 1, subject.written())]
}

fn unique(prefix: &str, n: usize) -> SharedString {
    format!("native-{prefix}-{n}").into()
}

/// One page's read, so its controls can ask the owner again.
struct Refresh {
    key: PageKey,
}

impl Refresh {
    fn new(key: &BrowseKey, _ctx: &Ctx<'_>, _cx: &Context<Reader>) -> Self {
        Self { key: PageKey::Browse(key.clone()) }
    }
}

/// A control the keyboard walks to (J and K), hint mode labels (F) and a
/// screen reader names. The action carries the guard that admits it.
fn control(
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
    id: SharedString,
    label: &str,
    key_cap: Option<&str>,
    primary: bool,
    action: TargetAction,
) -> AnyElement {
    let act = action.callback();
    ctx.targets.push(Target { id: id.clone(), label: label.to_owned().into(), action: action.clone(), peek: None, source: None });
    let focus = ctx.native_handle(&id, cx);
    let mut face = button(SharedString::from(format!("{id}-button")), label.to_owned(), &ctx.measure)
        .ghost()
        .aria_label(label.to_owned())
        .on_click(move |window, cx| act(window, cx))
        .when_current(Rc::new(move |cx| action.admits(cx)));
    if primary {
        face = face.primary();
    }
    if let Some(cap) = key_cap {
        face = face.key(cap.to_owned());
    }
    if let Some(focus) = focus {
        face = face.focus_handle(focus);
    }
    ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(face)).into_any_element()
}

/// The action that asks the owner for this page again.
fn check_again(refresh: &Refresh, ctx: &Ctx<'_>, cx: &Context<Reader>) -> Option<TargetAction> {
    let store = ctx.links.store.clone();
    let expected = store.read(cx).capture_page_retry(&refresh.key)?;
    let links = ctx.links.clone();
    let local = ctx.native_local_guard(cx);
    let receipt = expected.clone();
    let act: Act = Rc::new(move |_, cx| links.retry_at(&receipt, cx));
    Some(
        TargetAction::new(
            Rc::new(move |cx| local(cx) && store.read(cx).admits_page_retry(&expected)),
            act,
        ),
    )
}

/// The action that opens a route (its page, or an Impact question), and
/// remembers which control left so Back lands on it again.
fn go_action(ctx: &Ctx<'_>, cx: &Context<Reader>, id: &SharedString, route: Route) -> TargetAction {
    let links = ctx.links.clone();
    let recall = ctx.targets.recall();
    let leave = id.clone();
    let act: Act = Rc::new(move |_, cx| {
        let leaving = links.snapshot(cx).route().clone();
        recall.focus(leave.clone());
        recall.remember_leave(leaving, leave.clone());
        links.dispatch(Intent::Navigate(route.clone()), cx);
    });
    ctx.target_snapshot_action(act, cx)
}

/// The action that opens a project file at a line in the editor.
fn open_action(ctx: &Ctx<'_>, cx: &Context<Reader>, project: &LocalProjectId, site: &Site) -> TargetAction {
    let links = ctx.links.clone();
    let absolute: Arc<str> = project.path().join(&*site.path).to_string_lossy().into_owned().into();
    let line = site.line.unwrap_or(1);
    let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::OpenSource { path: absolute.clone(), line }, cx));
    ctx.target_local_action(act, cx)
}

/// A row whose whole face opens `action`, named for a screen reader.
fn link_row(
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
    id: SharedString,
    label: String,
    description: String,
    action: TargetAction,
    content: impl IntoElement,
) -> AnyElement {
    ctx.targets.push(Target { id: id.clone(), label: label.clone().into(), action: action.clone(), peek: None, source: None });
    let focus = ctx.native_handle(&id, cx);
    let face = native_control(id.clone(), label, gpui::Role::Link, focus, action.callback())
        .aria_description(description)
        .cursor_pointer()
        .hover(|style| style.bg(ctx.palette.tint.hsla()))
        .w_full()
        .child(content);
    ctx.targets.track(id, face).into_any_element()
}

fn rows_block(ctx: &Ctx<'_>, rows: Vec<AnyElement>) -> Leaf {
    Leaf::new(div().flex().flex_col().gap(ctx.measure.space(Space::Base)).children(rows)).wide()
}

fn badge(ctx: &mut Ctx<'_>, strength: Strength) -> AnyElement {
    let palette = ctx.palette;
    let tone = match strength {
        Strength::Exact => palette.mint.base,
        Strength::Possible => palette.amber.base,
    };
    let word = said(ctx, ty::SMALL, tone, strength.word());
    div()
        .flex_none()
        .border_1()
        .border_color(tone.hsla())
        .px(ctx.measure.space(Space::Snug))
        .child(word)
        .into_any_element()
}

fn mono(ctx: &mut Ctx<'_>, words: impl Into<SharedString>) -> AnyElement {
    let ink = ctx.palette.ink2;
    said(ctx, ty::MONO_SMALL, ink, words).into_any_element()
}

fn small(ctx: &mut Ctx<'_>, words: impl Into<SharedString>) -> AnyElement {
    let ink = ctx.palette.ink2;
    said(ctx, ty::SMALL, ink, words).into_any_element()
}

fn verified_words(verified: &Verified) -> Option<&'static str> {
    match verified {
        Verified::Matches => Some("Source re-read and matches the index"),
        Verified::Changed => Some("Source changed since it was indexed"),
        Verified::Unread => None,
    }
}

/// What a project's own re-index says, and the controls to ask again.
fn freshness_leaves(
    freshness: &FreshnessView,
    project: &NativeSubject,
    refresh: &Refresh,
    leaves: &mut Vec<Leaf>,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) {
    match freshness {
        FreshnessView::Current { checked } => {
            let files = if *checked == 1 { "file" } else { "files" };
            leaves.push(caption(ctx, format!("Checked {checked} {files} against the index: they match.")));
            if let Some(action) = check_again(refresh, ctx, cx) {
                let id = unique("check", 0);
                leaves.push(Leaf::new(control(ctx, cx, id, "Check again", None, false, action)));
            }
        }
        FreshnessView::Stale { checked, changed, action } => {
            let files = if *checked == 1 { "file" } else { "files" };
            let list = changed.join(", ");
            leaves.push(notice(
                ctx,
                Voice::Amber,
                "stale",
                format!("This answer is out of date. Changed since indexed: {list}. Of {checked} {files} checked, those no longer match. The owner says: {action}."),
            ));
            leaves.push(reindex_and_check(project, refresh, ctx, cx));
        }
        FreshnessView::Unknown(why) => {
            leaves.push(notice(ctx, Voice::Amber, "freshness-unknown", format!("Whether the files still match the index could not be checked: {why}.")));
            if let Some(action) = check_again(refresh, ctx, cx) {
                let id = unique("check", 0);
                leaves.push(Leaf::new(control(ctx, cx, id, "Check again", None, false, action)));
            }
        }
    }
}

/// The two ways out of a stale answer: index the project again, and ask again.
fn reindex_and_check(subject: &NativeSubject, refresh: &Refresh, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let mut buttons = Vec::new();
    let snapshot = ctx.links.snapshot(cx);
    let in_shelf = snapshot.workspace().projects.iter().any(|project| project.id == subject.project);
    if in_shelf {
        let links = ctx.links.clone();
        let project = subject.project.clone();
        let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::RetryIndex(project.clone()), cx));
        let action = ctx.target_local_action(act, cx);
        let label = format!("Re-index {}", subject.project_name());
        buttons.push(control(ctx, cx, unique("reindex", 0), &label, None, true, action));
    }
    if let Some(action) = check_again(refresh, ctx, cx) {
        buttons.push(control(ctx, cx, unique("check", 0), "Check again", None, false, action));
    }
    Leaf::new(div().flex().flex_wrap().items_center().gap(ctx.measure.space(Space::Roomy)).children(buttons))
}

// ---------------------------------------------------------------------------
// Impact
// ---------------------------------------------------------------------------

fn domain_line(model: &ImpactModel) -> (Option<String>, Vec<String>) {
    let mut read = Vec::new();
    let mut caveats = Vec::new();
    for domain in &model.domains {
        let name = domain.domain.label();
        match &domain.state {
            DomainStateView::Analyzed => {
                let files = if domain.files == 1 { "file" } else { "files" };
                read.push(format!("{name} ({} {files})", domain.files));
            }
            DomainStateView::Partial(reasons) => caveats.push(format!("{name} was read in part: {}.", reasons.join(", "))),
            DomainStateView::Unavailable(why) => caveats.push(format!("{name} could not be read: {why}.")),
            DomainStateView::NotInstalled => caveats.push(format!("{name} analysis is not installed in this build, so nothing in {name} was searched.")),
        }
    }
    (if read.is_empty() { None } else { Some(format!("Read: {}.", read.join(", "))) }, caveats)
}

fn impact_row(
    model: &ImpactModel,
    row: &ImpactRowView,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let project = &model.subject.project;
    let badge = badge(ctx, row.strength);
    let (ink3, ink0) = (ctx.palette.ink3, ctx.palette.ink0);
    let kind = said(ctx, ty::SMALL, ink3, row.kind.to_string());
    let name = said(ctx, ty::BODY, ink0, row.name.to_string());
    let mut lines = vec![div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(ctx.measure.space(Space::Base))
        .child(badge)
        .child(kind)
        .child(name)
        .into_any_element()];
    if let Some(site) = &row.site {
        lines.push(mono(ctx, site.label()));
    }
    if let Some(within) = &row.within {
        lines.push(small(ctx, format!("in {within}")));
    }
    lines.push(small(ctx, row.why.to_string()));
    let through = match row.via.and_then(|via| model.rows.iter().find(|candidate| candidate.n == via)) {
        Some(via) => format!("Reached through {} {}.", via.kind, via.name),
        None if row.depth > 1 => format!("{} steps from {}.", row.depth, model.subject.written()),
        None => format!("Straight from {}.", model.subject.written()),
    };
    lines.push(small(ctx, through));
    let content = div().flex().flex_col().gap(ctx.measure.space(Space::Hair)).py(ctx.measure.space(Space::Snug)).children(lines);
    let id = unique("impact-row", row.n as usize);
    let follow = row.follow.clone().filter(|follow| *follow != model.subject);
    let mut parts: Vec<AnyElement> = Vec::new();
    match &row.site {
        Some(site) => {
            let action = open_action(ctx, cx, project, site);
            let description = format!("Opens {} in your editor", site.label());
            parts.push(link_row(ctx, cx, id.clone(), row.reading(), description, action, content));
        }
        None => parts.push(content.into_any_element()),
    }
    if let Some(follow) = follow {
        let target_id = unique("impact-follow", row.n as usize);
        let label = format!("What changes if I change {}", follow.written());
        let action = go_action(ctx, cx, &target_id, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(ImpactRoute::new(follow)))));
        parts.push(control(ctx, cx, target_id, &label, None, false, action));
    }
    div()
        .flex()
        .flex_wrap()
        .items_start()
        .gap(ctx.measure.space(Space::Roomy))
        .child(div().flex_1().min_w(gpui::px(160.0)).children(parts.drain(..1)))
        .children(parts)
        .into_any_element()
}

fn seed_rows(model: &ImpactModel, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (index, seed) in model.seeds.iter().enumerate() {
        rows.push(seed_row(model, seed, index, ctx, cx));
    }
    rows
}

fn seed_row(model: &ImpactModel, seed: &NodeView, index: usize, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let words = format!("{} {}", seed.kind, seed.name);
    let mut lines = vec![small(ctx, words.clone())];
    if let Some(site) = &seed.site {
        lines.push(mono(ctx, site.label()));
    }
    let content = div().flex().flex_col().gap(ctx.measure.space(Space::Hair)).py(ctx.measure.space(Space::Snug)).children(lines);
    match &seed.follow {
        Some(follow) if matches!(model.seed, SeedView::Ambiguous) => {
            let id = unique("seed", index);
            let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(ImpactRoute::new(follow.clone()))));
            let action = go_action(ctx, cx, &id, route);
            let label = match &seed.site {
                Some(site) => format!("Follow {words}, {}", site.label()),
                None => format!("Follow {words}"),
            };
            link_row(ctx, cx, id, label, "Asks what changes if you change this one".to_owned(), action, content)
        }
        _ => content.into_any_element(),
    }
}

pub(super) fn impact_page(
    model: &Arc<ImpactModel>,
    refresh: &Refresh,
    leaves: &mut Vec<Leaf>,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) {
    let subject = &model.subject;
    leaves.extend(header(subject, &format!("Impact of a change to this {}", subject.kind.label()), ctx));
    // The verdict, then the one reason an empty list is or is not an answer.
    let verdict = model.verdict();
    leaves.push(plain(ctx, verdict.sentence(subject)));
    if let Verdict::UnprovenNone(reasons) = &verdict {
        for reason in reasons {
            leaves.push(caption(ctx, format!("Not proved because {reason}.")));
        }
    }
    freshness_leaves(&model.freshness, subject, refresh, leaves, ctx, cx);
    if subject.has_page() {
        let id = unique("page", 0);
        let route = match subject.kind {
            NativeKind::CssClass | NativeKind::CssCustomProperty => BrowseRoute::Css(subject.clone()),
            _ => BrowseRoute::Sql(subject.clone()),
        };
        let label = format!("Open the {} page", subject.kind.label());
        let action = go_action(ctx, cx, &id, Route::Orbit(OrbitRoute::Browse(route)));
        leaves.push(Leaf::new(control(ctx, cx, id, &label, None, false, action)));
    }
    // What resolved, and what each language could read.
    if matches!(model.seed, SeedView::Ambiguous) {
        leaves.push(heading(ctx, 2, "Which one?"));
        let rows = seed_rows(model, ctx, cx);
        leaves.push(rows_block(ctx, rows));
    } else if matches!(model.seed, SeedView::Resolved) && !model.seeds.is_empty() {
        let names = model
            .seeds
            .iter()
            .map(|seed| match &seed.site {
                Some(site) => format!("{} {} ({})", seed.kind, seed.name, site.label()),
                None => format!("{} {}", seed.kind, seed.name),
            })
            .collect::<Vec<_>>()
            .join("; ");
        leaves.push(caption(ctx, format!("Followed: {names}.")));
    }
    let (read, caveats) = domain_line(model);
    if let Some(read) = read {
        leaves.push(caption(ctx, read));
    }
    for caveat in caveats {
        leaves.push(notice(ctx, Voice::Amber, "domain", caveat));
    }
    // Exact and possible rows are told apart by heading, mark and word.
    if matches!(model.seed, SeedView::Resolved) {
        let legend = "Exact: every step is proven in the source. Possible: at least one step is a name match the analysis could not prove.";
        if model.exact + model.possible > 0 {
            leaves.push(caption(ctx, legend));
        }
        for (strength, total) in [(Strength::Exact, model.exact), (Strength::Possible, model.possible)] {
            let rows: Vec<&ImpactRowView> = model.rows.iter().filter(|row| row.strength == strength).collect();
            if total == 0 && rows.is_empty() {
                continue;
            }
            let shown = rows.len();
            let title = if shown as u32 == total {
                format!("{} · {total}", strength.word())
            } else {
                format!("{} · showing {shown} of {total}", strength.word())
            };
            leaves.push(heading(ctx, 2, title));
            let built = rows.into_iter().map(|row| impact_row(model, row, ctx, cx)).collect();
            leaves.push(rows_block(ctx, built));
        }
        end_of_list(model, leaves, ctx, cx);
    }
    // What it cannot see, always listed.
    if !model.gaps.is_empty() {
        leaves.push(heading(ctx, 2, "What this cannot see"));
        for gap in &model.gaps {
            leaves.push(caption(ctx, gap.sentence()));
        }
    }
}

fn end_of_list(model: &ImpactModel, leaves: &mut Vec<Leaf>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) {
    let shown = model.rows.len();
    let total = (model.exact + model.possible) as usize;
    match &model.end {
        ListEnd::Complete => {
            if total > 0 {
                leaves.push(caption(ctx, format!("That is every affected thing within {} steps.", model.depth)));
            }
        }
        ListEnd::More => {
            leaves.push(caption(ctx, format!("Showing {shown} of {total}.")));
            let route = ImpactRoute { subject: model.subject.clone(), pages: model.pages }.more();
            if let Some(route) = route {
                let id = unique("more", 0);
                let action = go_action(ctx, cx, &id, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(route))));
                leaves.push(Leaf::new(control(ctx, cx, id, "Show more", None, true, action)));
            }
        }
        ListEnd::ViewFull => {
            leaves.push(notice(
                ctx,
                Voice::Amber,
                "view-full",
                format!("This view holds {shown} of {total}. The rest exist but are not shown; the list is not complete."),
            ));
        }
        ListEnd::BudgetLimited(budget) => {
            leaves.push(notice(
                ctx,
                Voice::Amber,
                "budget",
                format!("The search stopped at its {budget} budget, so this list is a lower bound: more may be affected."),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

fn site_row(
    id: SharedString,
    project: &LocalProjectId,
    site: &Site,
    lines: Vec<AnyElement>,
    reading: String,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let content = div().flex().flex_col().gap(ctx.measure.space(Space::Hair)).py(ctx.measure.space(Space::Snug)).children(lines);
    let action = open_action(ctx, cx, project, site);
    let description = format!("Opens {} in your editor", site.label());
    link_row(ctx, cx, id, reading, description, action, content)
}

fn sql_site_row(
    index: usize,
    prefix: &str,
    subject: &NativeSubject,
    entry: &SqlSite,
    extra: Option<String>,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let mut lines = vec![mono(ctx, entry.site.label())];
    let said = match &extra {
        Some(extra) => format!("{extra} {}", entry.action),
        None => entry.action.to_string(),
    };
    lines.push(small(ctx, said.clone()));
    if let Some(written) = &entry.written {
        lines.push(small(ctx, format!("Written as {written}")));
    }
    if let Some(words) = verified_words(&entry.verified) {
        lines.push(small(ctx, words));
    }
    site_row(unique(prefix, index), &subject.project, &entry.site, lines, format!("{}, {said}", entry.site.label()), ctx, cx)
}

fn users_leaves(
    title: &str,
    users: &UsersView,
    subject: &NativeSubject,
    leaves: &mut Vec<Leaf>,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) {
    leaves.push(heading(ctx, 2, title));
    match &users.state {
        UsersState::Unavailable(why) => {
            leaves.push(notice(ctx, Voice::Amber, &format!("users-{title}"), format!("This could not be read: {why}. That does not mean nothing uses it.")));
            return;
        }
        UsersState::Read => {}
    }
    if users.rows.is_empty() {
        let words = if users.incomplete || !users.gaps.is_empty() {
            "None were found, but that does not prove nothing uses it: see what the analysis cannot see below."
        } else {
            "None were found, and the analysis reports nothing it could not see."
        };
        leaves.push(caption(ctx, words));
    } else {
        leaves.push(caption(ctx, format!("{} exact, {} possible.", users.exact, users.possible)));
        let model = ImpactModel {
            subject: subject.clone(),
            seed: SeedView::Resolved,
            seeds: Vec::new(),
            exact: users.exact,
            possible: users.possible,
            files: 0,
            by_kind: Vec::new(),
            rows: users.rows.clone(),
            domains: Vec::new(),
            gaps: Vec::new(),
            freshness: FreshnessView::Unknown(Arc::from("not applicable")),
            end: ListEnd::Complete,
            pages: 1,
            depth: 0,
            proves_no_impact: false,
            next: Vec::new(),
        };
        let rows = users.rows.iter().map(|row| impact_row(&model, row, ctx, cx)).collect();
        leaves.push(rows_block(ctx, rows));
    }
    if users.incomplete && !users.rows.is_empty() {
        leaves.push(caption(ctx, "There are more users than these."));
    }
    for gap in &users.gaps {
        leaves.push(caption(ctx, gap.sentence()));
    }
    if let Some(freshness) = &users.freshness {
        if let FreshnessView::Stale { changed, .. } = freshness {
            leaves.push(notice(ctx, Voice::Amber, "users-stale", format!("These users were read from files that changed since they were indexed: {}.", changed.join(", "))));
        }
    }
}

fn impact_door(subject: &NativeSubject, leaves: &mut Vec<Leaf>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) {
    let id = unique("impact", 0);
    let label = format!("What changes if I change {}", subject.written());
    let action = go_action(ctx, cx, &id, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(ImpactRoute::new(subject.clone())))));
    leaves.push(Leaf::new(control(ctx, cx, id, &label, Some(IMPACT_KEY), true, action)));
}

fn changed_notice(changed: &[Arc<str>], subject: &NativeSubject, refresh: &Refresh, leaves: &mut Vec<Leaf>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) {
    if changed.is_empty() {
        return;
    }
    leaves.push(notice(
        ctx,
        Voice::Amber,
        "changed",
        format!(
            "This page is out of date. Changed since indexed: {}. What is shown here comes from the index, not from the files as they are now. Re-index the project, then check again.",
            changed.join(", ")
        ),
    ));
    leaves.push(reindex_and_check(subject, refresh, ctx, cx));
}

pub(super) fn sql_page(model: &Arc<SqlModel>, refresh: &Refresh, leaves: &mut Vec<Leaf>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) {
    let subject = &model.subject;
    let table_subject = subject.table_of_column().unwrap_or_else(|| subject.clone());
    let kind_line = match &model.object {
        SqlObject::Defined { kind, dialect, .. } => format!("{} ({dialect})", kind.label()),
        SqlObject::NotDefined | SqlObject::Unavailable(_) => "SQL table or view".to_owned(),
    };
    leaves.extend(header(&table_subject, &kind_line, ctx));
    if let Some(column) = &model.column {
        leaves.push(plain(ctx, format!("You came for the column {column}. Its statements are marked below.")));
    }
    match &model.object {
        SqlObject::Unavailable(why) => {
            leaves.push(notice(ctx, Voice::Amber, "sql-unavailable", format!("The SQL analysis could not be read: {why} Nothing below is evidence about {}.", model.object_name)));
        }
        SqlObject::NotDefined => {
            leaves.push(notice(
                ctx,
                Voice::Amber,
                "sql-not-defined",
                format!(
                    "The SQL analysis read this project and found no table or view named {}. SQL held in code strings, in files that were not analysed, or in a database that is not in the project is not seen. This is not proof that it does not exist.",
                    model.object_name
                ),
            ));
        }
        SqlObject::Defined { live, .. } => {
            let statements = model.definitions.len();
            let head = if *live { "It exists after the last statement in the declared order." } else { "It does not exist after the last statement in the declared order: it was dropped." };
            let word = if statements == 1 { "statement defines or changes it" } else { "statements define or change it" };
            leaves.push(plain(ctx, format!("{statements} {word}. {head}")));
        }
    }
    changed_notice(&model.changed, subject, refresh, leaves, ctx, cx);
    if matches!(model.object, SqlObject::Defined { .. }) {
        impact_door(&table_subject, leaves, ctx, cx);
    }
    // Definition source, then what changed it.
    let created: Vec<(usize, &SqlSite)> = model.definitions.iter().enumerate().filter(|(_, entry)| &*entry.action == "created").collect();
    let changes: Vec<(usize, &SqlSite)> = model.definitions.iter().enumerate().filter(|(_, entry)| &*entry.action != "created").collect();
    if !created.is_empty() {
        leaves.push(heading(ctx, 2, "Where it is defined"));
        let rows = created.iter().map(|(index, entry)| sql_site_row(*index, "definition", &table_subject, entry, None, ctx, cx)).collect();
        leaves.push(rows_block(ctx, rows));
    }
    let mut history: Vec<(Option<String>, &SqlSite)> = changes.iter().map(|(_, entry)| (None, *entry)).collect();
    for column in &model.columns {
        for entry in column.sites.iter().filter(|entry| &*entry.action != "created") {
            history.push((Some(format!("Column {}:", column.name)), entry));
        }
    }
    history.sort_by(|a, b| a.1.site.path.cmp(&b.1.site.path).then(a.1.site.line.cmp(&b.1.site.line)));
    if !history.is_empty() {
        leaves.push(heading(ctx, 2, "Migrations that touched it"));
        let rows = history
            .iter()
            .enumerate()
            .map(|(index, (extra, entry))| sql_site_row(index, "migration", &table_subject, entry, extra.clone(), ctx, cx))
            .collect();
        leaves.push(rows_block(ctx, rows));
    } else if matches!(model.object, SqlObject::Defined { .. }) {
        leaves.push(caption(ctx, "No later statement alters, renames or drops it."));
    }
    if !model.columns.is_empty() {
        leaves.push(heading(ctx, 2, format!("Columns · {}", model.columns.len())));
        let mut rows = Vec::new();
        for (index, column) in model.columns.iter().enumerate() {
            let came = model.column.as_deref() == Some(&*column.name);
            let ink = if came { ctx.palette.ink0 } else { ctx.palette.ink1 };
            let mut lines = vec![said(ctx, ty::BODY, ink, column.name.to_string()).into_any_element()];
            if !column.live {
                lines.push(small(ctx, "Dropped: it does not exist after the last statement in the declared order."));
            }
            if came {
                lines.push(small(ctx, "The column you came for."));
            }
            let first = column.sites.first().map(|site| site.site.clone());
            let reading = format!("Column {}", column.name);
            let row = match first {
                Some(site) => {
                    lines.push(mono(ctx, site.label()));
                    site_row(unique("column", index), &table_subject.project, &site, lines, reading, ctx, cx)
                }
                None => div().flex().flex_col().children(lines).into_any_element(),
            };
            let mut parts = vec![div().flex_1().min_w(gpui::px(160.0)).child(row).into_any_element()];
            if let Ok(column_subject) = NativeSubject::new(table_subject.project.clone(), NativeKind::SqlColumn, &format!("{}.{}", model.object_name, column.name), None) {
                let id = unique("column-impact", index);
                let label = format!("What changes if I change {}", column_subject.written());
                let action = go_action(ctx, cx, &id, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(ImpactRoute::new(column_subject)))));
                parts.push(control(ctx, cx, id, &label, None, false, action));
            }
            rows.push(div().flex().flex_wrap().items_start().gap(ctx.measure.space(Space::Roomy)).children(parts).into_any_element());
        }
        leaves.push(rows_block(ctx, rows));
    }
    // Where it is used.
    if matches!(model.object, SqlObject::Defined { .. }) {
        let (sql, code): (Vec<_>, Vec<_>) = model.uses.iter().enumerate().partition(|(_, usage)| !usage.in_code);
        for (title, group, prefix) in [("Used in SQL", sql, "use-sql"), ("Used in code", code, "use-code")] {
            leaves.push(heading(ctx, 2, format!("{title} · {}", group.len())));
            if group.is_empty() {
                let words = if model.coverage.complete {
                    "No written mention was found, and the analysis reports nothing it could not read.".to_owned()
                } else {
                    "No written mention was found, but the analysis did not read everything: see below.".to_owned()
                };
                leaves.push(caption(ctx, words));
                continue;
            }
            let mut rows = Vec::new();
            for (index, usage) in group {
                let mut lines = vec![mono(ctx, usage.site.label())];
                let column = usage.column.as_ref().map(|column| format!(" {column}")).unwrap_or_default();
                let mut said = format!("{} {}{column}", usage.statement, usage.role);
                if let Some(within) = &usage.within {
                    said = format!("{said}, inside {within}");
                }
                lines.push(small(ctx, said.clone()));
                if !usage.live {
                    lines.push(small(ctx, "Bound to a definition that was later dropped."));
                }
                if let Some(words) = verified_words(&usage.verified) {
                    lines.push(small(ctx, words));
                }
                rows.push(site_row(unique(prefix, index), &subject.project, &usage.site, lines, format!("{}, {said}", usage.site.label()), ctx, cx));
            }
            leaves.push(rows_block(ctx, rows));
        }
        if model.uses_incomplete {
            leaves.push(notice(ctx, Voice::Amber, "uses-incomplete", "There are more mentions than these; the list ends before the last."));
        }
        users_leaves("Other things that depend on it", &model.users, &table_subject, leaves, ctx, cx);
    }
    // Said once, at the end, for everything above.
    if !model.coverage.notes.is_empty() || !model.gap_sites.is_empty() {
        leaves.push(heading(ctx, 2, "What this cannot see"));
        for note in &model.coverage.notes {
            leaves.push(caption(ctx, note.to_string()));
        }
        for gap in &model.gap_sites {
            leaves.push(caption(ctx, format!("{}: {}.", gap.path, gap.why)));
        }
    }
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

pub(super) fn css_page(model: &Arc<CssModel>, refresh: &Refresh, leaves: &mut Vec<Leaf>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) {
    let subject = &model.subject;
    leaves.extend(header(subject, subject.kind.label(), ctx));
    match &model.state {
        CssState::Unavailable(why) => {
            leaves.push(notice(ctx, Voice::Amber, "css-unavailable", format!("The CSS analysis could not be read: {why} Nothing below is evidence about {}.", subject.written())));
        }
        CssState::NotFound => {
            leaves.push(notice(
                ctx,
                Voice::Amber,
                "css-not-found",
                format!(
                    "The CSS analysis read this project's stylesheets and found nothing named {}. Names built at run time, framework-generated names and files that were not analysed are not seen. This is not proof that it is unused.",
                    subject.written()
                ),
            ));
        }
        CssState::Found => {
            let n = model.rules.len() + model.declarations.len() + model.reads.len();
            let word = if n == 1 { "place in the stylesheets names" } else { "places in the stylesheets name" };
            leaves.push(plain(ctx, format!("{n} {word} {}.", subject.written())));
        }
    }
    changed_notice(&model.changed, subject, refresh, leaves, ctx, cx);
    if !matches!(model.state, CssState::Unavailable(_)) {
        impact_door(subject, leaves, ctx, cx);
    }
    if !model.rules.is_empty() {
        leaves.push(heading(ctx, 2, format!("Rules · {}", model.rules.len())));
        let mut rows = Vec::new();
        for (index, rule) in model.rules.iter().enumerate() {
            let mut lines = vec![mono(ctx, if rule.approximate { format!("{} (line is approximate)", rule.site.label()) } else { rule.site.label() })];
            let ink = ctx.palette.ink0;
            lines.push(said(ctx, ty::CODE, ink, rule.selector.to_string()).into_any_element());
            if let Some(condition) = &rule.condition {
                lines.push(small(ctx, format!("Inside {condition}")));
            }
            if let Some(written) = &rule.written {
                lines.push(small(ctx, format!("Written as {written}")));
            }
            if let Some(words) = verified_words(&rule.verified) {
                lines.push(small(ctx, words));
            }
            rows.push(site_row(unique("rule", index), &subject.project, &rule.site, lines, format!("{}, {}", rule.site.label(), rule.selector), ctx, cx));
        }
        leaves.push(rows_block(ctx, rows));
    }
    if !model.declarations.is_empty() {
        leaves.push(heading(ctx, 2, format!("Declared · {}", model.declarations.len())));
        let mut rows = Vec::new();
        for (index, declaration) in model.declarations.iter().enumerate() {
            let mut lines = vec![mono(ctx, declaration.site.label())];
            let value = declaration.value.as_ref().map(|value| format!("{}: {value}{}", subject.name, if declaration.important { " !important" } else { "" }));
            if let Some(value) = &value {
                let ink = ctx.palette.ink0;
                lines.push(said(ctx, ty::CODE, ink, value.clone()).into_any_element());
            }
            if let Some(words) = verified_words(&declaration.verified) {
                lines.push(small(ctx, words));
            }
            let reading = format!("{}, declared{}", declaration.site.label(), value.map(|value| format!(" {value}")).unwrap_or_default());
            rows.push(site_row(unique("declaration", index), &subject.project, &declaration.site, lines, reading, ctx, cx));
        }
        leaves.push(rows_block(ctx, rows));
    }
    if !model.reads.is_empty() {
        leaves.push(heading(ctx, 2, format!("Read by · {}", model.reads.len())));
        let mut rows = Vec::new();
        for (index, read) in model.reads.iter().enumerate() {
            let lines = vec![mono(ctx, read.site.label()), small(ctx, format!("{} reads it with {}()", read.property, read.how))];
            rows.push(site_row(unique("read", index), &subject.project, &read.site, lines, format!("{}, read by {}", read.site.label(), read.property), ctx, cx));
        }
        leaves.push(rows_block(ctx, rows));
    }
    if model.incomplete {
        leaves.push(notice(ctx, Voice::Amber, "css-incomplete", "There are more places than these; the list ends before the last."));
    }
    if !matches!(model.state, CssState::Unavailable(_)) {
        users_leaves("Components that use it", &model.users, subject, leaves, ctx, cx);
    }
    if !model.coverage.notes.is_empty() {
        leaves.push(heading(ctx, 2, "What this cannot see"));
        for note in &model.coverage.notes {
            leaves.push(caption(ctx, note.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// Find: what the projects' SQL and CSS hold under the query's name
// ---------------------------------------------------------------------------

/// The section Find adds below its own answers. `None`: the query was too
/// short to look for, so there is nothing honest to say.
pub(super) fn find_section(
    query: &str,
    find: &NativeFind,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    if !find.asked {
        return Vec::new();
    }
    let mut leaves = vec![heading(ctx, 2, "In your projects' SQL and CSS")];
    if find.searched == 0 {
        leaves.push(caption(ctx, "No project is indexed, so no SQL or CSS was searched."));
    }
    let mut rows = Vec::new();
    for (index, hit) in find.hits.iter().enumerate() {
        let page = if hit.subject.has_page() {
            match hit.subject.kind {
                NativeKind::CssClass | NativeKind::CssCustomProperty => BrowseRoute::Css(hit.subject.clone()),
                _ => BrowseRoute::Sql(hit.subject.clone()),
            }
        } else {
            BrowseRoute::Impact(ImpactRoute::new(hit.subject.clone()))
        };
        let id = unique("find-hit", index);
        let ink = ctx.palette.ink0;
        let mut lines = vec![said(ctx, ty::BODY, ink, hit.subject.written()).into_any_element(), small(ctx, hit.detail.to_string())];
        if let Some(site) = &hit.site {
            lines.push(mono(ctx, site.label()));
        }
        let content = div().flex().flex_col().gap(ctx.measure.space(Space::Hair)).py(ctx.measure.space(Space::Snug)).children(lines);
        let action = go_action(ctx, cx, &id, Route::Orbit(OrbitRoute::Browse(page)));
        let label = format!("{}, {}", hit.subject.written(), hit.detail);
        let row = link_row(ctx, cx, id, label, "Opens its page".to_owned(), action, content);
        let impact_id = unique("find-impact", index);
        let impact_label = format!("What changes if I change {}", hit.subject.written());
        let impact_action = go_action(ctx, cx, &impact_id, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(ImpactRoute::new(hit.subject.clone())))));
        let impact = control(ctx, cx, impact_id, &impact_label, None, false, impact_action);
        rows.push(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(ctx.measure.space(Space::Roomy))
                .child(div().flex_1().min_w(gpui::px(160.0)).child(row))
                .child(impact)
                .into_any_element(),
        );
    }
    if !rows.is_empty() {
        leaves.push(rows_block(ctx, rows));
    } else if find.searched > 0 {
        let projects = if find.searched == 1 { "project".to_owned() } else { "projects".to_owned() };
        let complete = find.notes.is_empty();
        leaves.push(caption(
            ctx,
            if complete {
                format!("No SQL table, view, column, CSS class or custom property is named {query} in the {} {projects} searched. Names built at run time are not searched.", find.searched)
            } else {
                format!("Nothing named {query} was found in the {} {projects} searched, but not everything could be searched: see below.", find.searched)
            },
        ));
    }
    for note in &find.notes {
        leaves.push(caption(ctx, note.to_string()));
    }
    leaves
}

/// Find's SQL and CSS leaves for the stored resource, whatever state it is in.
pub(super) fn find_leaves(query: &crate::model::pages::SearchQuery, store: &Pages, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let key = BrowseKey::FindNative(query.clone());
    let resource = store.browse(&key);
    let (root, serving) = {
        let live = ctx.links.store.read(cx);
        (live.snapshot().key(), live.owner_serving())
    };
    let evidence = display_evidence(&resource, root, serving, |value| matches!(value, BrowseValue::FindNative(_)));
    match &evidence {
        DisplayEvidence::Current(BrowseValue::FindNative(find)) => find_section(&query.text, find, ctx, cx),
        DisplayEvidence::Earlier { value: BrowseValue::FindNative(find), .. } => {
            let mut leaves = find_section(&query.text, find, ctx, cx);
            if !leaves.is_empty() {
                leaves.push(caption(ctx, "These are from an earlier reading; they are read-only until the owner checks them again."));
            }
            leaves
        }
        DisplayEvidence::Current(_) | DisplayEvidence::Earlier { .. } | DisplayEvidence::WrongIdentity => Vec::new(),
        DisplayEvidence::Missing(super::state::Shown::Pending) | DisplayEvidence::Missing(super::state::Shown::Ready(_)) => {
            if query.text.trim().chars().count() < 2 {
                return Vec::new();
            }
            vec![caption(ctx, "Looking in your projects' SQL and CSS.")]
        }
        DisplayEvidence::Missing(super::state::Shown::Fault(error)) => {
            vec![caption(ctx, format!("The projects' SQL and CSS could not be searched: {}", error.message()))]
        }
        DisplayEvidence::Missing(super::state::Shown::Unavailable(..)) => {
            vec![caption(ctx, "This owner does not search the projects' SQL and CSS.")]
        }
    }
}
