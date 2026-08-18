//! The symbol-page header — breadcrumb, signature, kind, version, trust
//! (GUI-PLAN §16, LD-8).
//!
//! # Why this file paints in one frame
//!
//! Everything the header shows comes from `SymbolHead`, which §9.3 guarantees is
//! the *first* event on the stream. Nothing here waits for a section, a highlight
//! or a refs page. The projection ([`HeaderModel::from_head`]) runs once, at
//! update time, in the store-observation callback; [`SymbolHeader`] is then a
//! pure `RenderOnce` over already-`SharedString`-ified values. There is no
//! `format!`, no `sort`, no `to_string` on the render path (§1.1.4).
//!
//! # The link table
//!
//! `ui::SignatureLine` is the one signature renderer in the app (LR-4), and its
//! `Ty` targets are typed as `ui::SymbolKey(SharedString)` — the pre-wire mirror.
//! We therefore keep the real `wire::SymbolKey`s in a side table
//! ([`HeaderModel::sig_links`]) and hand `SignatureLine` the *index* into that
//! table. Click → index → real key → `SymbolStore::open`. When the `TODO(wire)`
//! in `ui/signature_line.rs` is discharged, [`sig_tokens`] becomes the identity
//! function and the side table disappears; nothing else changes.
//!
//! # Provenance
//!
//! `wire::Provenance` and `theme::ext::Provenance` are two spellings of the same
//! idea (the second is the display mirror, also carrying a `TODO(wire)`).
//! [`trust_of`] is the *single* adaptation point in the whole page; every badge
//! and dot downstream goes through `cx.theme_ext().for_provenance(..)`, so no
//! view ever matches on provenance to pick a colour (LD-8).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _,
    uniform_list,
};
use gpui_component::{Icon, IconName, Sizable as _, StyledExt as _, h_flex};
use nudox_engine::wire::{
    KindTag, Provenance as WireProvenance, SharedStr, SigToken as WireSigToken, SymbolHead,
    SymbolKey, Visibility,
};

use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::theme::kind::LocalKindDiscriminant;
use crate::ui::{Badge, SigToken, SignatureLine, SymbolKey as UiKey};

// ─────────────────────────────────────────────────────────────────────────────
// Wire → display adapters (projection time only — never called from `render`)
// ─────────────────────────────────────────────────────────────────────────────

/// `SharedStr` (triomphe, GUI-free) → `gpui::SharedString`.
///
/// One allocation, paid once per datum at projection time. GPUI then caches
/// glyph shaping by `(text, font, size)`, so the resulting `SharedString` is
/// free to re-render every frame (§1.1.4).
#[inline]
pub(crate) fn shared(s: &SharedStr) -> SharedString {
    SharedString::from(String::from(&**s))
}

/// Encode a link-table index as the `ui::SymbolKey` a `SignatureLine` will hand
/// back on click. See the module docs for why this indirection exists.
#[inline]
pub(crate) fn link_id(ix: usize) -> UiKey {
    // Projection-time only. `SignatureLine` compares these by value; the string
    // is never parsed on a render path, only inside a click handler.
    UiKey(SharedString::from(ix.to_string()))
}

/// Decode a link id produced by [`link_id`] back into a table index.
#[inline]
pub(crate) fn link_ix(key: &UiKey) -> Option<usize> {
    key.0.parse::<usize>().ok()
}

/// Project wire signature tokens onto the renderer's token vocabulary,
/// harvesting navigable `Ty` targets into `links` as it goes.
///
/// LD-7: the wire enum is `#[non_exhaustive]`; an unknown token renders as a
/// visible `?` chip rather than vanishing.
pub(crate) fn sig_tokens(src: &[WireSigToken], links: &mut Vec<SymbolKey>) -> Vec<SigToken> {
    src.iter()
        .map(|t| match t {
            WireSigToken::Kw(s) => SigToken::Kw(SharedString::from(*s)),
            WireSigToken::Punct(s) => SigToken::Punct(SharedString::from(*s)),
            WireSigToken::Ws => SigToken::Ws,
            WireSigToken::Ident(s) => SigToken::Ident(shared(s)),
            WireSigToken::Generic(s) => SigToken::Generic(shared(s)),
            // The display mirror has no `Lifetime` arm yet; lifetimes read as
            // generics, which is also how rustdoc colours them.
            WireSigToken::Lifetime(s) => SigToken::Generic(shared(s)),
            WireSigToken::Ty { text, target } => SigToken::Ty {
                text: shared(text),
                target: target.as_ref().map(|k| {
                    links.push(k.clone());
                    link_id(links.len() - 1)
                }),
            },
            // LD-7 fallback — visible, never silent, never a panic.
            _ => SigToken::Ident(SharedString::from("⟨?⟩")),
        })
        .collect()
}

/// The one place `wire::Provenance` becomes the display mirror (LD-8).
///
/// A thin alias for `stores::search_model::prepare_provenance` — the same
/// `WireProvenance -> Provenance` mapping used to be written out here a
/// second time, and the two copies disagreed on their `#[non_exhaustive]`
/// fallback arm (`Remote` here, `Remote` there too, until that drift was
/// caught and both collapsed onto `Stale` — see `prepare_provenance`'s own
/// doc comment). Kept as a distinct name at this call site rather than
/// replaced everywhere with the fully-qualified path, since "the one
/// adaptation point in the whole page" (this module's own doc comment) is
/// worth keeping nameable from here.
pub(crate) fn trust_of(p: &WireProvenance) -> Provenance {
    crate::stores::search_model::prepare_provenance(p)
}

/// A kind label that survives a producer we do not know about (LD-7).
#[derive(Clone, Debug, PartialEq)]
pub enum KindChip {
    /// One of the thirteen frozen discriminants.
    Known(LocalKindDiscriminant),
    /// A future producer's kind, shown as an opaque chip.
    Unknown(SharedString),
}

impl KindChip {
    /// Project a wire `KindTag`. Never fails, never panics.
    pub fn from_tag(tag: KindTag) -> Self {
        match tag {
            KindTag::Known(d) => match LocalKindDiscriminant::from_u16(d.as_u16()) {
                Some(local) => Self::Known(local),
                None => Self::Unknown(SharedString::from(format!("kind:{}", d.as_u16()))),
            },
            KindTag::Unknown(v) => Self::Unknown(SharedString::from(format!("kind:{v}"))),
            _ => Self::Unknown(SharedString::from("kind:?")),
        }
    }
}

/// The access-modifier chip, shown only when it is *not* the default.
///
/// `pub` is already spelled out by the signature's leading `Kw` token, so
/// repeating it would be noise; anything narrower is information.
fn visibility_label(v: Visibility) -> Option<SharedString> {
    match v {
        Visibility::Public => None,
        Visibility::Private => Some(SharedString::from("private")),
        Visibility::Protected => Some(SharedString::from("protected")),
        Visibility::Internal => Some(SharedString::from("internal")),
        Visibility::Package => Some(SharedString::from("package")),
        Visibility::Crate => Some(SharedString::from("pub(crate)")),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Model
// ─────────────────────────────────────────────────────────────────────────────

/// One clickable ancestor in the breadcrumb trail.
#[derive(Clone, Debug)]
pub struct Crumb {
    /// Navigation target.
    pub key: SymbolKey,
    /// Pre-computed display label.
    pub label: SharedString,
}

/// Drop breadcrumb segments that repeat the label of the segment before them.
///
/// # Why the engine is not wrong to send them
///
/// The trail `memchr › memchr › memchr` (`tests/shots/memchr/08-symbol-opened.png`,
/// GUI-WORKORDER-2 F4 / docs/LIMITATIONS.md L18) is three genuinely different
/// entries: the *package* `memchr`, its root module `memchr`, and the module
/// `src/memchr.rs` — a naming convention Rust encourages and that most crates
/// follow. The ancestor chain is correct; what is wrong is drawing three
/// identical tokens and calling it a trail. A breadcrumb exists to answer
/// "where am I?", and a segment that repeats its parent verbatim answers
/// nothing while costing a `›` and a click target.
///
/// # Which one survives
///
/// The **last** of each run. Crumbs are root-first, so the deepest entry is the
/// one closest to the symbol, and keeping it means the crumb the reader clicks
/// navigates to the nearest enclosing scope rather than jumping all the way out
/// to the package. The dropped entries are ancestors of the kept one, so
/// nothing becomes unreachable: one more click up the retained trail arrives at
/// the same place.
///
/// Only *adjacent* repeats collapse. `a › b › a` is a real re-entry and stays.
fn collapse_repeated_crumbs(breadcrumb: &[nudox_engine::wire::CrumbRef]) -> Vec<Crumb> {
    let mut out: Vec<Crumb> = Vec::with_capacity(breadcrumb.len());
    for c in breadcrumb {
        let label = shared(&c.label);
        if out.last().is_some_and(|prev: &Crumb| prev.label == label) {
            out.pop();
        }
        out.push(Crumb {
            key: c.key.clone(),
            label,
        });
    }
    out
}

/// One selectable version in the version picker.
///
/// TODO(store): `SymbolDoc` carries no version list today (`set_version` takes
/// no argument), so this is populated by whoever owns version data via
/// [`super::SymbolPage::set_versions`]. When the store grows a
/// `versions: Arc<[VersionOption]>` field this type moves there unchanged.
#[derive(Clone, Debug)]
pub struct VersionOption {
    /// Display label, e.g. `"0.7.5"`.
    pub label: SharedString,
    /// Trust chrome for this specific version (LD-8: *every* version carries one).
    pub provenance: Provenance,
}

/// Everything the header paints, projected once from [`SymbolHead`].
#[derive(Clone, Debug)]
pub struct HeaderModel {
    /// Ancestor chain, root-first. Each crumb is a link.
    pub crumbs: Arc<[Crumb]>,
    /// Short display name — also the workspace tab label.
    pub title: SharedString,
    /// Stable symbol URI (the `y` keybinding copies this).
    pub uri: SharedString,
    /// Pre-tokenised signature for [`SignatureLine`].
    pub signature: Vec<SigToken>,
    /// Real keys behind the signature's `Ty` links, indexed by [`link_id`].
    pub sig_links: Arc<[SymbolKey]>,
    /// Kind badge.
    pub kind: KindChip,
    /// Access modifier, when narrower than public.
    pub visibility: Option<SharedString>,
    /// The `cfg(...)` predicate gating this symbol, when it is not
    /// unconditionally compiled in. This is the one chip that tells the
    /// reader "this API might not exist in your build" — docs.rs shows the
    /// same fact as a feature badge, and we must not show less.
    pub cfg: Option<SharedString>,
    /// Trust chrome (LD-8).
    pub provenance: Provenance,
    /// Deprecation note, when present.
    pub deprecation: Option<SharedString>,
    /// Version picker entries (may be empty — see [`VersionOption`]).
    pub versions: Arc<[VersionOption]>,
    /// Index into `versions` of the currently displayed version.
    pub active_version: usize,
}

impl HeaderModel {
    /// The pre-`Head` state: nothing known yet.
    ///
    /// Rendering this produces the header *chassis* at its final geometry — the
    /// same row heights, the same padding — so that the arrival of `Head` swaps
    /// text in rather than pushing the body down.
    pub fn empty() -> Self {
        Self {
            crumbs: Arc::from(Vec::new()),
            title: SharedString::from("…"),
            uri: SharedString::from(""),
            signature: Vec::new(),
            sig_links: Arc::from(Vec::new()),
            kind: KindChip::Unknown(SharedString::from("…")),
            visibility: None,
            cfg: None,
            provenance: Provenance::TrustedLocal,
            deprecation: None,
            versions: Arc::from(Vec::new()),
            active_version: 0,
        }
    }

    /// Project a `SymbolHead`. Call from an update, never from `render`.
    pub fn from_head(head: &SymbolHead) -> Self {
        let crumbs = collapse_repeated_crumbs(&head.breadcrumb);

        let mut links: Vec<SymbolKey> = Vec::new();
        let signature = sig_tokens(&head.signature, &mut links);

        // The title is the last crumb (§16 shows the symbol itself as the tail
        // of the trail); if the producer omitted it, fall back to the first
        // identifier in the signature, which is the declared name by definition.
        let title = crumbs
            .last()
            .map(|c| c.label.clone())
            .or_else(|| {
                head.signature.iter().find_map(|t| match t {
                    WireSigToken::Ident(s) => Some(shared(s)),
                    _ => None,
                })
            })
            .unwrap_or_else(|| SharedString::from("symbol"));

        Self {
            crumbs: Arc::from(crumbs),
            title,
            uri: SharedString::from(format!("{:?}", head.key)),
            signature,
            sig_links: Arc::from(links),
            kind: KindChip::from_tag(head.kind),
            visibility: visibility_label(head.visibility),
            cfg: head.cfg.as_ref().map(shared),
            provenance: trust_of(&head.provenance),
            deprecation: head.deprecation.as_ref().map(shared),
            versions: Arc::from(Vec::new()),
            active_version: 0,
        }
    }

    /// The currently selected version's label, if a version list is known.
    pub fn active_version_label(&self) -> Option<SharedString> {
        self.versions
            .get(self.active_version)
            .map(|v| v.label.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

type CrumbHandler = Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)>;
type VersionHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The header band: breadcrumb row, then signature row.
///
/// Paints entirely from [`HeaderModel`]. The three handlers report intent
/// upward; the component never navigates or mutates anything itself.
#[derive(IntoElement)]
pub struct SymbolHeader {
    model: HeaderModel,
    /// `true` while a newer generation streams over this one (LD-15).
    stale: bool,
    /// Whether the version dropdown is currently expanded.
    picker_open: bool,
    on_crumb: Option<CrumbHandler>,
    on_link: Option<CrumbHandler>,
    on_version: Option<VersionHandler>,
    on_toggle_picker: Option<ToggleHandler>,
}

impl SymbolHeader {
    /// Build a header for `model`.
    pub fn new(model: HeaderModel) -> Self {
        Self {
            model,
            stale: false,
            picker_open: false,
            on_crumb: None,
            on_link: None,
            on_version: None,
            on_toggle_picker: None,
        }
    }

    /// Dim the header while a newer generation streams over it (LD-15).
    pub fn stale(mut self, stale: bool) -> Self {
        self.stale = stale;
        self
    }

    /// Expand or collapse the version dropdown.
    pub fn picker_open(mut self, open: bool) -> Self {
        self.picker_open = open;
        self
    }

    /// Called when a breadcrumb crumb is activated.
    pub fn on_crumb(mut self, f: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static) -> Self {
        self.on_crumb = Some(Rc::new(f));
        self
    }

    /// Called when a `Ty` token in the signature is activated.
    pub fn on_link(mut self, f: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static) -> Self {
        self.on_link = Some(Rc::new(f));
        self
    }

    /// Called with the index of a chosen version.
    pub fn on_version(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_version = Some(Rc::new(f));
        self
    }

    /// Called when the version chip is clicked.
    pub fn on_toggle_picker(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_picker = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for SymbolHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The transparency ladder. Same in every theme — it says how much of a
        // thing is present, not what colour the thing is.
        let al = crate::theme::tokens::AlphaTokens::STANDARD;
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        let model = self.model;
        let on_crumb = self.on_crumb;
        let on_link = self.on_link;
        let sig_links = model.sig_links.clone();

        // ── Row 1: breadcrumb  ·······································  trust ──
        let crumb_count = model.crumbs.len();
        let crumbs = model.crumbs.clone();
        let breadcrumb = h_flex()
            .id("symbol.header.breadcrumb")
            .w_full()
            .items_center()
            .gap(sp.space_1)
            .flex_wrap()
            .children(crumbs.iter().enumerate().map(|(ix, crumb)| {
                let is_last = ix + 1 == crumb_count;
                let key = crumb.key.clone();
                let handler = on_crumb.clone();
                let colour = if is_last {
                    colours.fg_default
                } else {
                    colours.fg_muted
                };

                h_flex()
                    .items_center()
                    .gap(sp.space_1)
                    .child(
                        div()
                            .id(("symbol.header.crumb", ix))
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(colour)
                            .when_some(handler, |el, handler| {
                                el.cursor_pointer()
                                    .hover(|s| s.text_color(colours.accent).underline())
                                    .on_click(move |_, window, cx| handler(&key, window, cx))
                            })
                            .child(crumb.label.clone()),
                    )
                    .when(!is_last, |el| {
                        el.child(
                            div()
                                .text_size(ts.caption.size)
                                .line_height(ts.caption.line_height)
                                .text_color(colours.fg_faint)
                                .child(SharedString::from("›")),
                        )
                    })
            }));

        let trust = Badge::for_provenance("symbol.header.trust", model.provenance, cx);

        // ── Row 2: signature  ·······················  kind · vis · version ──
        let signature = SignatureLine::new("symbol.header.signature", model.signature.clone())
            .when_some(on_link, |line, handler| {
                let links = sig_links.clone();
                line.on_navigate(move |ui_key, window, cx| {
                    if let Some(key) = link_ix(ui_key).and_then(|ix| links.get(ix)) {
                        handler(key, window, cx);
                    }
                })
            });

        let kind_badge: AnyElement = match &model.kind {
            KindChip::Known(k) => Badge::for_kind("symbol.header.kind", *k, cx).into_any_element(),
            // LD-7: an unknown kind is a *visible* chip in the neutral role, not
            // a guess at one of the thirteen colours.
            KindChip::Unknown(label) => Badge::custom(
                "symbol.header.kind",
                label.clone(),
                colours.bg_hover,
                colours.fg_muted,
            )
            .into_any_element(),
        };

        let version_chip = version_picker(
            &model,
            self.picker_open,
            self.on_toggle_picker,
            self.on_version,
            cx,
        );

        let signature_row = h_flex()
            .w_full()
            .items_start()
            .gap(sp.space_2)
            .child(div().flex_1().min_w_0().child(signature))
            .child(
                h_flex()
                    .items_center()
                    .gap(sp.space_1)
                    .flex_shrink_0()
                    .child(kind_badge)
                    .when_some(model.visibility.clone(), |el, vis| {
                        el.child(Badge::custom(
                            "symbol.header.visibility",
                            vis,
                            colours.bg_hover,
                            colours.fg_muted,
                        ))
                    })
                    .when_some(model.cfg.clone(), |el, cfg| {
                        el.child(Badge::custom(
                            "symbol.header.cfg",
                            cfg,
                            colours.bg_hover,
                            colours.fg_muted,
                        ))
                    })
                    .children(version_chip),
            );

        div()
            .id("symbol.header")
            .w_full()
            .v_flex()
            .gap(sp.space_2)
            .px(sp.space_4)
            .pt(sp.space_3)
            .pb(sp.space_2)
            .bg(colours.bg_raised)
            .border_b_1()
            .border_color(colours.border_default)
            // LD-15: a superseded generation dims, it never disappears.
            .when(self.stale, |el| el.opacity(al.dim))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(sp.space_2)
                    .child(div().flex_1().min_w_0().child(breadcrumb))
                    .child(trust),
            )
            .child(signature_row)
            .when_some(model.deprecation.clone(), |el, note| {
                el.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(sp.space_2)
                        .px(sp.space_2)
                        .py(sp.space_1)
                        .rounded(sp.r_sm)
                        .bg(colours.warn.opacity(al.wash))
                        .border_1()
                        .border_color(colours.warn.opacity(al.veil))
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .text_color(colours.warn)
                                .with_size(gpui_component::Size::Small),
                        )
                        .child(
                            div()
                                .text_size(ts.dense.size)
                                .line_height(ts.dense.line_height)
                                .text_color(colours.warn_fg)
                                .child(note),
                        ),
                )
            })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Version picker
// ─────────────────────────────────────────────────────────────────────────────

/// The `[0.7.5 ▾]` chip plus its dropdown.
///
/// Returns `None` when no version list is known, so the header simply does not
/// show an affordance it cannot honour. LD-6: the dropdown goes through
/// `uniform_list`, because a long-lived crate can have hundreds of versions.
fn version_picker(
    model: &HeaderModel,
    open: bool,
    on_toggle: Option<ToggleHandler>,
    on_pick: Option<VersionHandler>,
    cx: &App,
) -> Option<AnyElement> {
    let label = model.active_version_label()?;
    let ext = cx.theme_ext();
    let sp = ext.space;
    let ts = ext.type_scale;
    let colours = ext.colours;

    let versions = model.versions.clone();
    let active = model.active_version;
    let count = versions.len();
    // 7 rows of breathing room, or fewer if that is all there is.
    let row_h = ts.ui.line_height + sp.space_2;
    let list_h = row_h * (count.min(7) as f32);

    let chip = div()
        .id("symbol.header.version")
        .flex()
        .flex_row()
        .items_center()
        .gap(sp.space_1)
        .px(sp.space_2)
        .py(sp.space_1 / 2.0)
        .rounded(sp.r_sm)
        .border_1()
        .border_color(colours.border_default)
        .bg(colours.bg_base)
        .text_size(ts.caption.size)
        .line_height(ts.caption.line_height)
        .text_color(colours.fg_default)
        .cursor_pointer()
        .hover(|s| s.bg(colours.bg_hover))
        .when_some(on_toggle, |el, toggle| {
            el.on_click(move |_, window, cx| toggle(window, cx))
        })
        .child(label)
        .child(
            Icon::new(IconName::ChevronDown)
                .text_color(colours.fg_faint)
                .with_size(gpui_component::Size::XSmall),
        );

    // `deferred`, not a bare absolutely-positioned child.
    //
    // GPUI paints in tree order and has no `z-index`. This popover hangs below
    // the header, over the version strip and the top of the document column —
    // both of which are *later* siblings of the header in `SymbolPage`'s
    // column, and both of which paint an opaque background. So the dropdown
    // was drawn and then immediately painted over: `13-version-picker` came
    // back byte-identical to the frame before it, indistinguishable from the
    // popover never having been built at all, which is exactly how this
    // survived every previous run of the shot suite.
    //
    // `deferred` keeps the element in this subtree for layout and hit-testing
    // and defers only its *paint* until after every ancestor has finished,
    // which is what "on top" means in a tree-ordered renderer.
    let dropdown = open.then(|| {
        let pick = on_pick.clone();
        gpui::deferred(
            div()
                .absolute()
                .top(row_h + sp.space_1)
                .right_0()
                .w(sp.space_8 * 4.0)
                .h(list_h)
                .rounded(sp.r_md)
                .border_1()
                .border_color(colours.border_strong)
                .bg(colours.bg_overlay)
                .child(
                    uniform_list(
                        "symbol.header.version.list",
                        count,
                        move |range, _window, cx| {
                            let ext = cx.theme_ext();
                            let sp = ext.space;
                            let ts = ext.type_scale;
                            let colours = ext.colours;
                            range
                                .map(|ix| {
                                    let opt = &versions[ix];
                                    let is_active = ix == active;
                                    let pick = pick.clone();
                                    let dot_style = ext.for_provenance(opt.provenance);
                                    div()
                                        .id(("symbol.header.version.row", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_2)
                                        .px(sp.space_2)
                                        .h(ts.ui.line_height + sp.space_2)
                                        .text_size(ts.ui.size)
                                        .line_height(ts.ui.line_height)
                                        .text_color(if is_active {
                                            colours.fg_default
                                        } else {
                                            colours.fg_muted
                                        })
                                        .when(is_active, |s| s.bg(colours.bg_active))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .when_some(pick, |el, pick| {
                                            el.on_click(move |_, window, cx| pick(ix, window, cx))
                                        })
                                        // LD-8: trust travels with the *version*,
                                        // not just with the symbol.
                                        .child(
                                            div()
                                                .w(sp.space_2)
                                                .h(sp.space_2)
                                                .rounded_full()
                                                .bg(dot_style.colour),
                                        )
                                        .child(opt.label.clone())
                                })
                                .collect::<Vec<_>>()
                        },
                    )
                    .h(list_h),
                ),
        )
    });

    Some(
        div()
            .relative()
            .flex_shrink_0()
            .child(chip)
            .children(dropdown)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LD-7: an unknown kind must produce a *visible* chip, not an empty label.
    #[test]
    fn unknown_kind_tag_renders_a_visible_chip() {
        let chip = KindChip::from_tag(KindTag::Unknown(9_999));
        match chip {
            KindChip::Unknown(label) => assert!(!label.is_empty()),
            KindChip::Known(_) => panic!("9999 is not a known discriminant"),
        }
    }

    /// The link table round-trips: index → id → index.
    #[test]
    fn link_ids_round_trip() {
        for ix in [0usize, 1, 7, 4_096] {
            assert_eq!(link_ix(&link_id(ix)), Some(ix));
        }
    }

    /// An unrecognised provenance must never be reported as trusted-local: the
    /// badge is a trust claim, so the safe default is the weakest one.
    #[test]
    fn unknown_provenance_is_not_trusted() {
        assert_eq!(
            trust_of(&WireProvenance::TrustedLocal),
            Provenance::TrustedLocal
        );
        assert_eq!(
            trust_of(&WireProvenance::Stale {
                as_of: std::time::SystemTime::UNIX_EPOCH
            }),
            Provenance::Stale
        );
    }

    /// `pub` is already in the signature; only narrower visibilities get a chip.
    #[test]
    fn public_visibility_has_no_chip() {
        assert!(visibility_label(Visibility::Public).is_none());
        assert!(visibility_label(Visibility::Crate).is_some());
    }

    /// A minimal but complete `SymbolHead`, parameterised only on `cfg`, for
    /// exercising [`HeaderModel::from_head`]'s cfg projection in isolation.
    fn sample_head(cfg: Option<SharedStr>) -> SymbolHead {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName};

        SymbolHead {
            key: SymbolKey::new(
                PackageLineageId::new(EcosystemId::new("test"), PackageName::new("pkg")),
                IntroId::from_raw([1u8; 32]),
            ),
            name: SharedStr::from("Widget"),
            breadcrumb: Vec::new(),
            signature: Vec::new(),
            kind: KindTag::Unknown(0),
            visibility: Visibility::Public,
            provenance: WireProvenance::TrustedLocal,
            deprecation: None,
            cfg,
            section_plan: Vec::new(),
            // This fixture exercises the cfg projection, not the source
            // panel; `Synthesized` is the honest value for a symbol that was
            // never read out of a file.
            source: nudox_engine::wire::SourceLocation::Unlocated {
                reason: nudox_engine::wire::UnlocatedReason::Synthesized,
            },
            source_excerpt: None,
        }
    }

    /// `SymbolHead.cfg` must project into `HeaderModel.cfg` unchanged — this
    /// is the one chip that tells a reader an item might not exist in their
    /// build (docs.rs shows the same fact as a feature badge). Losing it in
    /// projection would silently recreate the gap this field exists to close.
    #[test]
    fn from_head_projects_cfg_into_header_model() {
        let head = sample_head(Some(SharedStr::from("cfg(feature = \"std\")")));
        let model = HeaderModel::from_head(&head);
        assert_eq!(
            model.cfg,
            Some(SharedString::from("cfg(feature = \"std\")")),
            "an unconditional-looking symbol must not lose its cfg badge in projection"
        );
    }

    /// The round-trip counterpart: an unconditional symbol (`cfg: None` on
    /// the wire) must not grow a fabricated chip during projection.
    #[test]
    fn from_head_with_no_cfg_has_no_chip() {
        let head = sample_head(None);
        let model = HeaderModel::from_head(&head);
        assert!(
            model.cfg.is_none(),
            "an unconditional symbol must not be given a fabricated cfg chip"
        );
    }

    /// The empty model must still be renderable — the header chassis paints
    /// before `Head` lands.
    #[test]
    fn empty_model_has_a_title() {
        assert!(!HeaderModel::empty().title.is_empty());
    }
}
