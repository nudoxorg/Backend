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


/// A real actor reply is held while the native submit/closing frame paints.
/// The guard releases it on every panic path, before the rig is dropped.
struct NativeIndexGate(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl NativeIndexGate {
    fn set(&self, ready: bool) {
        *self.0.0.lock().expect("gate") = ready;
        self.0.1.notify_all();
    }
}
impl Drop for NativeIndexGate { fn drop(&mut self) { self.set(true); } }
struct NativeIndexClient(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl EngineClient for NativeIndexClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        if let EngineRequest::IndexProject { request, project, operation, basis, .. } = request {
            let mut ready = self.0.0.lock().expect("gate");
            while !*ready { ready = self.0.1.wait(ready).expect("gate wake"); }
            Ok(EngineDto::IndexOperation {
                request: *request,
                basis: *basis,
                project: project.clone(),
                operation: operation.clone(),
                observation: crate::model::index_operation::tests::published(operation),
            })
        } else { RootOnly.execute(request) }
    }
}

#[gpui::test]
fn native_second_folder_submit_has_one_accessible_focus_owner_through_pending_and_ready(cx: &mut TestAppContext) {
    for pointer in [false, true] {
        let (first_parent, _) = project("native-first");
        let (second_parent, second_folder) = project("native-second");
        let gate = Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new()));
        let mut rig = rig_with_engine(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0,
            ReadPool::start(2, |_| NothingYet).expect("pool"), NativeIndexClient(Arc::clone(&gate)));
        let release = NativeIndexGate(gate);
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
        });
        rig.settle();
        rig.keys("cmd-o"); type_in(&mut rig, &format!("{}/toml_pin", first_parent.display()));
        rig.keys("enter");
        assert_eq!(phases(&mut rig), [ProjectPhase::Ready]);
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_some()),
            "the submitted native field hands keyboard dispatch to a live window owner");
        release.set(false);
        rig.keys("cmd-o");
        assert_eq!(overlay(&mut rig), Some(Overlay::AddProject), "the second native shortcut reopens the folder dialog");
        let field = rig.cx.update(|window, cx| super::field(window, cx)).expect("mounted input");
        assert!(rig.cx.update(|window, cx| field.read(cx).focus_handle(cx).is_focused(window)),
            "the reopened dialog owns native input before typing a second folder");
        assert!(field.read_with(rig.cx, |input, _| input.value().is_empty()), "the reopened dialog starts with an empty field");
        let second_path = format!("{}/toml_pin", second_parent.display());
        type_in(&mut rig, &second_path);
        assert_eq!(field.read_with(rig.cx, |input, _| input.value().to_string()), second_path,
            "the second path belongs to the current mounted editor");
        let words = drawn(&mut rig);
        assert!(words.iter().any(|line| line == "toml_pin · a Rust project (Cargo.toml)"),
            "actual folder preview painted for the second path: {words:?}");
        assert!(rig.cx.update(|window, cx| field.read(cx).focus_handle(cx).is_focused(window)));
        if pointer {
            let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("forced dialog AX");
            let tree: serde_json::Value = serde_json::from_str(&json).expect("AX JSON");
            let node = tree["nodes"].as_object().expect("AX nodes").values().find(|node|
                node["aria"]["role"].as_str() == Some("Button") && node["aria"]["label"].as_str() == Some("Add")).expect("actual native Add button");
            let b = &node["bounds"];
            let at = gpui::point(gpui::px((b["x"].as_f64().expect("x") + b["width"].as_f64().expect("width") / 2.0) as f32),
                gpui::px((b["y"].as_f64().expect("y") + b["height"].as_f64().expect("height") / 2.0) as f32));
            rig.cx.simulate_click(at, gpui::Modifiers::none());
        } else { rig.native_press("enter"); }
        rig.frame(16);
        assert_eq!(overlay(&mut rig), None);
        assert_eq!(phases(&mut rig), [ProjectPhase::Ready, ProjectPhase::Indexing]);
        assert!(!rig.cx.update(|window, cx| field.read(cx).focus_handle(cx).is_focused(window)), "retiring editor is not the native owner");
        assert!(rig.cx.update(|window, _| window.debug_a11y_tree_json()).is_some(), "pending transition publishes a complete native tree without duplicate-focus abort");
        release.set(true); rig.settle();
        assert_eq!(phases(&mut rig), [ProjectPhase::Ready, ProjectPhase::Ready]);
        assert_eq!(snapshot(&mut rig).workspace().active.as_ref(), Some(&crate::core::LocalProjectId::from_path(&second_folder).expect("identity")));
        rig.keys("cmd-o");
        assert_eq!(overlay(&mut rig), Some(Overlay::AddProject), "native keyboard remains usable after ready publication");
        rig.keys("escape");
        std::fs::remove_dir_all(first_parent).expect("clean first fixture");
        std::fs::remove_dir_all(second_parent).expect("clean second fixture");
    }
}


#[gpui::test]
fn native_menu_settings_keeps_independent_selected_radios_out_of_managed_focus(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    rig.settle();
    // This is the same action object and App dispatcher used by the native
    // menu callback, not an injected reducer OpenSettings intent.
    // App menu callbacks run without an already-borrowed Window. Dispatching
    // from VisualTestContext::update holds that Window mutably while App's
    // dispatcher tries to update it again, so the action never reaches Shell.
    rig.cx.cx.update(|cx| cx.dispatch_action(&crate::shell::OpenSettingsAction));
    rig.settle();
    assert!(matches!(overlay(&mut rig), Some(Overlay::Settings(_))));
    let tree = |rig: &mut Rig| -> serde_json::Value {
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("forced Settings AX");
        serde_json::from_str(&json).expect("AX JSON")
    };
    let native = tree(&mut rig);
    let selected = native["nodes"].as_object().expect("nodes").values().filter(|node|
        node["aria"]["role"].as_str() == Some("RadioButton") && node["aria"]["selected"].as_bool() == Some(true)).count();
    assert!(selected >= 3, "multiple independent selected groups are present, not stripped from accessibility: {native}");
    assert!(native["active_descendant_focus"].is_null(), "selected values are not competing claims on the Application's keyboard focus");
    let high = native["nodes"].as_object().expect("nodes").values().find(|node|
        node["aria"]["role"].as_str() == Some("RadioButton") && node["aria"]["label"].as_str() == Some("High")).expect("native contrast option");
    let b = &high["bounds"];
    rig.cx.simulate_click(gpui::point(gpui::px((b["x"].as_f64().expect("x") + b["width"].as_f64().expect("width") / 2.0) as f32),
        gpui::px((b["y"].as_f64().expect("y") + b["height"].as_f64().expect("height") / 2.0) as f32)), gpui::Modifiers::none());
    rig.settle();
    let focused = tree(&mut rig);
    let owner = focused["gpui_focus"].as_str().expect("one real native radio owner");
    assert_eq!(focused["nodes"][owner]["aria"]["label"].as_str(), Some("High"));
    assert!(focused["active_descendant_focus"].is_null());
    rig.keys("left space");
    let focused = tree(&mut rig);
    let owner = focused["gpui_focus"].as_str().expect("native reversal keeps ownership");
    assert_eq!(focused["nodes"][owner]["aria"]["label"].as_str(), Some("Normal"));
    assert_eq!(focused["nodes"][owner]["aria"]["selected"].as_bool(), Some(true));
    rig.keys("tab");
    let focused = tree(&mut rig);
    let owner = focused["gpui_focus"].as_str().expect("real native Tab advances");
    assert_ne!(focused["nodes"][owner]["aria"]["label"].as_str(), Some("Normal"));
    assert!(focused["active_descendant_focus"].is_null());
    rig.keys("escape");
    assert_eq!(overlay(&mut rig), None);
}

#[gpui::test]
fn native_settings_press_from_before_a_cover_cannot_select_after_its_return(cx: &mut TestAppContext) {
    let mut rig = first_run(cx, 1440.0);
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    rig.go(Intent::OpenSettings(crate::navigation::SettingsPage::Appearance));
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Settings tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native Settings JSON");
    let high = tree["nodes"].as_object().expect("nodes").values().find(|node|
        node["aria"]["role"].as_str() == Some("RadioButton") && node["aria"]["label"].as_str() == Some("High"))
        .expect("mounted High radio");
    let b = &high["bounds"];
    let at = gpui::point(gpui::px((b["x"].as_f64().expect("x") + b["width"].as_f64().expect("width") / 2.0) as f32),
        gpui::px((b["y"].as_f64().expect("y") + b["height"].as_f64().expect("height") / 2.0) as f32));
    rig.cx.simulate_event(gpui::MouseDownEvent { position: at, modifiers: gpui::Modifiers::none(),
        button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
    rig.go(Intent::OpenAddProject);
    assert_eq!(overlay(&mut rig), Some(Overlay::AddProject));
    rig.keys("escape");
    assert!(matches!(overlay(&mut rig), Some(Overlay::Settings(_))));
    rig.cx.simulate_event(gpui::MouseUpEvent { position: at, modifiers: gpui::Modifiers::none(),
        button: gpui::MouseButton::Left, click_count: 1 });
    rig.settle();
    assert_eq!(snapshot(&mut rig).settings().contrast, crate::model::ContrastPreference::Normal,
        "the old radio's pointer-up cannot select through a cover and return");
    rig.cx.simulate_click(at, gpui::Modifiers::none());
    rig.settle();
    assert_eq!(snapshot(&mut rig).settings().contrast, crate::model::ContrastPreference::High,
        "a new native press in the returned Settings visit still selects");
}

/// A fixture owner keeps the saved operation unknown until the explicit read.
/// It never reports success from a catalog, a timeout or a second start.
struct Reconciles { starts: Arc<AtomicUsize>, checks: Arc<AtomicUsize> }
impl EngineClient for Reconciles {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::IndexProject { request, basis, project, operation, .. } => {
                self.starts.fetch_add(1, Ordering::SeqCst);
                Ok(EngineDto::IndexOperation { request: *request, basis: *basis, project: project.clone(),
                    operation: operation.clone(), observation: backend_library::IndexOperationObservation::Unknown { operation_key: operation.key } })
            }
            EngineRequest::IndexOperationStatus { request, basis, project, operation, .. } => {
                self.checks.fetch_add(1, Ordering::SeqCst);
                Ok(EngineDto::IndexOperation { request: *request, basis: *basis, project: project.clone(),
                    operation: operation.clone(), observation: crate::model::index_operation::tests::published(operation) })
            }
            other => RootOnly.execute(other),
        }
    }
}

#[gpui::test]
fn native_check_outcome_queries_the_saved_key_and_admits_its_publication_without_starting_again(cx: &mut TestAppContext) {
    let (_, folder) = project("reconcile");
    let starts = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let authority = crate::core::VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(authority, crate::model::ServiceMode::Attached);
    let mut rig = crate::shell::tests::rig_with_engine_gate(cx, Some(Route::Orbit(OrbitRoute::Home)), 663.0, 900.0,
        ReadPool::start(2, |_| NothingYet).expect("pool"),
        Reconciles { starts: starts.clone(), checks: checks.clone() }, Some(gate));
    rig.go(Intent::AddProject { project: crate::core::LocalProjectId::from_path(&folder).expect("project") });
    assert_eq!(phases(&mut rig), [ProjectPhase::Unconfirmed]);
    let saved = snapshot(&mut rig).workspace().projects[0].operation.clone().expect("saved operation");
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.repaint();
    let native = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native tree");
    let tree: serde_json::Value = serde_json::from_str(&native).expect("native AccessKit JSON");
    assert!(tree["nodes"].as_object().expect("native nodes").values().any(|node|
        node["aria"]["role"].as_str() == Some("Button") && node["aria"]["label"].as_str() == Some("Check outcome")),
        "recovery is a real native accessible button");
    assert!(rig.said().iter().any(|line| line.contains("No operation receipt is available")));
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let (_, bounds) = targets.iter().find(|(target, _)| target.label == "Check outcome").expect("native recovery bounds");
    assert!(bounds.size.width > gpui::px(0.0) && bounds.size.height > gpui::px(0.0));
    click(&mut rig, "Check outcome");
    assert_eq!(phases(&mut rig), [ProjectPhase::Ready]);
    let settled = snapshot(&mut rig).workspace().projects[0].operation.clone().expect("receipt retained");
    assert!(settled.same_request(&saved));
    assert_eq!(starts.load(Ordering::SeqCst), 1, "recovery never sends a second mutation");
    assert_eq!(checks.load(Ordering::SeqCst), 1, "the exact operation was read");
}

#[gpui::test]
fn retained_check_outcome_callback_cannot_cross_a_same_root_owner_replacement(cx: &mut TestAppContext) {
    let (_, folder) = project("stale-reconcile");
    let starts = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let authority = crate::core::VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(authority, crate::model::ServiceMode::Attached);
    let mut rig = crate::shell::tests::rig_with_engine_gate(cx, Some(Route::Orbit(OrbitRoute::Home)), 663.0, 900.0,
        ReadPool::start(2, |_| NothingYet).expect("pool"),
        Reconciles { starts: starts.clone(), checks: checks.clone() }, Some(gate.clone()));
    rig.go(Intent::AddProject { project: crate::core::LocalProjectId::from_path(&folder).expect("project") });
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let action = targets.iter().find(|(target, _)| target.label == "Check outcome").expect("live recovery").0.act.clone();
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: authority, mode: crate::model::ServiceMode::Attached });
    rig.cx.update(|window, cx| action(window, cx));
    assert_eq!(checks.load(Ordering::SeqCst), 0, "a former native visit cannot use the replacement owner");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(snapshot(&mut rig).workspace().projects[0].phase, ProjectPhase::Unconfirmed);
}

struct ArchivedOperations { starts: Arc<std::sync::Mutex<Vec<crate::model::IndexOperationClaim>>> }
impl EngineClient for ArchivedOperations {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::IndexProject { request, basis, project, operation, .. } => {
                let mut starts = self.starts.lock().expect("fixture starts");
                starts.push(operation.clone());
                let observation = if starts.len() == 1 { crate::model::index_operation::tests::outside(operation) }
                    else { crate::model::index_operation::tests::published(operation) };
                Ok(EngineDto::IndexOperation { request: *request, basis: *basis, project: project.clone(), operation: operation.clone(), observation })
            }
            EngineRequest::IndexOperationStatus { request, basis, project, operation, .. } => {
                Ok(EngineDto::IndexOperation { request: *request, basis: *basis, project: project.clone(), operation: operation.clone(),
                    observation: crate::model::index_operation::tests::outside(operation) })
            }
            other => RootOnly.execute(other),
        }
    }
}

#[gpui::test]
fn native_new_index_after_archival_uses_a_distinct_key_only_on_user_activation(cx: &mut TestAppContext) {
    let (_, folder) = project("archived-operation");
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let authority = crate::core::VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(authority, crate::model::ServiceMode::Attached);
    let mut rig = crate::shell::tests::rig_with_engine_gate(cx, Some(Route::Orbit(OrbitRoute::Home)), 663.0, 900.0,
        ReadPool::start(2, |_| NothingYet).expect("pool"), ArchivedOperations { starts: starts.clone() }, Some(gate));
    rig.go(Intent::AddProject { project: crate::core::LocalProjectId::from_path(&folder).expect("project") });
    assert_eq!(phases(&mut rig), [ProjectPhase::Unconfirmed]);
    assert_eq!(starts.lock().expect("starts").len(), 1, "archival never causes an automatic second mutation");
    let saved = snapshot(&mut rig).workspace().projects[0].operation.clone().expect("consumed key");
    assert!(saved.permits_new_attempt());
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.repaint();
    let native = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native tree");
    let tree: serde_json::Value = serde_json::from_str(&native).expect("AccessKit JSON");
    assert!(tree["nodes"].as_object().expect("native nodes").values().any(|node|
        node["aria"]["role"].as_str() == Some("Button") && node["aria"]["label"].as_str() == Some("Start a new index")));
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let (target, bounds) = targets.iter().find(|(target, _)| target.label == "Start a new index").expect("native new attempt");
    assert!(bounds.size.width > gpui::px(0.0) && bounds.size.height > gpui::px(0.0));
    let stale = target.act.clone();
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(phases(&mut rig), [ProjectPhase::Ready]);
    let started = starts.lock().expect("starts");
    assert_eq!(started.len(), 2);
    assert_ne!(started[0].key, started[1].key, "production OS entropy allocates distinct durable work");
    assert_eq!(started[0].package, started[1].package);
    drop(started);
    rig.cx.update(|window, cx| stale(window, cx));
    rig.settle();
    assert_eq!(starts.lock().expect("starts").len(), 2, "captured consumed-key control cannot act on its replacement claim");
}

#[gpui::test]
fn captured_new_index_control_cannot_cross_same_root_owner_replacement(cx: &mut TestAppContext) {
    let (_, folder) = project("stale-archived-operation");
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let authority = crate::core::VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(authority, crate::model::ServiceMode::Attached);
    let mut rig = crate::shell::tests::rig_with_engine_gate(cx, Some(Route::Orbit(OrbitRoute::Home)), 663.0, 900.0,
        ReadPool::start(2, |_| NothingYet).expect("pool"), ArchivedOperations { starts: starts.clone() }, Some(gate.clone()));
    rig.go(Intent::AddProject { project: crate::core::LocalProjectId::from_path(&folder).expect("project") });
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed();
    let action = targets.iter().find(|(target, _)| target.label == "Start a new index").expect("live action").0.act.clone();
    gate.publish(crate::runtime::owner::OwnerState::Starting);
    gate.publish(crate::runtime::owner::OwnerState::Ready { key: authority, mode: crate::model::ServiceMode::Attached });
    rig.cx.update(|window, cx| action(window, cx));
    rig.settle();
    assert_eq!(starts.lock().expect("starts").len(), 1, "old owner attachment cannot start distinct work under replacement");
    assert_eq!(phases(&mut rig), [ProjectPhase::Unconfirmed]);
}
