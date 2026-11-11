//! The SQL, CSS and Impact pages through the real window root: what they say,
//! how the keyboard reaches them, what the native accessibility tree holds,
//! and what they do when the answer is empty, partial, stale or missing.
//!
//! A canned reader stands in for the owner (the lowering of real owner
//! replies is tested in `runtime::native_reads`), so every state below is
//! one the page has to draw: no test depends on a worker being installed.

#![allow(clippy::expect_used, clippy::panic)]

use super::fit_tests::{PHONES, SIZES, findings, painted, resize};
use super::tests::{Fixture, Rig, rig_with_reads};
use crate::core::LocalProjectId;
use crate::model::browse::{BrowseKey, BrowseValue};
use crate::model::native::{
    Coverage, CssDeclaration, CssModel, CssRead, CssRule, CssState, DomainStateView, DomainView, FreshnessView, GapView,
    ImpactModel, ImpactRowView, ListEnd, NativeFind, NativeHit, NodeView, SeedView, Site, SqlColumn, SqlGapSite,
    SqlModel, SqlObject, SqlSite, SqlUse, Strength, UsersState, UsersView, Verified,
};
use crate::model::pages::{PageValue, ReadFailure};
use crate::navigation::{
    BrowseRoute, ImpactRoute, Intent, NativeDomain, NativeKind, NativeSubject, OrbitRoute, Route,
};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use gpui::TestAppContext;
use std::sync::{Arc, Mutex};

type Answer = Arc<dyn Fn(&BrowseKey) -> Option<Result<BrowseValue, ReadFailure>> + Send + Sync>;

/// Answers the native pages from a function the test can change between reads.
#[derive(Clone)]
struct Canned {
    answer: Arc<Mutex<Answer>>,
    reads: Arc<Mutex<Vec<BrowseKey>>>,
}

impl Canned {
    fn new(answer: impl Fn(&BrowseKey) -> Option<Result<BrowseValue, ReadFailure>> + Send + Sync + 'static) -> Self {
        Self { answer: Arc::new(Mutex::new(Arc::new(answer))), reads: Arc::default() }
    }

    fn answer_with(&self, answer: impl Fn(&BrowseKey) -> Option<Result<BrowseValue, ReadFailure>> + Send + Sync + 'static) {
        *self.answer.lock().expect("answer") = Arc::new(answer);
    }

    fn reads_of(&self, wanted: impl Fn(&BrowseKey) -> bool) -> usize {
        self.reads.lock().expect("reads").iter().filter(|key| wanted(key)).count()
    }
}

struct Reader(Canned);

impl PageReader for Reader {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Browse(key @ (BrowseKey::Impact(_) | BrowseKey::Sql(_) | BrowseKey::Css(_) | BrowseKey::FindNative(_))) = request {
            self.0.reads.lock().expect("reads").push(key.clone());
            let answer = Arc::clone(&self.0.answer.lock().expect("answer"));
            if let Some(answer) = answer(key) {
                return answer.map(PageValue::Browse);
            }
        }
        Fixture.read(request, context)
    }
}

fn open(cx: &mut TestAppContext, canned: &Canned, width: f32, height: f32) -> Rig {
    let canned = canned.clone();
    let pool = ReadPool::start(2, move |_| Reader(canned.clone())).expect("pool");
    rig_with_reads(cx, None, width, height, pool)
}

fn project() -> LocalProjectId {
    LocalProjectId::new("/workspace/shop").expect("project")
}

fn subject(kind: NativeKind, name: &str) -> NativeSubject {
    NativeSubject::new(project(), kind, name, None).expect("subject")
}

fn route(browse: BrowseRoute) -> Route {
    Route::Orbit(OrbitRoute::Browse(browse))
}

fn impact_route(kind: NativeKind, name: &str) -> Route {
    route(BrowseRoute::Impact(ImpactRoute::new(subject(kind, name))))
}

fn site(path: &str, line: u32) -> Option<Site> {
    Some(Site { path: path.into(), line: Some(line) })
}

fn component(n: u32, via: Option<u32>, strength: Strength, name: &str, path: &str, line: u32, why: &str) -> ImpactRowView {
    ImpactRowView {
        n,
        depth: 1,
        strength,
        domain: NativeDomain::Code,
        kind: "component".into(),
        name: name.into(),
        site: site(path, line),
        within: None,
        relation: "code.uses-class".into(),
        why: why.into(),
        via,
        follow: Some(subject(NativeKind::CodeComponent, name)),
    }
}

fn domains() -> Vec<DomainView> {
    vec![
        DomainView { domain: NativeDomain::Css, state: DomainStateView::Analyzed, files: 2 },
        DomainView { domain: NativeDomain::Code, state: DomainStateView::Analyzed, files: 4 },
    ]
}

/// A class with one exact and one possible user, a gap the owner declared, and fresh files.
fn card_impact(pages: u8) -> ImpactModel {
    ImpactModel {
        subject: subject(NativeKind::CssClass, "card"),
        seed: SeedView::Resolved,
        seeds: vec![NodeView {
            domain: NativeDomain::Css,
            kind: "class".into(),
            name: "card".into(),
            site: site("src/card.css", 3),
            follow: None,
        }],
        exact: 1,
        possible: 1,
        files: 2,
        by_kind: vec![("component".into(), 2)],
        rows: vec![
            component(1, None, Strength::Exact, "Card", "src/Card.tsx", 7, "imports Card.module.css"),
            component(2, Some(1), Strength::Possible, "Page", "src/Page.tsx", 2, "renders Card"),
        ],
        domains: domains(),
        gaps: vec![GapView { facet: "code.dynamic-class".into(), note: "class names built at run time".into(), count: Some(3) }],
        freshness: FreshnessView::Current { checked: 6 },
        end: ListEnd::Complete,
        pages,
        depth: 3,
        proves_no_impact: false,
        next: Vec::new(),
    }
}

fn serve_impact(model: ImpactModel) -> Canned {
    Canned::new(move |key| match key {
        BrowseKey::Impact(route) if route.subject == model.subject => {
            Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { pages: route.pages, ..model.clone() }))))
        }
        _ => None,
    })
}

// ---- reading the native tree and the keyboard ----------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Node {
    role: gpui::Role,
    label: String,
    level: Option<usize>,
}

fn native(rig: &mut Rig) -> Vec<Node> {
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.repaint();
    rig.cx.update(|window, _| {
        window
            .a11y_tree()
            .expect("forced native tree")
            .nodes
            .iter()
            .filter_map(|(_, node)| {
                Some(Node { role: node.role(), label: node.label().or_else(|| node.value())?.to_owned(), level: node.level() })
            })
            .collect()
    })
}

fn has(nodes: &[Node], role: gpui::Role, label: &str) -> bool {
    nodes.iter().any(|node| node.role == role && node.label == label)
}

fn said(rig: &mut Rig) -> Vec<String> {
    rig.said()
}

fn says(rig: &mut Rig, words: &str) -> bool {
    said(rig).iter().any(|line| line == words)
}

fn targets(rig: &mut Rig) -> Vec<String> {
    rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed().into_iter().map(|(target, _)| target.label.to_string()).collect()
}

fn focused(rig: &mut Rig) -> Option<String> {
    rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).1).map(|id| id.to_string())
}

/// Walks the keyboard (J) until the target named `label` has it.
fn walk_to(rig: &mut Rig, label: &str) {
    for _ in 0..60 {
        rig.keys("j");
        let at = rig.shell.read_with(rig.cx, |shell, cx| {
            let targets = shell.reader_targets(cx);
            targets.current().map(|target| target.label.to_string())
        });
        if at.as_deref() == Some(label) {
            return;
        }
    }
    panic!("J never reached {label:?}; the walk has {:?}", targets(rig));
}

/// On Find the query field owns the letters, so a person clicks into it and
/// Tab walks the native controls after it (Find's own, then the ones below its
/// answers) until the one named `label` has the keyboard.
fn tab_to(rig: &mut Rig, label: &str) {
    let at = super::tests::native_bounds(rig, "TextInput", "Find query", false).expect("the query field").center();
    rig.cx.simulate_click(at, gpui::Modifiers::none());
    rig.settle();
    for _ in 0..80 {
        let reached = rig.shell.read_with(rig.cx, |shell, cx| {
            let targets = shell.reader_targets(cx);
            let id = targets.placed().into_iter().find(|(target, _)| target.label == label).map(|(target, _)| target.id)?;
            Some(id)
        });
        let on_it = reached.is_some_and(|id| {
            let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
            rig.cx.update(|window, cx| window.focused(cx).is_some_and(|focus| focus == targets.native_handle(&id, cx)))
        });
        if on_it {
            return;
        }
        rig.keys("tab");
    }
    panic!("Tab never reached {label:?}; the controls are {:?}", targets(rig));
}

mod launch {
    use crate::host::editor::{Command, Launch};
    use std::cell::RefCell;
    use std::rc::Rc;

    pub(super) struct Recorder(pub Rc<RefCell<Vec<Command>>>);

    impl Launch for Recorder {
        fn run(&self, command: &Command) -> std::io::Result<()> {
            self.0.borrow_mut().push(command.clone());
            Ok(())
        }
    }
}

/// An editor that only writes down what it was asked to open.
fn recorder(rig: &mut Rig) -> std::rc::Rc<std::cell::RefCell<Vec<crate::host::editor::Command>>> {
    let ran = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let launch = launch::Recorder(std::rc::Rc::clone(&ran));
    rig.cx.update(|_, cx| crate::host::editor::install(std::rc::Rc::new(launch), cx));
    ran
}

// ---- Impact ---------------------------------------------------------------------------

#[gpui::test]
fn the_impact_page_tells_exact_from_possible_and_says_what_it_cannot_see(cx: &mut TestAppContext) {
    let canned = serve_impact(card_impact(1));
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    for expected in [
        "Impact of a change to this CSS class in shop",
        ".card",
        "Changing .card affects 1 exact and 1 possible, in 2 files.",
        "Exact · 1",
        "Possible · 1",
        "Card",
        "Page",
        "src/Card.tsx:7",
        "src/Page.tsx:2",
        "imports Card.module.css",
        "Straight from .card.",
        "Reached through component Card.",
        "Read: CSS (2 files), Code (4 files).",
        "Checked 6 files against the index: they match.",
        "What this cannot see",
        "code.dynamic-class (3): class names built at run time",
    ] {
        assert!(says(&mut rig, expected) || said(&mut rig).iter().any(|line| line.contains(expected)), "{expected:?} is not on screen: {:#?}", said(&mut rig));
    }
    // Strength is a word and a heading, never a colour alone.
    let said = said(&mut rig);
    assert_eq!(said.iter().filter(|line| *line == "Exact").count(), 1);
    assert_eq!(said.iter().filter(|line| *line == "Possible").count(), 1);
    let exact = said.iter().position(|line| line == "Exact · 1").expect("exact heading");
    let card = said.iter().position(|line| line == "Card").expect("card row");
    let possible = said.iter().position(|line| line == "Possible · 1").expect("possible heading");
    let page = said.iter().position(|line| line == "Page").expect("page row");
    assert!(exact < card && card < possible && possible < page, "exact rows come before the possible ones: {said:#?}");
    // A complete list that found rows does not claim more than it found.
    assert!(said.iter().any(|line| line.starts_with("That is every affected thing within 3 steps")));
    assert!(!said.iter().any(|line| line.contains("Nothing depends")));
}

#[gpui::test]
fn every_word_the_impact_page_paints_reaches_the_native_tree_with_roles(cx: &mut TestAppContext) {
    let canned = serve_impact(card_impact(1));
    let mut rig = open(cx, &canned, 1280.0, 1000.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let nodes = native(&mut rig);
    let heading = nodes.iter().find(|node| node.role == gpui::Role::Heading && node.label == ".card").expect("the page's heading");
    assert_eq!(heading.level, Some(1), "the name is the page's first-level heading");
    for section in ["Exact · 1", "Possible · 1", "What this cannot see"] {
        let node = nodes.iter().find(|node| node.role == gpui::Role::Heading && node.label == section).unwrap_or_else(|| panic!("{section} heading"));
        assert_eq!(node.level, Some(2));
    }
    assert!(has(&nodes, gpui::Role::Link, "Exact: component Card, src/Card.tsx:7"), "{nodes:#?}");
    assert!(has(&nodes, gpui::Role::Link, "Possible: component Page, src/Page.tsx:2"), "{nodes:#?}");
    assert!(has(&nodes, gpui::Role::Button, "What changes if I change Card"));
    assert!(has(&nodes, gpui::Role::Button, "Check again"));
    assert!(has(&nodes, gpui::Role::Button, "Open the CSS class page"));
    let spoken: Vec<&str> = nodes.iter().map(|node| node.label.as_str()).collect();
    let prose: Vec<String> = said(&mut rig).into_iter().filter(|line| line.split_whitespace().count() >= 3).collect();
    assert!(prose.len() >= 6, "the page paints sentences: {prose:#?}");
    let silent: Vec<&String> = prose.iter().filter(|line| !spoken.iter().any(|said| said.contains(line.as_str()) || line.contains(said))).collect();
    assert!(silent.is_empty(), "visible but absent from the native tree: {silent:#?}\n{spoken:#?}");
}

#[gpui::test]
fn the_keyboard_reaches_every_row_and_enter_opens_its_source(cx: &mut TestAppContext) {
    let canned = serve_impact(card_impact(1));
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    let editor = recorder(&mut rig);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let walk = targets(&mut rig);
    for expected in [
        "Check again",
        "Open the CSS class page",
        "Exact: component Card, src/Card.tsx:7",
        "What changes if I change Card",
        "Possible: component Page, src/Page.tsx:2",
        "What changes if I change Page",
    ] {
        assert!(walk.iter().any(|label| label == expected), "J cannot reach {expected:?}: {walk:#?}");
    }
    walk_to(&mut rig, "Exact: component Card, src/Card.tsx:7");
    rig.keys("enter");
    let ran = editor.borrow();
    assert_eq!(ran.len(), 1, "one request to the editor");
    assert_eq!(ran[0].args, ["-g".to_owned(), "/workspace/shop/src/Card.tsx:7".to_owned()], "the file the row is in, at its line");
}

#[gpui::test]
fn a_row_can_start_the_next_question_and_back_returns_to_the_row(cx: &mut TestAppContext) {
    let canned = Canned::new({
        let card = card_impact(1);
        let page = ImpactModel {
            subject: subject(NativeKind::CodeComponent, "Card"),
            seeds: vec![NodeView { domain: NativeDomain::Code, kind: "component".into(), name: "Card".into(), site: site("src/Card.tsx", 7), follow: None }],
            exact: 1,
            possible: 0,
            rows: vec![component(1, None, Strength::Exact, "Page", "src/Page.tsx", 2, "renders Card")],
            gaps: Vec::new(),
            ..card.clone()
        };
        move |key| match key {
            BrowseKey::Impact(route) if route.subject == card.subject => Some(Ok(BrowseValue::Impact(Arc::new(card.clone())))),
            BrowseKey::Impact(route) if route.subject == page.subject => Some(Ok(BrowseValue::Impact(Arc::new(page.clone())))),
            _ => None,
        }
    });
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    walk_to(&mut rig, "What changes if I change Card");
    rig.keys("enter");
    assert_eq!(rig.route(), impact_route(NativeKind::CodeComponent, "Card"));
    assert!(says(&mut rig, "Changing Card affects 1 exact and 0 possible, in 2 files."), "{:#?}", said(&mut rig));
    rig.keys("cmd-[");
    assert_eq!(rig.route(), impact_route(NativeKind::CssClass, "card"));
    assert_eq!(focused(&mut rig).as_deref(), Some("native-impact-follow-1"), "Back lands on the control that left");
}

#[gpui::test]
fn a_list_that_has_more_pages_offers_them_and_one_that_is_full_says_so(cx: &mut TestAppContext) {
    let canned = Canned::new(|key| match key {
        BrowseKey::Impact(route) => {
            let mut model = card_impact(route.pages);
            model.exact = 3;
            model.possible = 0;
            let rows: Vec<ImpactRowView> = (1..=u32::from(route.pages))
                .map(|n| component(n, None, Strength::Exact, &format!("Row{n}"), &format!("src/Row{n}.tsx"), n, "a user"))
                .collect();
            model.rows = rows;
            model.gaps.clear();
            model.end = if route.pages >= crate::navigation::IMPACT_MAX_PAGES {
                ListEnd::ViewFull
            } else if route.pages < 3 {
                ListEnd::More
            } else {
                ListEnd::Complete
            };
            Some(Ok(BrowseValue::Impact(Arc::new(model))))
        }
        _ => None,
    });
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    assert!(says(&mut rig, "Showing 1 of 3."), "{:#?}", said(&mut rig));
    assert!(targets(&mut rig).iter().any(|label| label == "Show more"));
    walk_to(&mut rig, "Show more");
    rig.keys("enter");
    let Route::Orbit(OrbitRoute::Browse(BrowseRoute::Impact(now))) = rig.route() else { panic!("still an impact page") };
    assert_eq!(now.pages, 2, "the question holds one more page");
    assert!(says(&mut rig, "Row2"), "{:#?}", said(&mut rig));
    assert!(says(&mut rig, "Row1"), "the first page is still there");
    walk_to(&mut rig, "Show more");
    rig.keys("enter");
    assert!(says(&mut rig, "Row3"));
    assert!(!targets(&mut rig).iter().any(|label| label == "Show more"), "a complete list offers no more");
    // A view at its cap says it is not complete rather than pretending to be.
    rig.go(Intent::Navigate(route(BrowseRoute::Impact(ImpactRoute::new(subject(NativeKind::CssClass, "card")).with_pages(crate::navigation::IMPACT_MAX_PAGES)))));
    assert!(said(&mut rig).iter().any(|line| line.contains("The rest exist but are not shown; the list is not complete.")), "{:#?}", said(&mut rig));
}

#[gpui::test]
fn zero_rows_never_read_as_no_impact_unless_the_owner_proved_it(cx: &mut TestAppContext) {
    // Nothing found, a gap declared: not proof.
    let mut unproved = card_impact(1);
    unproved.exact = 0;
    unproved.possible = 0;
    unproved.rows.clear();
    let canned = serve_impact(unproved.clone());
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line == "No affected rows were found for .card, but that does not prove it has no impact."), "{lines:#?}");
    assert!(lines.iter().any(|line| line.starts_with("Not proved because code.dynamic-class (3)")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("Nothing depends")), "{lines:#?}");

    // The same answer when the owner proves it: the only way to say nothing.
    let mut proved = unproved;
    proved.gaps.clear();
    proved.proves_no_impact = true;
    canned.answer_with(move |key| match key {
        BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..proved.clone() })))),
        _ => None,
    });
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "proved")));
    let lines = said(&mut rig);
    assert!(
        lines.iter().any(|line| line == "Nothing depends on .proved: every language was fully analysed and nothing was left unseen."),
        "{lines:#?}"
    );
}

#[gpui::test]
fn a_name_that_was_not_found_was_not_searched_or_is_ambiguous_says_which(cx: &mut TestAppContext) {
    let mut missing = card_impact(1);
    missing.seed = SeedView::NotFound("The analysed scope holds no such name.".into());
    missing.seeds.clear();
    missing.exact = 0;
    missing.possible = 0;
    missing.rows.clear();
    let first = missing.clone();
    let canned = Canned::new(move |key| match key {
        BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..first.clone() })))),
        _ => None,
    });
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.contains("No CSS class named .card was found in what was analysed.") && line.contains("not evidence")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.starts_with("Exact") || line.starts_with("Possible")), "no rows, no row headings: {lines:#?}");

    let mut unavailable = missing.clone();
    unavailable.seed = SeedView::Unavailable("no CSS worker is installed".into());
    unavailable.domains = vec![DomainView { domain: NativeDomain::Css, state: DomainStateView::Unavailable("no CSS worker is installed".into()), files: 0 }];
    canned.answer_with(move |key| match key {
        BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..unavailable.clone() })))),
        _ => None,
    });
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "other")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.contains("Nothing could be searched") && line.contains("says nothing about its impact")), "{lines:#?}");
    assert!(lines.iter().any(|line| line == "CSS could not be read: no CSS worker is installed."), "{lines:#?}");

    let mut ambiguous = missing;
    ambiguous.seed = SeedView::Ambiguous;
    ambiguous.seeds = vec![
        NodeView { domain: NativeDomain::Css, kind: "class".into(), name: "card".into(), site: site("src/a.css", 1), follow: Some(NativeSubject::new(project(), NativeKind::CssClass, "card", Some("src/a.css")).expect("a")) },
        NodeView { domain: NativeDomain::Css, kind: "class".into(), name: "card".into(), site: site("src/b.css", 9), follow: Some(NativeSubject::new(project(), NativeKind::CssClass, "card", Some("src/b.css")).expect("b")) },
    ];
    canned.answer_with(move |key| match key {
        BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..ambiguous.clone() })))),
        _ => None,
    });
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "third")));
    let walk = targets(&mut rig);
    assert!(walk.iter().any(|label| label == "Follow class card, src/a.css:1"), "{walk:#?}");
    assert!(walk.iter().any(|label| label == "Follow class card, src/b.css:9"), "{walk:#?}");
    assert!(said(&mut rig).iter().any(|line| line.contains("matches more than one thing, so nothing was followed")));
}

#[gpui::test]
fn a_stale_answer_says_what_changed_and_checking_again_reads_it_afresh(cx: &mut TestAppContext) {
    let mut stale = card_impact(1);
    stale.freshness = FreshnessView::Stale { checked: 6, changed: vec!["src/card.css".into()], action: "re-index the project, then ask again".into() };
    let canned = serve_impact(stale);
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    let notice = lines.iter().find(|line| line.starts_with("This answer is out of date.")).unwrap_or_else(|| panic!("no stale notice: {lines:#?}"));
    assert!(notice.contains("src/card.css") && notice.contains("re-index the project, then ask again"), "{notice}");
    assert!(!lines.iter().any(|line| line.starts_with("Checked 6 files")), "a stale answer does not also say it is fresh");
    let nodes = native(&mut rig);
    assert!(nodes.iter().any(|node| node.role == gpui::Role::Status && node.label.starts_with("This answer is out of date")), "the warning is a status node");
    assert!(has(&nodes, gpui::Role::Button, "Check again"));
    assert!(!has(&nodes, gpui::Role::Button, "Re-index shop"), "a project that is not on the shelf cannot be re-indexed from here");

    // The files are indexed again: asking again reads the answer afresh.
    let before = canned.reads_of(|key| matches!(key, BrowseKey::Impact(_)));
    let fresh = card_impact(1);
    canned.answer_with(move |key| matches!(key, BrowseKey::Impact(_)).then(|| Ok(BrowseValue::Impact(Arc::new(fresh.clone())))));
    walk_to(&mut rig, "Check again");
    rig.keys("enter");
    assert!(canned.reads_of(|key| matches!(key, BrowseKey::Impact(_))) > before, "the owner was asked again");
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line == "Checked 6 files against the index: they match."), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.starts_with("This answer is out of date.")), "the warning goes with the staleness: {lines:#?}");
}

#[gpui::test]
fn a_stale_answer_for_a_project_on_the_shelf_offers_to_index_it_again(cx: &mut TestAppContext) {
    let mut stale = card_impact(1);
    stale.freshness = FreshnessView::Stale { checked: 6, changed: vec!["src/card.css".into()], action: "re-index the project, then ask again".into() };
    let canned = serve_impact(stale);
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::AddProject { project: project() });
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let walk = targets(&mut rig);
    assert!(walk.iter().any(|label| label == "Re-index shop"), "{walk:#?}");
    assert!(walk.iter().any(|label| label == "Check again"), "{walk:#?}");
    assert!(has(&native(&mut rig), gpui::Role::Button, "Re-index shop"));
}

#[gpui::test]
fn an_earlier_reading_is_shown_as_earlier_and_a_failed_read_says_so_with_a_way_to_try_again(cx: &mut TestAppContext) {
    let canned = Canned::new(|key| match key {
        BrowseKey::Impact(_) => Some(Err(ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Transport, "the owner stopped answering")))),
        _ => None,
    });
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line == "the owner stopped answering"), "{lines:#?}");
    assert!(lines.iter().any(|line| line == "READ-TRANSPORT"), "{lines:#?}");
    assert!(has(&native(&mut rig), gpui::Role::Button, "Try again"));
}

// ---- SQL ---------------------------------------------------------------------------

fn sql_site(path: &str, line: u32, action: &str, live: bool) -> SqlSite {
    SqlSite { site: Site { path: path.into(), line: Some(line) }, action: action.into(), written: Some("orders".into()), live, verified: Verified::Matches }
}

fn orders_model() -> SqlModel {
    SqlModel {
        subject: subject(NativeKind::SqlTable, "orders"),
        object_name: "orders".into(),
        column: None,
        object: SqlObject::Defined { kind: NativeKind::SqlTable, live: true, dialect: "SQLite".into() },
        definitions: vec![sql_site("db/001_init.sql", 3, "created", true), sql_site("db/004_alter.sql", 1, "altered", true)],
        columns: vec![
            SqlColumn { name: "total".into(), live: true, sites: vec![sql_site("db/001_init.sql", 4, "created", true)] },
            SqlColumn { name: "note".into(), live: false, sites: vec![sql_site("db/001_init.sql", 5, "created", false), sql_site("db/003_drop.sql", 1, "dropped as a column", false)] },
        ],
        uses: vec![
            SqlUse { site: Site { path: "queries/recent.sql".into(), line: Some(2) }, statement: "SELECT".into(), role: "reads".into(), column: None, within: Some("view recent_orders".into()), live: true, in_code: false, verified: Verified::Matches },
            SqlUse { site: Site { path: "src/repo.ts".into(), line: Some(31) }, statement: "UPDATE".into(), role: "writes".into(), column: Some("total".into()), within: None, live: false, in_code: true, verified: Verified::Matches },
        ],
        uses_incomplete: false,
        users: UsersView {
            state: UsersState::Read,
            rows: vec![component(1, None, Strength::Exact, "OrderList", "src/OrderList.tsx", 4, "runs a query on orders")],
            exact: 1,
            possible: 0,
            incomplete: false,
            gaps: vec![GapView { facet: "code.dynamic-sql".into(), note: "SQL built from strings".into(), count: None }],
            freshness: Some(FreshnessView::Current { checked: 3 }),
        },
        coverage: Coverage { analysed: true, complete: false, notes: vec!["1 statement was only partly understood: a mention in it may be missing.".into(), "This is what the project's SQL files say. Nothing was read from a running database.".into()] },
        gap_sites: vec![SqlGapSite { path: "db/009_fn.sql".into(), why: "a CREATE FUNCTION statement was only partly read: its body is opaque to the analysis".into() }],
        changed: Vec::new(),
    }
}

fn serve_sql(model: SqlModel) -> Canned {
    Canned::new(move |key| match key {
        BrowseKey::Sql(subject) if *subject == model.subject => Some(Ok(BrowseValue::Sql(Arc::new(model.clone())))),
        _ => None,
    })
}

fn sql_route(name: &str) -> Route {
    route(BrowseRoute::Sql(subject(NativeKind::SqlTable, name)))
}

#[gpui::test]
fn the_sql_page_shows_definition_columns_migrations_uses_and_gaps_each_opening_its_source(cx: &mut TestAppContext) {
    let canned = serve_sql(orders_model());
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    let editor = recorder(&mut rig);
    rig.go(Intent::Navigate(sql_route("orders")));
    let lines = said(&mut rig);
    for expected in [
        "SQL table (SQLite) in shop",
        "orders",
        "2 statements define or change it. It exists after the last statement in the declared order.",
        "Where it is defined",
        "db/001_init.sql:3",
        "Migrations that touched it",
        "db/004_alter.sql:1",
        "Column note: dropped as a column",
        "Columns · 2",
        "total",
        "Dropped: it does not exist after the last statement in the declared order.",
        "Used in SQL · 1",
        "Used in code · 1",
        "queries/recent.sql:2",
        "SELECT reads, inside view recent_orders",
        "src/repo.ts:31",
        "UPDATE writes total",
        "Bound to a definition that was later dropped.",
        "Other things that depend on it",
        "What this cannot see",
        "1 statement was only partly understood: a mention in it may be missing.",
        "db/009_fn.sql: a CREATE FUNCTION statement was only partly read: its body is opaque to the analysis.",
        "Source re-read and matches the index",
    ] {
        assert!(lines.iter().any(|line| line == expected), "{expected:?} is not on screen: {lines:#?}");
    }
    let nodes = native(&mut rig);
    assert!(has(&nodes, gpui::Role::Heading, "orders"));
    assert!(has(&nodes, gpui::Role::Button, "What changes if I change orders"));
    assert!(has(&nodes, gpui::Role::Link, "db/001_init.sql:3, created"));
    assert!(has(&nodes, gpui::Role::Link, "src/repo.ts:31, UPDATE writes total"));
    assert!(has(&nodes, gpui::Role::Button, "What changes if I change orders.note"));

    walk_to(&mut rig, "src/repo.ts:31, UPDATE writes total");
    rig.keys("enter");
    assert_eq!(editor.borrow().last().map(|command| command.args.clone()), Some(vec!["-g".to_owned(), "/workspace/shop/src/repo.ts:31".to_owned()]));
}

#[gpui::test]
fn on_the_sql_page_one_key_asks_what_changes_and_the_columns_ask_for_themselves(cx: &mut TestAppContext) {
    let canned = Canned::new({
        let sql = orders_model();
        let table = ImpactModel { subject: subject(NativeKind::SqlTable, "orders"), seeds: Vec::new(), ..card_impact(1) };
        move |key| match key {
            BrowseKey::Sql(_) => Some(Ok(BrowseValue::Sql(Arc::new(sql.clone())))),
            BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..table.clone() })))),
            _ => None,
        }
    });
    let mut rig = open(cx, &canned, 1440.0, 1000.0);
    rig.go(Intent::Navigate(sql_route("orders")));
    rig.keys("i");
    assert_eq!(rig.route(), impact_route(NativeKind::SqlTable, "orders"), "I asks about the page's own table");
    rig.keys("cmd-[");
    assert_eq!(rig.route(), sql_route("orders"));
    walk_to(&mut rig, "What changes if I change orders.note");
    rig.keys("enter");
    assert_eq!(rig.route(), impact_route(NativeKind::SqlColumn, "orders.note"));
}

#[gpui::test]
fn an_unanalysed_or_unknown_table_is_never_called_absent(cx: &mut TestAppContext) {
    let mut unavailable = orders_model();
    unavailable.object = SqlObject::Unavailable("SQL has not been analysed in this project. Index it to read its SQL.".into());
    unavailable.definitions.clear();
    unavailable.columns.clear();
    unavailable.uses.clear();
    unavailable.gap_sites.clear();
    unavailable.users = UsersView::unavailable("the SQL analysis has nothing to follow");
    unavailable.coverage = Coverage { analysed: false, complete: false, notes: vec!["SQL has not been analysed in this project. Index it to read its SQL.".into()] };
    let canned = serve_sql(unavailable);
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(sql_route("orders")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.starts_with("The SQL analysis could not be read: SQL has not been analysed") && line.ends_with("Nothing below is evidence about orders.")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("found no table or view")), "{lines:#?}");
    assert!(!targets(&mut rig).iter().any(|label| label.starts_with("What changes")), "nothing to ask about an unread table");

    let mut not_defined = orders_model();
    not_defined.object = SqlObject::NotDefined;
    not_defined.definitions.clear();
    not_defined.columns.clear();
    not_defined.uses.clear();
    let canned = serve_sql(not_defined);
    let mut rig2 = open(cx, &canned, 1440.0, 900.0);
    rig2.go(Intent::Navigate(sql_route("orders")));
    let lines = said(&mut rig2);
    assert!(lines.iter().any(|line| line.contains("found no table or view named orders") && line.contains("This is not proof that it does not exist.")), "{lines:#?}");
}

#[gpui::test]
fn a_sql_page_built_from_files_that_changed_says_so_and_offers_to_index_again(cx: &mut TestAppContext) {
    let mut changed = orders_model();
    changed.changed = vec!["queries/recent.sql".into()];
    changed.uses[0].verified = Verified::Changed;
    changed.uses[0].site.line = None;
    let canned = serve_sql(changed);
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    rig.go(Intent::AddProject { project: project() });
    rig.go(Intent::Navigate(sql_route("orders")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.starts_with("This page is out of date. Changed since indexed: queries/recent.sql.")), "{lines:#?}");
    assert!(lines.iter().any(|line| line == "Source changed since it was indexed"));
    assert!(lines.iter().any(|line| line == "queries/recent.sql"), "a line nobody could re-read is not invented: {lines:#?}");
    let walk = targets(&mut rig);
    assert!(walk.iter().any(|label| label == "Re-index shop") && walk.iter().any(|label| label == "Check again"), "{walk:#?}");
}

// ---- CSS ----------------------------------------------------------------------------

fn card_css() -> CssModel {
    CssModel {
        subject: subject(NativeKind::CssClass, "card"),
        state: CssState::Found,
        rules: vec![
            CssRule { site: Site { path: "src/card.css".into(), line: Some(12) }, approximate: false, selector: ".card".into(), condition: None, written: Some(".card".into()), verified: Verified::Matches },
            CssRule { site: Site { path: "src/other.css".into(), line: Some(1) }, approximate: true, selector: ".card:hover".into(), condition: Some("(min-width: 40em)".into()), written: None, verified: Verified::Unread },
        ],
        declarations: Vec::new(),
        reads: Vec::new(),
        incomplete: false,
        users: UsersView {
            state: UsersState::Read,
            rows: vec![
                component(1, None, Strength::Exact, "Card", "src/Card.tsx", 3, "imports the module"),
                component(2, None, Strength::Possible, "Tile", "src/Tile.tsx", 5, "a literal className token"),
            ],
            exact: 1,
            possible: 1,
            incomplete: false,
            gaps: vec![GapView { facet: "code.dynamic-class".into(), note: "class names built at run time".into(), count: Some(2) }],
            freshness: Some(FreshnessView::Current { checked: 4 }),
        },
        coverage: Coverage { analysed: true, complete: false, notes: vec!["Which elements a rule matches, and which rule wins the cascade, are not worked out.".into()] },
        changed: Vec::new(),
    }
}

fn serve_css(model: CssModel) -> Canned {
    Canned::new(move |key| match key {
        BrowseKey::Css(subject) if *subject == model.subject => Some(Ok(BrowseValue::Css(Arc::new(model.clone())))),
        _ => None,
    })
}

fn css_route(kind: NativeKind, name: &str) -> Route {
    route(BrowseRoute::Css(subject(kind, name)))
}

#[gpui::test]
fn the_css_page_shows_rules_with_exact_source_and_the_components_that_use_it(cx: &mut TestAppContext) {
    let canned = serve_css(card_css());
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    let editor = recorder(&mut rig);
    rig.go(Intent::Navigate(css_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    for expected in [
        "CSS class in shop",
        ".card",
        "2 places in the stylesheets name .card.",
        "Rules · 2",
        "src/card.css:12",
        "src/other.css:1 (line is approximate)",
        "Inside (min-width: 40em)",
        "Written as .card",
        "Source re-read and matches the index",
        "Components that use it",
        "1 exact, 1 possible.",
        "Exact",
        "Possible",
        "a literal className token",
        "code.dynamic-class (2): class names built at run time",
        "What this cannot see",
        "Which elements a rule matches, and which rule wins the cascade, are not worked out.",
    ] {
        assert!(lines.iter().any(|line| line == expected), "{expected:?} is not on screen: {lines:#?}");
    }
    let nodes = native(&mut rig);
    assert!(has(&nodes, gpui::Role::Link, "src/card.css:12, .card"));
    assert!(has(&nodes, gpui::Role::Link, "Exact: component Card, src/Card.tsx:3"));
    assert!(has(&nodes, gpui::Role::Link, "Possible: component Tile, src/Tile.tsx:5"));
    assert!(has(&nodes, gpui::Role::Button, "What changes if I change .card"));
    walk_to(&mut rig, "src/card.css:12, .card");
    rig.keys("enter");
    assert_eq!(editor.borrow().last().map(|command| command.args.clone()), Some(vec!["-g".to_owned(), "/workspace/shop/src/card.css:12".to_owned()]));
}

#[gpui::test]
fn a_custom_property_page_separates_its_declarations_from_what_reads_them(cx: &mut TestAppContext) {
    let mut model = card_css();
    model.subject = subject(NativeKind::CssCustomProperty, "--brand");
    model.rules.clear();
    model.declarations = vec![CssDeclaration {
        site: Site { path: "src/theme.css".into(), line: Some(2) },
        value: Some("#336699".into()),
        important: true,
        selector: None,
        written: Some("--brand".into()),
        verified: Verified::Matches,
    }];
    model.reads = vec![CssRead { site: Site { path: "src/card.css".into(), line: Some(5) }, property: "color".into(), how: "var".into() }];
    let canned = serve_css(model);
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    rig.go(Intent::Navigate(css_route(NativeKind::CssCustomProperty, "--brand")));
    let lines = said(&mut rig);
    for expected in ["CSS custom property in shop", "--brand", "Declared · 1", "--brand: #336699 !important", "Read by · 1", "color reads it with var()", "src/card.css:5"] {
        assert!(lines.iter().any(|line| line == expected), "{expected:?} is not on screen: {lines:#?}");
    }
}

#[gpui::test]
fn an_unanalysed_stylesheet_is_unavailable_and_an_unknown_class_is_not_proof_of_absence(cx: &mut TestAppContext) {
    let mut unavailable = card_css();
    unavailable.state = CssState::Unavailable("CSS has not been analysed in this project. Index it to read its stylesheets.".into());
    unavailable.rules.clear();
    unavailable.users = UsersView::unavailable("the CSS analysis has nothing to follow");
    unavailable.coverage = Coverage { analysed: false, complete: false, notes: vec!["CSS has not been analysed in this project. Index it to read its stylesheets.".into()] };
    let canned = serve_css(unavailable);
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(css_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.starts_with("The CSS analysis could not be read:") && line.ends_with("Nothing below is evidence about .card.")), "{lines:#?}");
    assert!(!targets(&mut rig).iter().any(|label| label.starts_with("What changes")), "nothing to ask about an unread class");

    let mut missing = card_css();
    missing.state = CssState::NotFound;
    missing.rules.clear();
    missing.users = UsersView { rows: Vec::new(), exact: 0, possible: 0, ..card_css().users };
    let canned = serve_css(missing);
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(css_route(NativeKind::CssClass, "card")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.contains("found nothing named .card") && line.contains("This is not proof that it is unused.")), "{lines:#?}");
    assert!(lines.iter().any(|line| line.starts_with("None were found, but that does not prove nothing uses it")), "{lines:#?}");
}

// ---- Find and the whole journey --------------------------------------------------------

fn find_route(text: &str) -> Route {
    route(BrowseRoute::Find(crate::model::pages::SearchQuery::new(text, 200).expect("query")))
}

fn hits(query: &str) -> NativeFind {
    NativeFind {
        asked: true,
        searched: 2,
        notes: vec!["CSS has not been analysed in api, so it was not searched there.".into()],
        hits: if query == "orders" {
            vec![NativeHit { subject: subject(NativeKind::SqlTable, "orders"), site: site("db/001_init.sql", 3), detail: "SQL table in shop".into() }]
        } else {
            Vec::new()
        },
    }
}

#[gpui::test]
fn find_names_the_projects_sql_and_css_and_says_what_it_could_not_search(cx: &mut TestAppContext) {
    let canned = Canned::new(|key| match key {
        BrowseKey::FindNative(query) => Some(Ok(BrowseValue::FindNative(Arc::new(hits(&query.text))))),
        _ => None,
    });
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    rig.go(Intent::Navigate(find_route("orders")));
    let lines = said(&mut rig);
    for expected in ["In your projects' SQL and CSS", "orders", "SQL table in shop", "db/001_init.sql:3", "CSS has not been analysed in api, so it was not searched there."] {
        assert!(lines.iter().any(|line| line == expected), "{expected:?} is not on screen: {lines:#?}");
    }
    let nodes = native(&mut rig);
    assert!(has(&nodes, gpui::Role::Link, "orders, SQL table in shop"));
    assert!(has(&nodes, gpui::Role::Button, "What changes if I change orders"));
    assert!(has(&nodes, gpui::Role::Heading, "In your projects' SQL and CSS"));

    // Nothing by that name, and not everything was searched: Find does not say "no such table".
    rig.go(Intent::Navigate(find_route("nothing_here")));
    let lines = said(&mut rig);
    assert!(lines.iter().any(|line| line.starts_with("Nothing named nothing_here was found in the 2 projects searched, but not everything could be searched")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("No SQL table, view, column")), "{lines:#?}");
}

#[gpui::test]
fn find_says_nothing_about_sql_and_css_for_a_query_too_short_to_search(cx: &mut TestAppContext) {
    let canned = Canned::new(|key| match key {
        BrowseKey::FindNative(_) => Some(Ok(BrowseValue::FindNative(Arc::new(NativeFind::default())))),
        _ => None,
    });
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(find_route("a")));
    assert!(!said(&mut rig).iter().any(|line| line.contains("SQL and CSS")), "{:#?}", said(&mut rig));
}

#[gpui::test]
fn the_main_journey_runs_from_find_to_a_table_to_where_it_is_used_to_what_changes(cx: &mut TestAppContext) {
    let canned = Canned::new({
        let sql = orders_model();
        let impact = ImpactModel { subject: subject(NativeKind::SqlTable, "orders"), seeds: Vec::new(), gaps: Vec::new(), ..card_impact(1) };
        move |key| match key {
            BrowseKey::FindNative(query) => Some(Ok(BrowseValue::FindNative(Arc::new(hits(&query.text))))),
            BrowseKey::Sql(_) => Some(Ok(BrowseValue::Sql(Arc::new(sql.clone())))),
            BrowseKey::Impact(route) => Some(Ok(BrowseValue::Impact(Arc::new(ImpactModel { subject: route.subject.clone(), ..impact.clone() })))),
            _ => None,
        }
    });
    let mut rig = open(cx, &canned, 1440.0, 1400.0);
    let editor = recorder(&mut rig);
    // 1. Find the table.
    rig.go(Intent::Navigate(find_route("orders")));
    tab_to(&mut rig, "orders, SQL table in shop");
    rig.keys("enter");
    assert_eq!(rig.route(), sql_route("orders"), "Find opens the table's page");
    // 2. See where it is used.
    assert!(says(&mut rig, "Used in code · 1"));
    walk_to(&mut rig, "src/repo.ts:31, UPDATE writes total");
    rig.keys("enter");
    assert_eq!(editor.borrow().last().map(|command| command.args[1].clone()), Some("/workspace/shop/src/repo.ts:31".to_owned()), "a use opens its source");
    // 3. See what a change affects: one key from the page.
    rig.keys("escape");
    rig.keys("i");
    assert_eq!(rig.route(), impact_route(NativeKind::SqlTable, "orders"));
    assert!(said(&mut rig).iter().any(|line| line.starts_with("Changing orders affects")), "{:#?}", said(&mut rig));
    // 4. And back.
    rig.keys("cmd-[");
    assert_eq!(rig.route(), sql_route("orders"));
    rig.keys("cmd-[");
    assert_eq!(rig.route(), find_route("orders"));
}

#[gpui::test]
fn i_where_there_is_nothing_to_ask_about_changes_nothing(cx: &mut TestAppContext) {
    let canned = Canned::new(|_| None);
    let mut rig = open(cx, &canned, 1440.0, 900.0);
    rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
    rig.keys("i");
    assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Home));
    assert_eq!(canned.reads_of(|_| true), 0);
}

#[test]
fn a_declaration_of_a_local_project_asks_about_a_component_of_that_name() {
    // A registry release is not a project of yours: there is nothing of yours to ask about.
    let registry = Route::Symbol(crate::navigation::SymbolRoute {
        project: None,
        package: crate::core::PackageId::new("pkg:cargo/toml@0.8.23").expect("package"),
        id: crate::navigation::Coordinate::new("pkg:cargo/toml@0.8.23::src/de.rs:1::Value").expect("coordinate"),
        at: None,
        view: crate::navigation::View::Page,
        line: None,
        selected: None,
    });
    assert_eq!(NativeSubject::for_route(&registry), None);
    let local = Route::Symbol(crate::navigation::SymbolRoute {
        project: None,
        package: crate::core::PackageId::new("/workspace/shop").expect("package"),
        id: crate::navigation::Coordinate::new("/workspace/shop::src/Card.tsx:7::Card").expect("coordinate"),
        at: None,
        view: crate::navigation::View::Page,
        line: None,
        selected: None,
    });
    let asked = NativeSubject::for_route(&local).expect("a local declaration asks about a component");
    assert_eq!((asked.kind, &*asked.name), (NativeKind::CodeComponent, "Card"));
}

// ---- Narrow and wide -------------------------------------------------------------------

fn pages_under_test() -> Vec<(&'static str, Canned, Route)> {
    let mut stale = card_impact(1);
    stale.freshness = FreshnessView::Stale { checked: 6, changed: vec!["src/card.css".into()], action: "re-index the project, then ask again".into() };
    vec![
        ("impact", serve_impact(stale), impact_route(NativeKind::CssClass, "card")),
        ("sql", serve_sql(orders_model()), sql_route("orders")),
        ("css", serve_css(card_css()), css_route(NativeKind::CssClass, "card")),
    ]
}

#[gpui::test]
fn the_pages_paint_clean_and_keep_their_controls_in_reach_at_every_width(cx: &mut TestAppContext) {
    for (name, canned, route) in pages_under_test() {
        let mut rig = open(cx, &canned, 1440.0, 900.0);
        rig.go(Intent::Navigate(route));
        let mut wrong = Vec::new();
        for (width, height) in SIZES.into_iter().chain(PHONES) {
            resize(&mut rig, width, height);
            let ledger = painted(&mut rig);
            assert!(!ledger.texts.is_empty(), "{name} painted nothing at {width}x{height}");
            wrong.extend(findings(&ledger, width, height).into_iter().map(|finding| format!("{name} {width:.0}x{height:.0}: {finding}")));
        }
        assert!(wrong.is_empty(), "{} findings:\n{}", wrong.len(), wrong.join("\n"));
    }
}

#[gpui::test]
fn at_a_phone_width_the_rows_wrap_and_the_keyboard_still_reaches_their_sources(cx: &mut TestAppContext) {
    let canned = serve_impact(card_impact(1));
    let mut rig = open(cx, &canned, 360.0, 640.0);
    let editor = recorder(&mut rig);
    rig.go(Intent::Navigate(impact_route(NativeKind::CssClass, "card")));
    let said = said(&mut rig);
    assert!(said.iter().any(|line| line == "Exact · 1") && said.iter().any(|line| line == "Possible · 1"), "{said:#?}");
    let walk = targets(&mut rig);
    assert!(walk.iter().any(|label| label == "Exact: component Card, src/Card.tsx:7"), "{walk:#?}");
    walk_to(&mut rig, "Exact: component Card, src/Card.tsx:7");
    rig.keys("enter");
    assert_eq!(editor.borrow().len(), 1);
    let nodes = native(&mut rig);
    assert!(has(&nodes, gpui::Role::Link, "Possible: component Page, src/Page.tsx:2"), "the native tree is the same at a phone width");
}
