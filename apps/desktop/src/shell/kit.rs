//! Small shell-side vocabulary shared by the regions and the plain page
//! bodies: typeset text, kind and language marks from engine types, the
//! "said once" gap line, the loading comb, and hover-intent prefetch.
//!
//! Nothing here is a component: anything with a design owner in `facet`
//! (buttons, key caps, cut plates, marks) comes from `facet`.

use super::region::Links;
use crate::model::pages::{Gap, GapReason, PageKey};
use backend_library::DeclarationKind;
use facet::icons::Kind;
use facet::tokens::TypeRole;
use facet::{Measure, Palette, Set as _};
use gpui::{
    App, Bounds, Context, Div, Hsla, IntoElement, ParentElement, Pixels, SharedString, Styled,
    Task, canvas, div, fill, point, px, size,
};
use std::time::Duration;

/// How long the pointer rests on a link before its page is prefetched.
pub(crate) const PREFETCH_DELAY: Duration = Duration::from_millis(120);

/// A text run in `role` for the container `measure` describes. Every string
/// it is given is published to the probe ledger with its box and overflow
/// mode (wrap, ellipsis or clip, read from its own style), so the harness
/// can lint what the shell actually set.
pub(crate) fn text(role: TypeRole, measure: &Measure, color: impl Into<Hsla>) -> Said {
    Said {
        div: div().set(role, measure).text_color(color.into()),
        role: measure.role(role),
        content: String::new(),
        key: None,
    }
}

/// See [`text`]: a styled text box that knows its words.
pub(crate) struct Said {
    div: Div,
    role: TypeRole,
    content: String,
    key: Option<gpui::ElementId>,
}

/// A child a [`Said`] can hold, and the words it contributes.
pub(crate) trait SaidChild: IntoElement {
    /// The words this child puts on screen, when it knows them.
    fn words(&self) -> Option<&str>;
}

impl SaidChild for &'static str {
    fn words(&self) -> Option<&str> {
        Some(self)
    }
}

impl SaidChild for String {
    fn words(&self) -> Option<&str> {
        Some(self)
    }
}

impl SaidChild for SharedString {
    fn words(&self) -> Option<&str> {
        Some(self)
    }
}

impl SaidChild for gpui::StyledText {
    fn words(&self) -> Option<&str> {
        None
    }
}

impl SaidChild for gpui::InteractiveText {
    fn words(&self) -> Option<&str> {
        None
    }
}

impl Said {
    /// Adds a child; strings are also recorded as the box's words.
    pub(crate) fn child(mut self, child: impl SaidChild) -> Self {
        if let Some(words) = child.words() {
            self.content.push_str(words);
        }
        self.div = self.div.child(child);
        self
    }
}

impl Styled for Said {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.div.style()
    }
}

impl gpui::InteractiveElement for Said {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl IntoElement for Said {
    type Element = gpui::AnyElement;

    fn into_element(mut self) -> gpui::AnyElement {
        if self.content.is_empty() {
            return self.div.into_any_element();
        }
        let style = &self.div.style().text;
        let overflow = if style.text_overflow.is_some() {
            facet::probe::TextOverflow::Ellipsis
        } else if style.white_space == Some(gpui::WhiteSpace::Nowrap) {
            facet::probe::TextOverflow::Clip
        } else {
            facet::probe::TextOverflow::Wrap
        };
        let key = self
            .key
            .unwrap_or_else(|| gpui::ElementId::Name(SharedString::from(format!("text:{}", self.content))));
        facet::probe::text(key, self.content, self.role, 1.0, overflow, self.div).into_any_element()
    }
}

/// The facet kind mark for an engine declaration kind.
pub(crate) const fn kind_of(kind: Option<DeclarationKind>) -> Kind {
    match kind {
        Some(DeclarationKind::Module) => Kind::Module,
        Some(DeclarationKind::Import) => Kind::Import,
        Some(DeclarationKind::Struct) => Kind::Struct,
        Some(DeclarationKind::Class) => Kind::Class,
        Some(DeclarationKind::Enum) => Kind::Enum,
        Some(DeclarationKind::Union) => Kind::Union,
        Some(DeclarationKind::Type) => Kind::Type,
        Some(DeclarationKind::Trait) => Kind::Trait,
        Some(DeclarationKind::Interface) => Kind::Interface,
        Some(DeclarationKind::Function) => Kind::Function,
        Some(DeclarationKind::Method) => Kind::Method,
        Some(DeclarationKind::Constructor) => Kind::Constructor,
        Some(DeclarationKind::Macro) => Kind::Macro,
        Some(DeclarationKind::Constant) => Kind::Constant,
        Some(DeclarationKind::Field) => Kind::Field,
        Some(DeclarationKind::Property) => Kind::Property,
        Some(DeclarationKind::Variable) => Kind::Variable,
        Some(DeclarationKind::Variant) => Kind::Variant,
        Some(DeclarationKind::Unknown) | None => Kind::Unknown,
    }
}

/// Plain words for why a field is unknown: said once, in place.
pub(crate) fn gap_words(gap: &Gap) -> SharedString {
    let reason = match gap.reason {
        GapReason::NotCaptured => "Nothing was captured for this",
        GapReason::NotHydrated => "Not on this machine yet",
        GapReason::Unconfigured => "No provider is configured for this",
        GapReason::NoSemanticPublication => {
            "Relations need a compiler publication; this package has none"
        }
        GapReason::NotRecorded => "The feed does not record this",
        GapReason::Unsupported => "The source does not support this",
        GapReason::Unavailable => "The source could not provide this",
        GapReason::Stale => "This answer is from an older index",
        GapReason::Unknown => "Coverage is not established",
        GapReason::LocalProject => "A local project has no registry record",
        GapReason::ReadFailed => "The read failed",
        GapReason::Encoded => "Served as an encoded compiler type, not source text",
        GapReason::NotServed => "The engine does not serve this",
    };
    if gap.detail.is_empty() || gap.reason == GapReason::NoSemanticPublication {
        SharedString::from(format!("{reason}."))
    } else {
        SharedString::from(format!("{reason}: {}", gap.detail))
    }
}

/// One quiet italic line: a gap, a caption, a margin note's body.
pub(crate) fn quiet(words: impl Into<SharedString>, measure: &Measure, palette: &Palette) -> Said {
    text(facet::tokens::ty::CAPTION, measure, palette.ink3).child(words.into())
}

/// A comb of hairline ticks where text will arrive (the loading board):
/// `width` px wide, one line of `role` tall.
pub(crate) fn pending(width: Pixels, role: TypeRole, measure: &Measure, palette: &Palette) -> impl IntoElement {
    let role = measure.role(role);
    let tick: Hsla = palette.ink4.into();
    let height = px(role.line);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let top = bounds.origin.y + height * 0.25;
            let tall = height * 0.5;
            let mut x = bounds.origin.x;
            let end = bounds.origin.x + bounds.size.width;
            while x < end {
                window.paint_quad(fill(Bounds::new(point(x, top), size(px(1.0), tall)), tick));
                x += px(4.0);
            }
        },
    )
    .w(width)
    .h(height)
}

/// Hover intent for one region: after the pointer rests on a link for
/// [`PREFETCH_DELAY`] its page is prefetched through the read pool; leaving
/// first cancels the timer, leaving after cancels the prefetch.
#[derive(Default)]
pub(crate) struct HoverIntent {
    hovered: Option<PageKey>,
    timer: Option<Task<()>>,
}

impl HoverIntent {
    /// Reports the pointer entering (`true`) or leaving (`false`) a link.
    pub(crate) fn hover<R: 'static>(&mut self, key: PageKey, hovered: bool, links: &Links, cx: &mut Context<R>) {
        if hovered {
            if self.hovered.as_ref() == Some(&key) {
                return;
            }
            self.leave(links, cx);
            self.hovered = Some(key.clone());
            let links = links.clone();
            self.timer = Some(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(PREFETCH_DELAY).await;
                cx.update(|cx| links.prefetch(key, cx));
            }));
        } else if self.hovered.as_ref() == Some(&key) {
            self.leave(links, cx);
        }
    }

    fn leave(&mut self, links: &Links, cx: &mut App) {
        self.timer = None;
        if let Some(key) = self.hovered.take() {
            links.cancel_prefetch(&key, cx);
        }
    }
}

/// Publishes a scroll container's viewport and full content extent to the
/// probe ledger, so the harness's `offscreen` lint can tell rows reachable by
/// scrolling from rows clipped away. Add it right after the container, as
/// its sibling: the container has settled its scroll bounds by then.
pub(crate) fn scroll_probe(key: &'static str, handle: gpui::ScrollHandle) -> impl IntoElement {
    canvas(
        move |_, _, cx| {
            if facet::probe::enabled(cx) {
                let viewport = handle.bounds();
                let reach = handle.max_offset();
                let content = Bounds::new(
                    viewport.origin,
                    size(viewport.size.width + reach.x, viewport.size.height + reach.y),
                );
                facet::probe::record_scroll(cx, &gpui::ElementId::Name(SharedString::new_static(key)), viewport, content);
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_0()
}

/// A key cap that shows over a shell-drawn control while ⌘ is held (facet
/// controls rise their own through `.key(…)`).
pub(crate) fn keycap(shown: bool, label: &'static str, measure: &Measure) -> Option<gpui::AnyElement> {
    shown.then(|| {
        div()
            .absolute()
            .top(-measure.space(facet::Space::Snug))
            .right(-measure.space(facet::Space::Snug))
            .child(facet::controls::kbd(label, measure).hot())
            .into_any_element()
    })
}

/// The route to a declaration inside `package`, shown as `view`.
pub(crate) fn symbol_view_route(
    package: &str,
    symbol: &crate::model::pages::SymbolRef,
    view: crate::navigation::View,
    line: Option<u32>,
) -> Option<crate::navigation::Route> {
    Some(crate::navigation::Route::Symbol(crate::navigation::SymbolRoute {
        project: None,
        package: crate::core::PackageId::new(package).ok()?,
        id: crate::navigation::Coordinate::new(symbol.as_str()).ok()?,
        at: None,
        view,
        line,
        selected: None,
    }))
}

/// The page route for a declaration inside `package`.
pub(crate) fn symbol_route(package: &str, symbol: &crate::model::pages::SymbolRef) -> Option<crate::navigation::Route> {
    symbol_view_route(package, symbol, crate::navigation::View::Page, None)
}

/// The package route for a package.
pub(crate) fn package_route(package: &crate::model::pages::PackageRef) -> Option<crate::navigation::Route> {
    Some(crate::navigation::Route::Package(crate::navigation::PackageRoute {
        project: None,
        package: crate::core::PackageId::new(package.as_str()).ok()?,
        lane: crate::navigation::PackageLane::Overview,
        selected: None,
        at: None,
    }))
}

/// The element id a declaration's mark carries on every view that shows it
/// (the page's hero gem, the graph's node, a row's mark), so W-Flow's
/// shared-element transition matches them across a view switch.
pub(crate) fn shared_id(symbol: &crate::model::pages::SymbolRef) -> gpui::ElementId {
    gpui::ElementId::Name(SharedString::from(shared_key(symbol)))
}

/// The string behind [`shared_id`] (also its debug selector, so tests can
/// find the mark's bounds on each view).
pub(crate) fn shared_key(symbol: &crate::model::pages::SymbolRef) -> String {
    format!("decl:{}", symbol.as_str())
}

/// The package a declaration's coordinate names, as the route spelling.
pub(crate) fn package_of(symbol: &crate::model::pages::SymbolRef) -> Option<String> {
    symbol.package().map(|package| package.as_str().to_owned())
}

/// A kind mark at the size its text is set at: the three board sizes, picked
/// by the text scale so a mark beside 200 % text is not a 100 % speck.
pub(crate) fn kind_mark(kind: Kind, base: facet::icons::KindSize, measure: &Measure, palette: &Palette) -> gpui::AnyElement {
    use facet::icons::KindSize;
    let (boxed, _, _) = base.metrics();
    let wanted = boxed * measure.scale();
    let size = if wanted < 16.5 {
        KindSize::Sm
    } else if wanted < 22.0 {
        KindSize::Md
    } else {
        KindSize::Lg
    };
    facet::icons::kind_mark(kind, size, palette)
}
