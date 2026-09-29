//! The simple symbol page in the gallery, built from the design board's own
//! page models (`Nudox-Design-System/v6/moments/data/page6/<id>.json`): the
//! same seven pages, so a capture sits beside the board's still. The facts
//! are read from the board's raw fields (declaration, docs, members, use
//! sites); everything the page says is derived from them by
//! [`derive`](super::derive), never copied from the board's own derivation.
//!
//! State is the gallery's own: a click on a chip, a fold or a package changes
//! it, so a script can open any state before a capture.

use super::derive::uses::{Reader, Rel, Site, read_all};
use super::facts::{Facts, Member, Owes, Receives, Section, SectionKind, Site as Source};
use super::host::{Act, Change, Host, Spots, Ui};
use super::key::{FoldKey, Key, Sec};
use super::kit::{Env, ink, roles, said};
use super::layout::Layout;
use super::view::{Block, Do, Kind, Lang, Uses, View};
use super::{Chrome, compile, page, with_uses};
use crate::anatomy::page::{Door, Doors, Fold, Still};
use crate::gallery::Scene;
use crate::motion::presence::Presence;
use crate::overlay::float;
use crate::theme::ActiveFacet;
use gpui::{AnyElement, AnyView, App, AppContext, Context, EntityId, InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

const READER: (u32, u32) = (1176, 1500);

pub(crate) const SCENES: &[Scene] = &[
    Scene { id: "sym6-from-str", title: "serde_json::from_str: a generic that can fail (board: symbol6 rs-from_str)", size: READER, build: |_, cx| scene("rs-from_str", &[], cx) },
    Scene { id: "sym6-value", title: "serde_json::Value: an enum that nests (board: symbol6 rs-Value)", size: READER, build: |_, cx| scene("rs-Value", &[], cx) },
    Scene { id: "sym6-as-str", title: "Value::as_str: a method that may give nothing (board: symbol6 rs-as_str)", size: READER, build: |_, cx| scene("rs-as_str", &[], cx) },
    Scene { id: "sym6-alloc", title: "AllocationInfo: a struct with public fields (board: symbol6 rs-AllocationInfo)", size: READER, build: |_, cx| scene("rs-AllocationInfo", &[], cx) },
    Scene { id: "sym6-serialize", title: "serde::Serialize: a trait (board: symbol6 rs-Serialize)", size: READER, build: |_, cx| scene("rs-Serialize", &[], cx) },
    Scene { id: "sym6-py-match", title: "re.match: Python with no type hints (board: symbol6 py-re.match)", size: READER, build: |_, cx| scene("py-re.match", &[], cx) },
    Scene { id: "sym6-js-which", title: "which.sync: JavaScript, an options object, unfolded (board: symbol6 js-which.sync &opts=opt)", size: READER, build: |_, cx| scene("js-which.sync", &[FoldKey::Options("opt".to_owned())], cx) },
];

fn board_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v6/moments/data/page6")
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn lang(name: &str) -> Lang {
    Lang::from_name(name)
}

fn kind_of(v: &Value) -> Kind {
    match text(v, "kind").as_str() {
        "function" | "fn" => Kind::Function,
        "method" => Kind::Method,
        "enum" => Kind::Enum,
        "struct" => Kind::Struct,
        "trait" => Kind::Trait,
        _ => Kind::Other,
    }
}

fn receives(how: &str) -> Receives {
    match how {
        "reads" => Receives::Reads,
        "changes" => Receives::Changes,
        "uses up" | "consumes" => Receives::UsesUp,
        _ => Receives::Makes,
    }
}

fn ty_text(v: Option<&Value>) -> String {
    v.and_then(|v| v.get("text")).and_then(Value::as_str).unwrap_or("()").to_owned()
}

/// A method's signature as the source would write it, from the board's
/// derived model (the board keeps no raw text for methods).
fn method_signature(name: &str, sig: &Value) -> String {
    let recv = match sig.get("recv").and_then(|r| r.get("how")).and_then(Value::as_str) {
        Some("reads") => "&self",
        Some("changes") => "&mut self",
        Some("uses up") => "self",
        _ => "",
    };
    let mut params: Vec<String> = Vec::new();
    if !recv.is_empty() {
        params.push(recv.to_owned());
    }
    for input in sig.get("ins").and_then(Value::as_array).into_iter().flatten() {
        params.push(format!("{}: {}", text(input, "name"), ty_text(input.get("type"))));
    }
    let out = sig.get("out");
    let mut ret = ty_text(out.and_then(|o| o.get("type")));
    if out.is_some_and(|o| o.get("none").is_some()) {
        ret = format!("Option<{ret}>");
    }
    if out.is_some_and(|o| o.get("fails").is_some()) {
        ret = format!("Result<{ret}>");
    }
    if ret == "()" {
        format!("pub fn {name}({})", params.join(", "))
    } else {
        format!("pub fn {name}({}) -> {ret}", params.join(", "))
    }
}

fn from_type(word: &str) -> &'static str {
    match word {
        "a number" => "f64",
        "yes or no" => "bool",
        "text" => "&str",
        "a list" | "a list of Value" => "Vec<Value>",
        "a map of text to Value" => "Map<String, Value>",
        _ => "Value",
    }
}

fn blocks(v: &Value) -> Vec<Block> {
    v.get("docs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|b| {
            let t = text(b, "t");
            match text(b, "k").as_str() {
                "h" => Block::Head(t),
                "li" => Block::Item(t),
                "code" => Block::Code(t),
                _ => Block::Para(t),
            }
        })
        .collect()
}

/// The facts a board page gives, from its raw fields.
fn facts_of(v: &Value) -> Facts {
    let lang = lang(&text(v, "lang"));
    let mut facts = Facts::new(&text(v, "name"), kind_of(v), lang, &text(v, "pkg"));
    facts.version = Some(text(v, "version"));
    facts.path = v.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect()).unwrap_or_default();
    facts.signature = Some(text(v, "decl"));
    facts.docs = blocks(v);
    if let Some(source) = v.get("source") {
        facts.site = Some(Source { file: text(source, "file"), line: u32::try_from(source.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1), open: Some(format!("/board/{}", text(source, "file"))) });
    }
    if facts.kind == Kind::Method {
        facts.owner = facts.path.last().cloned();
    }
    facts.links = vec![("Error".to_owned(), "addr:Error".to_owned()), ("Value".to_owned(), "addr:Value".to_owned())];
    if let Some(shape) = v.get("shape") {
        for case in shape.get("cases").and_then(Value::as_array).into_iter().flatten() {
            let ty = case.get("type").filter(|t| !t.is_null());
            let signature = match ty {
                Some(t) => format!("{}({})", text(case, "name"), ty_text(Some(t))),
                None => text(case, "name"),
            };
            facts.made_of.push(Member { name: text(case, "name"), signature: Some(signature), summary: Some(text(case, "doc")), more: Some(text(case, "more")).filter(|m| !m.is_empty()), link: Some(format!("addr:{}", text(case, "name"))), ..Member::default() });
        }
        for field in shape.get("fields").and_then(Value::as_array).into_iter().flatten() {
            facts.made_of.push(Member { name: text(field, "name"), signature: Some(format!("pub {}: {}", text(field, "name"), ty_text(field.get("type")))), summary: Some(text(field, "doc")), more: Some(text(field, "more")).filter(|m| !m.is_empty()), ..Member::default() });
        }
    }
    for group in v.get("groups").and_then(Value::as_array).into_iter().flatten() {
        for item in group.get("items").and_then(Value::as_array).into_iter().flatten() {
            let name = text(item, "name");
            if let Some(froms) = item.get("froms").and_then(Value::as_array) {
                for word in froms.iter().filter_map(Value::as_str) {
                    facts.does.push(Member { name: "from".to_owned(), signature: Some(format!("fn from(val: {}) -> Value", from_type(word))), receives: Receives::Makes, ..Member::default() });
                }
                continue;
            }
            if name == "parse" {
                facts.does.push(Member { name: "from_str".to_owned(), signature: Some("fn from_str(s: &str) -> Result<Value, Self::Err>".to_owned()), receives: Receives::Makes, ..Member::default() });
                continue;
            }
            let Some(sig) = item.get("sig") else {
                facts.does.push(Member { name, summary: Some(text(item, "doc")), receives: Receives::Makes, signature: Some("fn default() -> Self".to_owned()), ..Member::default() });
                continue;
            };
            let how = sig.get("recv").and_then(|r| r.get("how")).and_then(Value::as_str).unwrap_or("");
            facts.does.push(Member { signature: Some(method_signature(&name, sig)), summary: Some(text(item, "doc")), receives: receives(how), link: Some(format!("addr:{name}")), name, ..Member::default() });
        }
    }
    if let Some(required) = v.get("required").and_then(Value::as_array) {
        for item in required {
            facts.does.push(Member {
                name: text(item, "name"),
                signature: Some("fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;".to_owned()),
                summary: Some(text(item, "doc")),
                owes: Owes::Required,
                ..Member::default()
            });
        }
    }
    facts.implementors = v.get("implementors").and_then(|i| i.get("total")).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    for item in v.get("implements").and_then(Value::as_array).into_iter().flatten() {
        facts.implements.push((text(item, "name"), text(item, "how") == "derive"));
    }
    for sibling in v.get("siblings").and_then(Value::as_array).into_iter().flatten() {
        let kind = match text(sibling, "kind").as_str() {
            "fn" => Kind::Function,
            "method" => Kind::Method,
            "struct" => Kind::Struct,
            "trait" => Kind::Trait,
            "enum" => Kind::Enum,
            _ => Kind::Other,
        };
        facts.beside.push(super::facts::Beside { name: text(sibling, "name"), kind, signature: None, link: Some(format!("addr:{}", text(sibling, "name"))) });
    }
    if let Some(releases) = v.get("history").and_then(|h| h.get("releases")).and_then(Value::as_array) {
        let last = releases.last().and_then(|r| r.get("sig")).cloned();
        facts.releases = releases.iter().map(|r| (text(r, "v"), r.get("sig") != last.as_ref())).collect();
    }
    match text(v, "id").as_str() {
        "rs-from_str" => {
            facts.error_kinds = v
                .get("sig")
                .and_then(|s| s.get("out"))
                .and_then(|o| o.get("fails"))
                .and_then(|f| f.get("kinds"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|k| (text(k, "name"), text(k, "doc")))
                .collect();
            facts.error_tells = Some("Error::classify() → Category".to_owned());
        }
        "py-re.match" => {
            facts.docs = vec![Block::Para(text(v, "summary"))];
            facts.sections = vec![
                Section { kind: SectionKind::Parameters, body: String::new(), entries: vec![("pattern".to_owned(), "a regular expression, text or compiled".to_owned()), ("string".to_owned(), "what to look at".to_owned())] },
                Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("re.error".to_owned(), "if the pattern itself is invalid".to_owned())] },
            ];
        }
        "js-which.sync" => {
            facts.signature = Some("const whichSync = (cmd, opt) => {".to_owned());
            facts.sections = vec![
                Section {
                    kind: SectionKind::Parameters,
                    body: String::new(),
                    entries: vec![
                        ("cmd".to_owned(), "{string} a program name".to_owned()),
                        ("opt".to_owned(), "{object} options".to_owned()),
                        ("opt.nothrow".to_owned(), "{boolean} give null instead of throwing".to_owned()),
                        ("opt.all".to_owned(), "{boolean} give every match, a list".to_owned()),
                        ("opt.path".to_owned(), "{string} search this instead of $PATH".to_owned()),
                        ("opt.pathExt".to_owned(), "{string} extensions to try (Windows)".to_owned()),
                    ],
                },
                Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("Error".to_owned(), "if it isn't on PATH, unless nothrow".to_owned())] },
            ];
        }
        _ => {}
    }
    facts
}

fn sites_of(v: &Value) -> Vec<Site> {
    let page_kind = kind_of(v);
    v.get("uses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|u| {
            let tag = text(u, "tag");
            let rel = match tag.as_str() {
                "imports" => Rel::Imports,
                _ if matches!(page_kind, Kind::Function | Kind::Method) => Rel::Calls,
                _ => Rel::TypeReference,
            };
            let dir = text(u, "dir");
            let file = text(u, "file");
            Site {
                package: text(u, "pkg"),
                path: if dir.is_empty() { file.clone() } else { format!("/work/{dir}/{file}") },
                file,
                line: u32::try_from(u.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1),
                text: text(u, "text"),
                rel,
                exact: u.get("approx").is_none(),
            }
        })
        .collect()
}

fn reader_of(view: &View) -> Reader {
    let mut members = Vec::new();
    let mut cases = Vec::new();
    for group in &view.verbs {
        for row in &group.rows {
            members.push((row.name.clone(), group.verb));
        }
    }
    if let Some(super::view::Shape::OneOf(shape)) = &view.shape {
        cases.extend(shape.iter().map(|c| c.name.clone()));
    }
    if let Some(super::view::Shape::Holds { fields, .. }) = &view.shape {
        members.extend(fields.iter().map(|f| (f.name.clone(), Do::Reads)));
    }
    Reader { name: view.head.name.clone(), kind: view.head.kind, members, cases, generic: !view.generics.is_empty() }
}

/// One board page, compiled and read: the view and its workspace.
pub(crate) fn board(id: &str) -> Option<(View, Uses)> {
    let json = std::fs::read_to_string(board_dir().join(format!("{id}.json"))).ok()?;
    let v: Value = serde_json::from_str(&json).ok()?;
    let facts = facts_of(&v);
    let view = compile(&facts);
    let mut uses = read_all(&sites_of(&v), &reader_of(&view));
    uses.elsewhere = v.get("workspace").and_then(Value::as_str).map(ToOwned::to_owned);
    let view = with_uses(view, &uses);
    Some((view, uses))
}

// ------------------------------------------------------------------ the scene

struct State {
    ui: Ui,
    open: BTreeSet<FoldKey>,
    presences: BTreeMap<FoldKey, Presence>,
    spots: Rc<Spots>,
}

struct GalleryHost {
    state: Rc<RefCell<State>>,
    view: EntityId,
    doors: Still,
}

impl Doors for GalleryHost {
    fn door(&self, _: &str) -> Option<Door> {
        None
    }
    fn fold(&self, _: &'static str) -> Option<Fold> {
        None
    }
    fn track(&self, key: SharedString, label: SharedString, door: Option<&Door>, element: AnyElement) -> AnyElement {
        self.doors.track(key, label, door, element)
    }
    fn say(&self, _: &str) {}
}

impl Host for GalleryHost {
    fn ui(&self) -> Ui {
        self.state.borrow().ui.clone()
    }

    fn change(&self, change: Change) -> Act {
        let (state, view) = (Rc::clone(&self.state), self.view);
        Rc::new(move |_, cx| {
            let next = state.borrow().ui.clone().apply(&change);
            state.borrow_mut().ui = next;
            cx.notify(view);
        })
    }

    fn open_source(&self, _: &str, _: u32) -> Act {
        Rc::new(|_, _| {})
    }

    fn unfold(&self, key: &FoldKey) -> Option<Fold> {
        let mut state = self.state.borrow_mut();
        let presence = state.presences.entry(key.clone()).or_insert_with(|| Presence::new(format!("sym6-gallery-{key:?}"))).clone();
        let open = state.open.contains(key);
        drop(state);
        let (state, view, key) = (Rc::clone(&self.state), self.view, key.clone());
        Some(Fold {
            open,
            presence,
            toggle: Rc::new(move |_, cx| {
                let mut state = state.borrow_mut();
                if !state.open.remove(&key) {
                    state.open.insert(key.clone());
                }
                drop(state);
                cx.notify(view);
            }),
        })
    }

    fn lookup(&self, _: &str) -> Option<Act> {
        None
    }

    fn target(&self, _: &Key, _: SharedString, _: Act, element: AnyElement) -> AnyElement {
        element
    }

    fn reveal(&self, _: Sec) -> Act {
        Rc::new(|_, _| {})
    }

    fn spots(&self) -> Rc<Spots> {
        Rc::clone(&self.state.borrow().spots)
    }
}

struct SymbolScene {
    view: View,
    uses: Uses,
    state: Rc<RefCell<State>>,
}

fn scene(id: &str, open: &[FoldKey], cx: &mut App) -> AnyView {
    let Some((view, uses)) = board(id) else {
        return cx.new(|_: &mut Context<Missing>| Missing(id.to_owned())).into();
    };
    let state = Rc::new(RefCell::new(State { ui: Ui::default(), open: open.iter().cloned().collect(), presences: BTreeMap::new(), spots: Spots::new() }));
    cx.new(|_: &mut Context<SymbolScene>| SymbolScene { view, uses, state }).into()
}

struct Missing(String);

impl Render for Missing {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.facet().palette();
        div().size_full().bg(palette.g1.hsla()).child(SharedString::from(format!("the board's page model `{}` is not at {}", self.0, board_dir().display())))
    }
}

impl Render for SymbolScene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let width = f32::from(window.viewport_size().width);
        // The reader's own padding: fluid 22–40 px aside, the page as wide as the room.
        let pad = if width >= 900.0 { 40.0 } else { 22.0 };
        let measure = facet.measure(px((width - pad * 2.0).max(0.0)));
        let host = GalleryHost { state: Rc::clone(&self.state), view: cx.entity_id(), doors: Still };
        let spots = host.spots();
        let lay = Layout::of(&measure, &spots);
        let env = Env { m: measure.within(lay.main), p: palette, host: &host, lay };
        let i = ink(palette);
        let title = div().flex().items_baseline().self_start().child(said(&env, &Key::of(super::key::Part::Kind).field("title"), self.view.head.name.clone(), crate::tokens::scale::DISPLAY, i.ink0)).into_any_element();
        let _ = roles::NAME;
        let gem = super::gem(&self.view, &measure, palette);
        let element = page(&self.view, &self.uses, Chrome { gem, title }, &measure, palette, &host);
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(palette.g1.hsla())
            .child(div().id("sym6-scroll").size_full().overflow_y_scroll().child(div().px(px(pad)).pt(px(if width >= 900.0 { 40.0 } else { 22.0 })).pb(px(120.0)).child(element)))
            .child(float::layer(window, cx))
    }
}
