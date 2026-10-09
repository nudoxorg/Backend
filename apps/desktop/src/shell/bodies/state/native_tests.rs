//! Controlled native projection of the closed FastAPI Cargo-scope fault.
//! The fixture address replaces the user's path; the producer's reason and
//! diagnostic code stay exact. This is not a current compiler acceptance run.

use super::*;
use crate::core::LocalProjectId;
use crate::model::browse::BrowseKey;
use crate::model::pages::{PageValue, ReadFailure};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::{fit_tests, tests};
use gpui::{Modifiers, TestAppContext, size};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

const FAULT: &str = "command failed: invalid query: requested project /fixture/fastapi-full-stack is outside Cargo scope: its exact directory has no recognized Cargo package or workspace manifest; ancestor workspaces are not used for project-tree requests";

struct FailedPage { reads: Arc<AtomicUsize>, unavailable: bool, diagnostic: Option<String> }
impl PageReader for FailedPage {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if !matches!(request, ReadRequest::Browse(BrowseKey::Tree(_))) { return tests::Fixture.read(request, context); }
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(detail) = &self.diagnostic { return Err(ReadFailure::Fault(ErrorValue::with_diagnostic(FaultCode::Transport, "The index could not start. ", detail))); }
        if self.unavailable { Err(ReadFailure::Unavailable(UnavailableReason::OutOfScope, Arc::from("controlled scope refusal"))) }
        else { Err(ReadFailure::Fault(ErrorValue::new(FaultCode::Protocol, FAULT))) }
    }
}

#[gpui::test]
fn native_terminal_page_exposes_exact_fault_words_and_explicit_retry_only(cx: &mut TestAppContext) {
    for unavailable in [false, true] {
        let reads = Arc::new(AtomicUsize::new(0));
        let observations = Arc::clone(&reads);
        let pool = ReadPool::start(1, move |_| FailedPage { reads: Arc::clone(&observations), unavailable, diagnostic: None }).expect("fault read pool");
        let project = LocalProjectId::new("/fixture/fastapi-full-stack").expect("exact fixture address");
        let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project)));
        let mut rig = tests::rig_with_reads(cx, Some(route), 1440.0, 2400.0, pool);
        rig.settle();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        for (width, percent) in [(1440.0, 100), (720.0, 200)] {
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            rig.cx.simulate_resize(size(px(width), px(2400.0)));
            rig.settle();
            let heading = if unavailable { "Library" } else { "Library could not be read." };
            let words = if unavailable { "This page is outside the selected scope." } else { FAULT };
            for (role, label) in [("Heading", heading), ("Label", words)] {
                let bounds = tests::native_bounds(&mut rig, role, label, false).expect("actual native failure words");
                assert!(bounds.left() >= px(0.0) && bounds.right() <= px(width + 0.5), "native failure words leave the window: {bounds:?}");
                assert!(fit_tests::painted(&mut rig).texts.iter().any(|text| text.content == label), "native words equal painted words");
            }
            if !unavailable { assert!(tests::native_bounds(&mut rig, "Label", "READ-PROTOCOL", false).is_some()); }
            let before = reads.load(Ordering::SeqCst);
            for _ in 0..3 { rig.frame(700); }
            assert_eq!(reads.load(Ordering::SeqCst), before, "a terminal page never loops its failed read");
            let retry = tests::native_bounds(&mut rig, "Button", "Try again", true);
            if unavailable { assert!(retry.is_none()); }
            else {
                rig.cx.simulate_click(retry.expect("native explicit Retry").center(), Modifiers::none());
                rig.settle();
                assert_eq!(reads.load(Ordering::SeqCst), before + 1, "native Retry renews exactly one current request");
            }
        }
    }
}


#[gpui::test]
fn native_owner_fault_copies_full_diagnostic_without_renewing_failed_read(cx: &mut TestAppContext) {
    let terminal = "caused by: publication journal root cause 日本語 sentinel";
    let detail = format!("open compiler owner: {}\n{terminal}\0", "intermediate 原因\t".repeat(90));
    let expected = ErrorValue::with_diagnostic(FaultCode::Transport, "The index could not start. ", &detail);
    let reads = Arc::new(AtomicUsize::new(0));
    let observations = Arc::clone(&reads);
    let pool = ReadPool::start(1, move |_| FailedPage { reads: Arc::clone(&observations), unavailable: false, diagnostic: Some(detail.clone()) }).expect("controlled fault pool");
    let project = LocalProjectId::new("/fixture/owner-diagnostic").expect("fixture route");
    let route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project)));
    let mut rig = tests::rig_with_reads(cx, Some(route), 1440.0, 2400.0, pool);
    rig.settle();
    for (width, percent) in [(1440.0, 100), (720.0, 200)] {
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        rig.go(Intent::ZoomTo { display, percent });
        rig.cx.simulate_resize(size(px(width), px(2400.0)));
        rig.settle();
        let summary = tests::native_bounds(&mut rig, "Label", expected.message(), false).expect("native concise fault summary");
        assert!(summary.left() >= px(0.0) && summary.right() <= px(width + 0.5));
        assert!(expected.message().contains(terminal));
        let before = reads.load(Ordering::SeqCst);
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("before-diagnostic-copy".into()));
        let copy = tests::native_bounds(&mut rig, "Button", "Copy diagnostic", true).expect("native explicit diagnostic copy");
        rig.cx.simulate_click(copy.center(), Modifiers::none());
        rig.settle();
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()), expected.diagnostic_detail().map(str::to_owned));
        assert_eq!(reads.load(Ordering::SeqCst), before, "copying evidence cannot renew the failed read");
    }
}
