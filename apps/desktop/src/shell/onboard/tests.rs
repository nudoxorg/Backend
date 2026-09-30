//! First run through the real window root: the empty Library, ⌘O, the
//! dialog, admission, and what a running index says. Every assertion is on
//! what the window drew or held (words in the probe ledger, the snapshot the
//! reducer produced, where focus stood), never a count of calls.

#![allow(clippy::expect_used, clippy::panic)]

use crate::model::pages::{Gap, GapReason, Known, OrbitModel, PageValue, ReadFailure};
use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::{Intent, OrbitRoute, Overlay, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::shell::tests::{Fixture, RootOnly, Rig, rig_with_engine, rig_with_reads};
use std::sync::atomic::{AtomicUsize, Ordering};
use facet::overlay::dialog;
use gpui::{Focusable as _, TestAppContext};
use std::path::PathBuf;
use std::sync::Arc;

/// A library nobody has added anything to: the orbit read is empty, so the
/// first-run screen is what the Library shows.
struct NothingYet;

impl PageReader for NothingYet {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        match request {
            ReadRequest::Orbit => Ok(PageValue::Orbit(OrbitModel {
                indexed: Known::Known(Arc::from([])),
                projects: Known::Known(Arc::from([])),
                explore: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                tree: Known::Unknown(Gap::new(GapReason::NotServed, "")),
            })),
            other => Fixture.read(other, context),
        }
    }
}

fn first_run(cx: &mut TestAppContext, width: f32) -> Rig {
    rig_with_reads(cx, Some(Route::Orbit(OrbitRoute::Home)), width, 900.0, ReadPool::start(2, |_| NothingYet).expect("pool"))
}

/// A folder this test owns, holding a Rust manifest, under a scratch parent.
fn project(tag: &str) -> (PathBuf, PathBuf) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let parent = std::env::temp_dir().join(format!("nudox-onboard-{tag}-{}-{nonce}", std::process::id()));
    let folder = parent.join("toml_pin");
    std::fs::create_dir_all(folder.join("src")).expect("project folder");
    std::fs::write(folder.join("Cargo.toml"), "[package]\nname = \"toml-pin-fixture\"\nversion = \"0.1.0\"\n").expect("manifest");
    (parent.canonicalize().expect("parent"), folder.canonicalize().expect("folder"))
}

/// Every text run the window drew this frame, in paint order.
fn drawn(rig: &mut Rig) -> Vec<String> {
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    rig.cx.update(|_, cx| facet::probe::take(cx)).texts.into_iter().map(|text| text.content).collect()
}

fn overlay(rig: &mut Rig) -> Option<Overlay> {
    rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay())
}

fn snapshot(rig: &mut Rig) -> Arc<AppSnapshot> {
    rig.graph.store.read_with(rig.cx, |store, _| store.snapshot())
}

fn dialog_open(rig: &mut Rig) -> bool {
    rig.cx.update(|window, cx| dialog::is_open(window, cx))
}

/// Types `text` into whatever holds focus, then lets the disk's answer land.
fn type_in(rig: &mut Rig, text: &str) {
    rig.cx.simulate_input(text);
    rig.settle();
}

#[gpui::test]
fn the_first_screen_says_what_the_app_is_for_and_offers_the_way_in(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    let said = rig.said();
    for words in [
        "Read the code you depend on.",
        "Add a project folder. Nudox compiles it and every package it uses, then keeps them together here, ready to browse.",
        "Your source stays on this machine.",
    ] {
        assert!(said.iter().any(|line| line == words), "the first screen does not say {words:?}: {said:#?}");
    }
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let add = targets.iter().find(|(target, _)| target.label == "Add a folder").expect("the way in is a target");
    assert!(add.1.size.width > gpui::px(40.0) && add.1.size.height > gpui::px(16.0), "and it has a body: {:?}", add.1);
    let words = drawn(&mut rig);
    assert!(words.iter().any(|line| line == "or press"), "the key that does the same is offered: {words:#?}");
    assert_eq!(crate::shell::keys::cap(crate::shell::keys::Command::AddFolder), "⌘O", "and it is ⌘O");
}

#[gpui::test]
fn pressing_the_button_opens_the_dialog_with_the_cursor_in_the_field(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let (_, bounds) = targets.iter().find(|(target, _)| target.label == "Add a folder").expect("target").clone();
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject));
    assert!(dialog_open(&mut rig), "the dialog is the layer's own modal");
    let words = drawn(&mut rig);
    assert!(
        words.iter().any(|line| line == "Type or paste the path of a folder."),
        "the dialog does not say what to do: {words:#?}"
    );
    let field = rig.cx.update(|window, cx| super::field(window, cx)).expect("the field exists once the dialog opened");
    let focused = rig.cx.update(|window, cx| field.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "typing needs no click first");
}

#[gpui::test]
fn command_o_opens_the_dialog_and_escape_closes_it_and_gives_focus_back(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    let before = rig.cx.update(|window, cx| window.focused(cx));
    rig.keys("cmd-o");
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject), "⌘O reaches the same state the button does");
    assert!(dialog_open(&mut rig));
    rig.keys("escape");
    assert_eq!(overlay(&mut rig), None, "Esc closes it through the overlay");
    assert!(!dialog_open(&mut rig), "and the dialog follows the overlay");
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), before, "focus goes back where it was");
}

#[gpui::test]
fn typing_a_path_lists_the_folders_it_continues_to_and_a_project_says_what_it_is(cx: &mut TestAppContext) {
    let (parent, folder) = project("typing");
    std::fs::create_dir_all(parent.join("toml_extra")).expect("sibling");
    let mut rig = first_run(cx, 1440.0);
    rig.keys("cmd-o");
    type_in(&mut rig, &format!("{}/tom", parent.display()));
    let words = drawn(&mut rig);
    for expected in ["toml_extra", "toml_pin", "Rust"] {
        assert!(words.iter().any(|line| line == expected), "the folders it continues to do not include {expected:?}: {words:#?}");
    }
    type_in(&mut rig, "l_pin");
    let words = drawn(&mut rig);
    assert!(
        words.iter().any(|line| line == "toml_pin · a Rust project (Cargo.toml)"),
        "a path that is a project says what it is: {words:#?}"
    );
    let _ = folder;
}

#[gpui::test]
fn enter_adds_the_folder_and_lands_on_the_library_saying_what_it_is_doing(cx: &mut TestAppContext) {
    let (parent, folder) = project("admit");
    let mut rig = first_run(cx, 1440.0);
    // Opened the way J0 opens it: by pressing "Add a folder", which then
    // holds the focus the dialog would give back.
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let (_, bounds) = targets.iter().find(|(target, _)| target.label == "Add a folder").expect("target").clone();
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
    assert!(dialog_open(&mut rig));
    type_in(&mut rig, &format!("{}/toml_pin", parent.display()));
    rig.keys("enter");
    assert_eq!(overlay(&mut rig), None, "adding closes the dialog");
    assert!(!dialog_open(&mut rig));
    let focused = rig.cx.update(|window, cx| window.focused(cx));
    assert!(
        focused.is_none(),
        "an add lands on a Library that changed: focus does not go back to \"Add a folder\", whose keyboard ring would stand on a page that moved on (D6)"
    );
    let snapshot = snapshot(&mut rig);
    let admitted = snapshot.workspace().projects.iter().find(|project| project.path.as_ref() == folder.to_str().expect("utf-8")).expect("the folder is on the shelf");
    assert_eq!(admitted.phase, ProjectPhase::Indexing, "an admitted folder starts indexing");
    assert_eq!(snapshot.workspace().active.as_ref(), Some(&admitted.id));
    assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Home), "the Library is where a new project shows what it is doing");
    let said = rig.said();
    for expected in [
        "toml_pin".to_owned(),
        "indexing".to_owned(),
        "Compiling toml_pin.".to_owned(),
        "Then each package it uses is indexed from your cargo cache, one at a time. A first install takes a few minutes.".to_owned(),
        "started just now".to_owned(),
    ] {
        assert!(said.iter().any(|line| *line == expected), "the Library does not say {expected:?}: {said:#?}");
    }
    assert!(
        !said.iter().any(|line| line.contains('%')),
        "the owner reports no percentage, so none is drawn: {said:#?}"
    );
}

#[gpui::test]
fn a_path_that_names_no_folder_is_refused_in_place_and_the_refusal_goes_with_the_next_edit(cx: &mut TestAppContext) {
    let (parent, _) = project("refuse");
    let mut rig = first_run(cx, 1440.0);
    rig.keys("cmd-o");
    let missing = format!("{}/nowhere", parent.display());
    type_in(&mut rig, &missing);
    rig.keys("enter");
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject), "a refusal keeps the dialog open");
    let refusal = format!("There is no folder at {missing}. The nearest that exists is {}.", parent.display());
    assert!(drawn(&mut rig).iter().any(|line| *line == refusal), "the field says why, once, in place");
    assert!(snapshot(&mut rig).workspace().projects.is_empty(), "nothing reached the shelf");
    type_in(&mut rig, "x");
    assert!(!drawn(&mut rig).iter().any(|line| line.starts_with("There is no folder")), "the next edit takes the refusal away");
}

#[gpui::test]
fn a_folder_already_on_the_shelf_is_gone_to_not_added_twice(cx: &mut TestAppContext) {
    let (parent, folder) = project("twice");
    let mut rig = first_run(cx, 1440.0);
    let id = crate::core::LocalProjectId::from_path(&folder).expect("identity");
    rig.go(Intent::AddProject { project: id.clone() });
    rig.keys("cmd-o");
    type_in(&mut rig, &format!("{}/toml_pin", parent.display()));
    assert!(
        drawn(&mut rig).iter().any(|line| line == "toml_pin is on your shelf already. ↵ goes to it."),
        "the dialog says it is there"
    );
    rig.keys("enter");
    assert_eq!(snapshot(&mut rig).workspace().projects.len(), 1, "one folder, one project");
    assert_eq!(overlay(&mut rig), None);
}

#[gpui::test]
fn tab_takes_the_folder_the_text_continues_to(cx: &mut TestAppContext) {
    let (parent, _) = project("tab");
    let mut rig = first_run(cx, 1440.0);
    rig.keys("cmd-o");
    type_in(&mut rig, &format!("{}/tom", parent.display()));
    rig.keys("tab");
    let field = rig.cx.update(|window, cx| super::field(window, cx)).expect("field");
    let typed = rig.cx.update(|_, cx| field.read(cx).value().to_string());
    assert_eq!(typed, format!("{}/toml_pin/", parent.display()), "one folder to go to: Tab takes it");
}

#[gpui::test]
fn the_dialog_fits_a_phone_and_says_the_same_things(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 320.0);
    rig.keys("cmd-o");
    let words = drawn(&mut rig);
    assert!(
        words.iter().any(|line| line == "Type or paste the path of a folder."),
        "the dialog says nothing at 320 px: {words:#?}"
    );
}

/// The owner's real refusal of a project whose compile did not finish.
const REFUSED: &str = "command execution failed: local semantic compilation failed; prior selected semantic generation was preserved: \
    package semantic compilation failed for src/lib.rs: Compile { cause: Lowering(LoweringCause(RustGenericParameter)) }";

/// An owner that refuses every index, and counts how often it was asked.
struct Refuses(Arc<AtomicUsize>);

impl EngineClient for Refuses {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::IndexProject { project, .. } => {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(EngineFault::IndexFailed {
                    project: project.clone(),
                    error: crate::core::ErrorValue::new(crate::core::FaultCode::Protocol, REFUSED),
                })
            }
            other => RootOnly.execute(other),
        }
    }
}

fn click(rig: &mut Rig, label: &str) {
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let (_, bounds) = targets
        .iter()
        .find(|(target, _)| target.label == label)
        .unwrap_or_else(|| panic!("no target reads {label:?}: {:?}", targets.iter().map(|(target, _)| target.label.to_string()).collect::<Vec<_>>()))
        .clone();
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
}

fn phases(rig: &mut Rig) -> Vec<ProjectPhase> {
    snapshot(rig).workspace().projects.iter().map(|project| project.phase).collect()
}

#[gpui::test]
fn a_project_that_did_not_index_says_what_stopped_and_every_way_forward_works(cx: &mut TestAppContext) {
    let (parent, folder) = project("failed");
    let asked = Arc::new(AtomicUsize::new(0));
    let mut rig = rig_with_engine(
        cx,
        Some(Route::Orbit(OrbitRoute::Home)),
        1440.0,
        900.0,
        ReadPool::start(2, |_| NothingYet).expect("pool"),
        Refuses(Arc::clone(&asked)),
    );
    let id = crate::core::LocalProjectId::from_path(&folder).expect("identity");
    rig.go(Intent::AddProject { project: id });
    assert_eq!(phases(&mut rig), [ProjectPhase::Failed], "the owner's refusal is the project's phase");
    let said = rig.said();
    for expected in [
        "toml_pin stopped.",
        "The compiler could not finish reading src/lib.rs.",
        "The last good index of this project is still in use.",
        "stopped",
    ] {
        assert!(said.iter().any(|line| line == expected), "the Library does not say {expected:?}: {said:#?}");
    }
    assert!(!said.iter().any(|line| line.contains("RustGenericParameter")), "the owner's own words wait behind a disclosure: {said:#?}");

    // The owner's words, on request, exactly as it said them.
    click(&mut rig, "The index's own words");
    assert!(rig.said().iter().any(|line| line == REFUSED), "the disclosure shows the refusal verbatim: {:#?}", rig.said());

    // Try again asks the owner again (and, refused again, says so again).
    let before = asked.load(Ordering::SeqCst);
    click(&mut rig, "Try again");
    assert!(asked.load(Ordering::SeqCst) > before, "Try again reached the owner: asked {before} then {}", asked.load(Ordering::SeqCst));
    assert_eq!(phases(&mut rig), [ProjectPhase::Failed]);

    // Remove takes it off the shelf, and the Library is the first screen again.
    click(&mut rig, "Remove from shelf");
    assert!(snapshot(&mut rig).workspace().projects.is_empty(), "removed");
    assert!(rig.said().iter().any(|line| line == "Read the code you depend on."), "and the Library is empty again");
    let _ = parent;
}

#[gpui::test]
fn what_the_window_says_about_this_launch_is_said_once_and_goes_when_dismissed(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    rig.go(Intent::LibraryRebuilding { kept_at: Arc::from("/data/from-another-build") });
    let said = rig.said();
    for expected in [
        "Your library was built by an earlier version and is being rebuilt.",
        "The earlier index is kept at /data/from-another-build; nothing was deleted.",
    ] {
        assert!(said.iter().any(|line| line == expected), "not said: {expected:?}: {said:#?}");
    }
    click(&mut rig, "Got it");
    assert!(
        !rig.said().iter().any(|line| line.starts_with("Your library was built")),
        "dismissed, it does not come back: {:#?}",
        rig.said()
    );
}
