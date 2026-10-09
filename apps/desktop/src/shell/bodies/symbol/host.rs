//! The shell's side of the page: a name on the page lights with its subject
//! (the one hover grammar, `facet::hover`), unfurls its page's peek from the
//! float layer, opens its page on a click, and stands in the reader's
//! keyboard walk. The page's filters and folds live in the reader's
//! per-symbol disclosure, so they survive a repaint.

use crate::model::pages::{
    DocFragment, DocSections, PageKey, SearchQuery, Stamp, SymbolPage, SymbolRef,
};
use crate::navigation::Intent;
use crate::shell::focus::{Act as Action, Target, TargetAction, Targets};
use crate::shell::kit::symbol_route;
use crate::shell::reader::{Reader, SymbolDisclosure};
use crate::shell::region::Links;
use facet::anatomy::page::{Door, Doors, Fold};
use facet::anatomy::symbol::key::{FoldKey, Key, Sec};
use facet::anatomy::symbol::{Act, Change, Host, Spots, Ui};
use facet::hover::Subject;
use gpui::{AnyElement, IntoElement, ScrollHandle, SharedString, WeakEntity};
use std::cell::RefCell;
use std::rc::Rc;

/// The host of one symbol page.
pub(super) struct ShellHost<'a> {
    pub package: String,
    pub symbol: SymbolRef,
    pub links: Links,
    pub targets: &'a Targets,
    pub active: bool,
    /// Earlier exact content keeps its layout without navigation or peeks.
    pub read_only: bool,
    /// The declaration this page was reached from (a hop forward), ringed.
    pub from: Option<SymbolRef>,
    pub disclosure: SymbolDisclosure,
    pub reader: WeakEntity<Reader>,
    pub scroll: ScrollHandle,
    pub said: RefCell<Vec<SharedString>>,
    pub docs: DocLinks,
    pub dependency: (PageKey, Stamp),
    pub admit: facet::controls::button::ActivationAdmission,
}

impl ShellHost<'_> {
    /// All page callbacks share the exact painted Reader visit and read receipt.
    fn action(&self, action: Act) -> Act {
        let admit = self.admit.clone();
        let active = self.active;
        Rc::new(move |window, cx| {
            if active && admit(cx) {
                action(window, cx);
            }
        })
    }

    fn navigate(&self, intent: Intent) -> Act {
        let links = self.links.clone();
        let dependency = self.dependency.clone();
        self.action(Rc::new(move |_, cx| {
            links.dispatch_read(intent.clone(), dependency.clone(), cx)
        }))
    }

    /// What the page said, in order.
    pub(super) fn take_said(&self) -> Vec<SharedString> {
        std::mem::take(&mut *self.said.borrow_mut())
    }
}

impl Doors for ShellHost<'_> {
    fn door(&self, link: &str) -> Option<Door> {
        if self.read_only { return None; }
        let target = SymbolRef::new(link).ok()?;
        // A door to the page you are on is no door (and must not carry this
        // page's title key a second time).
        if target == self.symbol {
            return None;
        }
        let route = symbol_route(&self.package, &target)?;
        let from = self.from.as_ref() == Some(&target);
        let store = self.links.store.clone();
        let label = SharedString::from(target.identity().name().to_owned());
        let key = PageKey::Symbol(target);
        let peek_key = key.clone();
        Some(Door {
            subject: Subject::new(link.to_owned()),
            peek: Some(Rc::new(move |bounds| {
                crate::shell::peeks::request(peek_key.clone(), label.clone(), bounds, store.clone())
            })),
            open: Some(self.navigate(Intent::Navigate(route))),
            from,
        })
    }

    fn fold(&self, _: &'static str) -> Option<Fold> {
        None
    }

    fn track(
        &self,
        key: SharedString,
        label: SharedString,
        door: Option<&Door>,
        element: AnyElement,
    ) -> AnyElement {
        if self.active && let Some(act) = door.and_then(|door| door.open.clone()) {
            let target = door.and_then(|door| SymbolRef::new(door.subject.0.as_ref()).ok());
            self.targets.push(Target {
                id: key.clone(),
                label,
                action: TargetAction::new(self.admit.clone(), act),
                peek: target.clone().map(PageKey::Symbol),
                source: target,
            });
        }
        if self.active && door.is_some_and(|door| door.open.is_some()) {
            self.targets.track(key, element).into_any_element()
        } else {
            self.targets.measure(key, element).into_any_element()
        }
    }

    fn track_hoverable(
        &self,
        key: SharedString,
        label: SharedString,
        door: &Door,
        focus: facet::hover::FocusTarget,
        element: AnyElement,
    ) -> AnyElement {
        if self.active && let Some(act) = door.open.clone() {
            let target = SymbolRef::new(focus.subject().0.as_ref()).ok();
            self.targets.push(Target {
                id: key.clone(),
                label,
                action: TargetAction::new(self.admit.clone(), act),
                peek: target.clone().map(PageKey::Symbol),
                source: target,
            });
        }
        if self.active && door.open.is_some() {
            self.targets.track(key, element).into_any_element()
        } else {
            self.targets.measure(key, element).into_any_element()
        }
    }

    fn up(&self) -> Option<Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>> {
        if self.read_only { return None; }
        Some(self.navigate(Intent::ZoomOut))
    }

    fn mark(&self, link: &str) -> Option<gpui::ElementId> {
        SymbolRef::new(link)
            .ok()
            .map(|symbol| crate::shell::kit::shared_id(&symbol))
    }

    fn say(&self, text: &str) {
        self.said
            .borrow_mut()
            .push(SharedString::from(text.to_owned()));
    }
}

impl Host for ShellHost<'_> {
    fn ui(&self) -> Ui {
        self.disclosure.ui.clone()
    }

    fn change(&self, change: Change) -> Act {
        let (reader, symbol) = (self.reader.clone(), self.symbol.clone());
        self.action(Rc::new(move |_, cx| {
            let _ = reader.update(cx, |reader, cx| {
                reader.change_symbol(symbol.clone(), &change, cx)
            });
        }))
    }

    fn open_source(&self, path: &str, line: u32) -> Act {
        self.navigate(Intent::OpenSource {
            path: std::sync::Arc::from(path),
            line,
        })
    }

    fn unfold(&self, key: &FoldKey) -> Option<Fold> {
        let fold = key.clone();
        let (reader, symbol) = (self.reader.clone(), self.symbol.clone());
        let toggle_fold = fold.clone();
        Some(Fold {
            open: self.disclosure.is_open(&fold),
            presence: self.disclosure.unroll(fold),
            toggle: self.action(Rc::new(move |_, cx| {
                let _ = reader.update(cx, |reader, cx| {
                    reader.toggle_symbol(symbol.clone(), toggle_fold.clone(), cx)
                });
            })),
        })
    }

    fn link_admission(&self) -> Option<facet::controls::button::ActivationAdmission> {
        Some(self.admit.clone())
    }

    fn lookup(&self, target: &str) -> Option<Act> {
        if self.read_only { return None; }
        match self.docs.resolve(target)? {
            DocDestination::Declaration(symbol) => {
                Some(self.navigate(Intent::Navigate(symbol_route(&self.package, &symbol)?)))
            }
            DocDestination::External(uri) => {
                Some(self.action(Rc::new(move |_, cx| cx.open_url(&uri.0))))
            }
            DocDestination::Query(query) => Some(self.navigate(Intent::Navigate(
                crate::navigation::Route::Orbit(crate::navigation::OrbitRoute::Browse(
                    crate::navigation::BrowseRoute::Find(query),
                )),
            ))),
        }
    }

    fn target(&self, key: &Key, label: SharedString, act: Act, element: AnyElement) -> AnyElement {
        let id = key.text();
        if self.active {
            self.targets.push(Target {
                id: id.clone(),
                label,
                action: TargetAction::new(self.admit.clone(), act),
                peek: None,
                source: None,
            });
        }
        gpui::IntoElement::into_any_element(self.targets.track(id, element))
    }

    fn target_control(
        &self,
        key: &Key,
        label: SharedString,
        act: Act,
        activation: facet::anatomy::symbol::docs::Activation,
        control: gpui::Stateful<gpui::Div>,
    ) -> AnyElement {
        let id = key.text();
        let action = TargetAction::new(self.admit.clone(), act);
        let act = action.callback();
        let handle = self
            .active
            .then(|| self.targets.reuse_native_handle(&id))
            .flatten();
        if self.active {
            self.targets.push(Target {
                id: id.clone(),
                label,
                action,
                peek: None,
                source: None,
            });
        }
        gpui::IntoElement::into_any_element(NativeTargetControl {
            id,
            control,
            targets: self.targets.clone(),
            handle,
            act,
            activation,
            admit: self.admit.clone(),
            active: self.active,
        })
    }

    fn reveal(&self, section: Sec) -> Act {
        let (spots, scroll) = (self.spots(), self.scroll.clone());
        self.action(Rc::new(move |_, _| {
            if let Some(bounds) = spots.get(section) {
                let offset = scroll.offset();
                let delta = bounds.top() - scroll.bounds().top() - gpui::px(20.0);
                scroll.set_offset(gpui::point(offset.x, offset.y - delta));
            }
        }))
    }

    fn spots(&self) -> Rc<Spots> {
        Rc::clone(&self.disclosure.spots)
    }

    fn flow(&self) -> facet::motion::Flow {
        // A page drawn away from the scroller (leaving, folding, under a
        // plate) is inert: it stands where it was. A fresh flow places each
        // part where it lays out, so a still copy neither glides (a moving
        // part is drawn above the page, out of the fold's reach) nor moves
        // the live page's springs.
        if self.active {
            self.disclosure.flow.clone()
        } else {
            facet::motion::Flow::new("s6-inert")
        }
    }
}

/// The semantic Div supplied by FACET remains the native control; this
/// adapter adds its existing Reader focus owner at normal GPUI construction.
#[derive(gpui::IntoElement)]
struct NativeTargetControl {
    id: SharedString,
    control: gpui::Stateful<gpui::Div>,
    targets: Targets,
    handle: Option<gpui::FocusHandle>,
    act: Act,
    activation: facet::anatomy::symbol::docs::Activation,
    admit: facet::controls::button::ActivationAdmission,
    active: bool,
}

impl gpui::RenderOnce for NativeTargetControl {
    fn render(self, _: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        use gpui::{IntoElement as _, StatefulInteractiveElement as _};
        let control: gpui::AnyElement = if self.active {
            let handle = self
                .handle
                .unwrap_or_else(|| self.targets.native_handle(&self.id, cx));
            let control =
                facet::controls::button::capture_activation_admission(self.control, self.admit);
            let activation = self.activation;
            let control = activation.pointer_down(control, self.act.clone());
            let act = self.act;
            facet::controls::button::native_button_with_event(
                control,
                &handle,
                move |event, window, cx| {
                    if activation.admits_click(event) {
                        act(window, cx);
                    }
                },
            ).into_any_element()
        } else {
            gpui::inert(
                format!("inert-symbol-control-{}", self.id),
                "This retained declaration control does not own input",
                self.control,
            ).into_any_element()
        };
        self.targets.track(self.id, control)
    }
}

/// Targets from producer links are exact coordinates, including opaque semantic
/// coordinates. Display labels and source locations cannot disambiguate them.
#[derive(Default)]
pub(super) struct DocLinks(std::collections::BTreeSet<SymbolRef>);

enum DocDestination {
    Declaration(SymbolRef),
    External(ExternalUri),
    Query(SearchQuery),
}

/// Uses the same spelling admission as the README model; arbitrary schemes
/// and malformed addresses cannot reach the platform opener.
struct ExternalUri(String);

impl ExternalUri {
    fn parse(target: &str) -> Option<Self> {
        crate::model::local_package::readme_external_address(target).map(|uri| Self(uri.to_owned()))
    }
}

impl DocLinks {
    pub(super) fn of(page: &SymbolPage) -> Self {
        let mut links = Self::default();
        links.fragments(&page.docs);
        links.sections(&page.sections);
        if let Some(members) = page.members.known() {
            for member in members.all() {
                links.fragments(&member.docs);
                links.sections(&member.sections);
            }
        }
        links
    }

    fn fragments(&mut self, fragments: &[DocFragment]) {
        self.0
            .extend(fragments.iter().filter_map(|fragment| match fragment {
                DocFragment::Link { coordinate, .. } => coordinate.clone(),
                _ => None,
            }));
    }

    fn sections(&mut self, sections: &DocSections) {
        self.fragments(&sections.lead);
        for section in sections.sections.iter() {
            self.fragments(&section.body);
            for entry in section.entries.iter() {
                self.fragments(&entry.body);
            }
        }
    }

    fn resolve(&self, target: &str) -> Option<DocDestination> {
        let candidate = SymbolRef::new(target).ok()?;
        // A displayed href contains the coordinate spelling, while the
        // producer value may also carry an alternate-release origin receipt.
        // Return that original value rather than reparsing away its receipt.
        let mut bindings = self.0.iter().filter(|symbol| symbol.as_str() == target);
        if let Some(symbol) = bindings.next() {
            // A rendered href carries no release-origin receipt. Conflicting
            // producer bindings cannot be disambiguated by their shared text.
            if bindings.any(|other| other != symbol) {
                return None;
            }
            return Some(DocDestination::Declaration(symbol.clone()));
        }
        if let Some(uri) = ExternalUri::parse(target) {
            return Some(DocDestination::External(uri));
        }
        if target.contains("://") || target.to_ascii_lowercase().starts_with("mailto:") {
            return None;
        }
        let identity = candidate.identity();
        // The existing display parser classifies bare words as package
        // spellings too. Neither shape grants declaration authority here.
        if !matches!(
            identity.shape(),
            backend_present::IdentityShape::Package | backend_present::IdentityShape::Opaque
        ) || target.starts_with('#')
        {
            return None;
        }
        SearchQuery::new(target, 50).ok().map(DocDestination::Query)
    }
}

#[cfg(test)]
mod doc_link_tests {
    use super::*;
    use std::sync::Arc;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn link(target: &SymbolRef) -> DocFragment {
        DocFragment::Link {
            label: Arc::from("advance_signal"),
            target: crate::model::pages::RowKey::from(backend_library::symbol_key(target.as_str())),
            coordinate: Some(target.clone()),
        }
    }

    #[test]
    fn producer_target_disambiguates_same_named_function_and_reexport_without_a_path() -> TestResult {
        let function = SymbolRef::new("/abs/project::semantic::00ff::advance_signal")?;
        let reexport = SymbolRef::new("/abs/project::semantic::11ff::advance_signal")?;
        assert!(function.identity().path().is_none());
        let mut links = DocLinks::default();
        links.fragments(&[link(&function), link(&reexport)]);
        for exact in [&function, &reexport] {
            let Some(DocDestination::Declaration(actual)) = links.resolve(exact.as_str()) else {
                panic!("producer coordinate must remain semantic")
            };
            assert_eq!(&actual, exact);
            let route = symbol_route("/abs/project", &actual).ok_or("producer declaration route")?;
            let crate::navigation::Route::Symbol(route) = route else {
                panic!("declaration route")
            };
            assert_eq!(route.id.as_str(), exact.as_str());
            assert_eq!(route.package.as_str(), "/abs/project");
        }
        Ok(())
    }

    #[test]
    fn old_or_unresolved_coordinates_are_not_text_searches() -> TestResult {
        let old = SymbolRef::new("/abs/project::semantic::00ff::advance_signal")?;
        let current = SymbolRef::new("/abs/project::semantic::22ff::advance_signal")?;
        let mut links = DocLinks::default();
        links.fragments(&[link(&current)]);
        assert!(links.resolve(old.as_str()).is_none());
        assert!(
            links
                .resolve("/abs/project::src/lib.rs:4::advance_signal")
                .is_none()
        );
        assert!(matches!(
            links.resolve("advance_signal"),
            Some(DocDestination::Query(_))
        ));
        assert!(matches!(
            links.resolve("https://example.org/doc"),
            Some(DocDestination::External(_))
        ));
        assert!(matches!(
            links.resolve("mailto:docs@example.org"),
            Some(DocDestination::External(_))
        ));
        assert!(links.resolve("#local").is_none());
        for invalid in [
            "https://",
            "https://user@example.org",
            "https://example.org/%0A",
            "https://example.org/has space",
            "file:///tmp/file",
        ] {
            assert!(
                links.resolve(invalid).is_none(),
                "malformed/unsupported external URI: {invalid}"
            );
        }
        assert!(ExternalUri::parse("javascript:alert(1)").is_none());
        Ok(())
    }

    #[test]
    fn displayed_coordinate_keeps_the_producers_alternate_release_receipt() -> TestResult {
        let pinned = crate::model::pages::PackageRef::parse("pkg:cargo/demo@1.0.0")?;
        let alternate = crate::model::pages::PackageRef::parse("pkg:cargo/demo@2.0.0")?
            .with_release_origin(&pinned);
        let symbol =
            SymbolRef::new("pkg:cargo/demo@1.0.0::semantic::00ff::advance_signal")?;
        let target = symbol.rebased(&pinned, &alternate).ok_or("alternate-release symbol receipt")?;
        let mut links = DocLinks::default();
        links.fragments(&[link(&target)]);
        let Some(DocDestination::Declaration(actual)) = links.resolve(target.as_str()) else {
            panic!("exact producer coordinate")
        };
        assert_eq!(actual, target);
        assert_eq!(actual.release_origin(), Some(pinned.as_str()));
        Ok(())
    }

    #[test]
    fn identical_display_coordinates_with_conflicting_release_receipts_are_inert() -> TestResult {
        let package = crate::model::pages::PackageRef::parse("pkg:cargo/demo@2.0.0")?;
        let first_pin = crate::model::pages::PackageRef::parse("pkg:cargo/demo@1.0.0")?;
        let second_pin = crate::model::pages::PackageRef::parse("pkg:cargo/demo@1.1.0")?;
        let coordinate =
            SymbolRef::new("pkg:cargo/demo@2.0.0::semantic::00ff::advance_signal")?;
        let first = coordinate
            .rebased(&package, &package.clone().with_release_origin(&first_pin))
            .ok_or("first release-origin receipt")?;
        let second = coordinate
            .rebased(&package, &package.clone().with_release_origin(&second_pin))
            .ok_or("second release-origin receipt")?;
        assert_eq!(first.as_str(), second.as_str());
        assert_ne!(
            first, second,
            "the producer receipts carry different authority"
        );
        let mut links = DocLinks::default();
        links.fragments(&[link(&first), link(&first)]);
        let Some(DocDestination::Declaration(actual)) = links.resolve(first.as_str()) else {
            panic!("repeated equal receipt remains navigable")
        };
        assert_eq!(actual, first);
        links.fragments(&[link(&second)]);
        assert!(
            links.resolve(first.as_str()).is_none(),
            "no arbitrary first receipt and no text-query fallback"
        );
        let mut reversed = DocLinks::default();
        reversed.fragments(&[link(&second), link(&first)]);
        assert!(
            reversed.resolve(first.as_str()).is_none(),
            "producer order cannot choose authority"
        );
        Ok(())
    }

    #[gpui::test]
    fn a_mounted_symbol_door_rejects_a_callback_from_the_previous_reader_visit(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = check_mounted_symbol_door_rejects_previous_reader_visit(cx);
        assert!(result.is_ok(), "mounted symbol door fixture failed: {result:?}");
    }

    fn check_mounted_symbol_door_rejects_previous_reader_visit(
        cx: &mut gpui::TestAppContext,
    ) -> TestResult {
        let pool = crate::runtime::reads::ReadPool::start(2, |_| super::super::page_tests::Pinned)?;
        let mut rig = crate::shell::tests::rig_with_reads(
            cx,
            Some(super::super::page_tests::route("de.rs", 2709, "from_str")),
            1440.0,
            900.0,
            pool,
        );
        rig.settle();
        rig.repaint();
        let targets = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        let action = targets
            .placed()
            .into_iter()
            .find_map(|(target, _)| {
                target
                    .source
                    .as_ref()
                    .filter(|symbol| symbol.identity().name() == "from_slice")
                    .map(|_| target.action.callback())
            })
            .expect("the mounted producer sibling has a real ShellHost door");
        rig.cx.update(|window, cx| action(window, cx));
        rig.settle();
        let crate::navigation::Route::Symbol(route) = rig.route() else {
            panic!("door opens a symbol page")
        };
        assert!(route.id.as_str().ends_with("::from_slice"));
        let later = super::super::page_tests::route("value/mod.rs", 116, "Value");
        rig.go(Intent::Navigate(later.clone()));
        rig.settle();
        rig.cx.update(|window, cx| action(window, cx));
        rig.settle();
        assert_eq!(
            rig.route(),
            later,
            "a stale mounted door cannot replace the later Reader visit"
        );
        Ok(())
    }
}
