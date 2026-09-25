//! Loading, fault and unavailable pages, said once, in place.
//!
//! A page that has a value shows it, even while a newer one is fetched (a
//! refresh never blanks the screen). Without one, it says exactly one of:
//! still arriving (a comb of hairlines where the words will land), stopped
//! (one coral plate: what happened, the code, one way to try again), or not
//! served (one quiet line).

use super::{Ctx, Leaf};
use crate::core::{ErrorValue, FaultCode, Resource, ResourceTerminal, UnavailableReason};
use crate::model::pages::PageKey;
use crate::shell::kit::{pending, quiet, text};
use crate::shell::reader::Reader;
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{Set as _, Space};
use gpui::{Context, IntoElement, ParentElement, SharedString, Styled, div, px};

/// What a resource can show right now.
pub(crate) enum Shown<'a, T> {
    /// A value (possibly while a newer one is fetched).
    Ready(&'a T),
    /// Nothing yet; a read is coming or running.
    Pending,
    /// The last read stopped with a fault.
    Fault(&'a ErrorValue),
    /// The producer does not serve it.
    Unavailable(&'a UnavailableReason, Option<SharedString>),
}

/// Classifies a resource.
pub(crate) fn shown<T>(resource: &Resource<T>) -> Shown<'_, T> {
    if let Some(value) = resource.loaded_value() {
        return Shown::Ready(value);
    }
    match resource.terminal() {
        ResourceTerminal::Fault(error) => Shown::Fault(error),
        ResourceTerminal::Unavailable(reason) => Shown::Unavailable(reason, None),
        ResourceTerminal::Complete => Shown::Pending,
    }
}

/// The page for a resource with no value: `what` names it ("RelationLabel").
pub(crate) fn not_ready<T>(
    shown: &Shown<'_, T>,
    key: &PageKey,
    what: &str,
    ctx: &mut Ctx<'_>,
    _cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    match shown {
        Shown::Ready(_) => Vec::new(),
        Shown::Pending => {
            let said = ctx.say(format!("{what} is on its way"));
            let hero = div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Wide))
                .child(facet::paint::gem::gem(facet::icons::Kind::Unknown).size(56.0 * measure.scale()).opacity(0.35))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(measure.space(Space::Base))
                        .child(text(ty::HERO, &measure, palette.ink2).child(SharedString::from(what.to_owned())))
                        .child(pending(px(320.0 * measure.scale()), ty::LEDE, &measure, palette)),
                );
            let lines = div()
                .flex()
                .flex_col()
                .gap(measure.space(Space::Roomy))
                .child(pending(px(220.0 * measure.scale()), ty::BODY, &measure, palette))
                .child(pending(px(420.0 * measure.scale()), ty::CODE, &measure, palette))
                .child(pending(px(360.0 * measure.scale()), ty::CODE, &measure, palette))
                .child(quiet(said, &measure, palette));
            vec![Leaf::new(hero), Leaf::new(lines)]
        }
        Shown::Fault(error) => {
            let message = ctx.say(error.message().to_owned());
            let code = ctx.say(fault_code(error.code()));
            let links = ctx.links.clone();
            let key = key.clone();
            let plate = cut()
                .chamfer(Chamfer::Md)
                .bevel(Bevel::Coral)
                .fill(palette.plate)
                .p(measure.space(Space::Gutter))
                .flex()
                .flex_col()
                .gap(measure.space(Space::Base))
                .child(text(ty::HEAD, &measure, palette.ink0).child(format!("{what} could not be read.")))
                .child(text(ty::BODY, &measure, palette.ink2).child(message))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(measure.space(Space::Roomy))
                        .child(
                            facet::controls::button("retry", "Try again", &measure)
                                .primary()
                                .on_click(move |_, cx| links.retry(key.clone(), cx)),
                        )
                        .child(div().set(ty::MONO_SMALL, &measure).text_color(palette.ink3.hsla()).child(code)),
                );
            vec![Leaf::new(plate)]
        }
        Shown::Unavailable(reason, detail) => {
            let words = match reason {
                UnavailableReason::Unsupported => "The local service does not serve this page",
                UnavailableReason::OutOfScope => "This page is outside the selected scope",
            };
            let line = ctx.say(match detail {
                Some(detail) => format!("{words}: {detail}."),
                None => format!("{words}."),
            });
            vec![Leaf::new(
                div()
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Base))
                    .child(text(ty::TITLE, &measure, palette.ink1).child(SharedString::from(what.to_owned())))
                    .child(quiet(line, &measure, palette)),
            )]
        }
    }
}

/// A diagnostic code, mono and copyable.
pub(crate) fn fault_code(code: FaultCode) -> String {
    let name = match code {
        FaultCode::Transport => "TRANSPORT",
        FaultCode::Protocol => "PROTOCOL",
        FaultCode::Missing => "MISSING",
        FaultCode::Persistence => "PERSISTENCE",
        FaultCode::Unsupported => "UNSUPPORTED",
        FaultCode::Cancelled => "CANCELLED",
    };
    format!("READ-{name}")
}
