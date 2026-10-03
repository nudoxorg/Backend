//! Loading, fault and unavailable pages, said once, in place.
//!
//! A page that has a value shows it, even while a newer one is fetched (a
//! refresh never blanks the screen). Without one, it says exactly one of:
//! still arriving (a comb of hairlines where the words will land), stopped
//! (one coral plate: what happened, the code, one way to try again), or not
//! served (one quiet line).

use super::{Ctx, Leaf};
use crate::core::{ErrorValue, FaultCode, Resource, ResourceAdmission, ResourceTerminal, UnavailableReason, VersionedRoot, admit_resource};
use crate::model::pages::PageKey;
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{pending, quiet, text};
use crate::shell::reader::Reader;
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{Set as _, Space};
use gpui::{Context, InteractiveElement, ParentElement, SharedString, Styled, div, px};
use std::rc::Rc;

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
        ResourceTerminal::Complete | ResourceTerminal::Partial => Shown::Pending,
    }
}

/// What an exact typed pane may paint. An earlier value is visual evidence,
/// never the current owner's action capability. The embedded identity check
/// is mandatory: a map key alone cannot certify saved or retained content.
pub(crate) enum DisplayEvidence<'a, T> {
    Current(&'a T),
    Earlier { value: &'a T, at: Option<VersionedRoot>, failed: bool },
    Missing(Shown<'a, T>),
    WrongIdentity,
}

pub(crate) fn display_evidence<T>(
    resource: &Resource<T>,
    root: VersionedRoot,
    owner_serving: bool,
    matches_address: impl Fn(&T) -> bool,
) -> DisplayEvidence<'_, T> {
    let selected = match admit_resource(resource, root, owner_serving) {
        ResourceAdmission::Current(value) => Some((value, false, false)),
        ResourceAdmission::Retained { value, .. } => Some((value, true, false)),
        ResourceAdmission::Failed { retained: Some(value), .. } => Some((value, true, true)),
        ResourceAdmission::Pending(_) | ResourceAdmission::Failed { retained: None, .. } => None,
    };
    match selected {
        Some((value, _, _)) if !matches_address(value) => DisplayEvidence::WrongIdentity,
        Some((value, false, _)) => DisplayEvidence::Current(value),
        Some((value, true, failed)) => DisplayEvidence::Earlier {
            value,
            at: resource.value_root(),
            failed,
        },
        None => DisplayEvidence::Missing(shown(resource)),
    }
}

pub(crate) fn earlier_notice<T>(evidence: &DisplayEvidence<'_, T>) -> Option<String> {
    let DisplayEvidence::Earlier { at, failed, .. } = evidence else { return None };
    let origin = match at {
        Some(root) if !root.is_unserved() => format!("Earlier producer reading at {root}"),
        _ => "Saved or earlier reading awaiting a producer".to_owned(),
    };
    Some(if *failed {
        format!("{origin}. The current read failed; this content is read-only.")
    } else {
        format!("{origin}. This content is read-only until the current producer checks it.")
    })
}

#[cfg(test)]
mod display_tests {
    use super::{DisplayEvidence, display_evidence, earlier_notice};
    use crate::core::{FaultCode, Resource, VersionedRoot};

    fn root(epoch: u64) -> VersionedRoot {
        VersionedRoot::synthetic(
            backend_library::view_state_root(&[("display".into(), "exact route".into())]),
            epoch,
        )
    }

    #[test]
    fn a_retained_value_is_display_evidence_only_after_its_embedded_identity_matches() {
        let value = Resource::loaded_at("package A", root(1));
        let evidence = display_evidence(&value, root(2), true, |value| *value == "package A");
        assert!(matches!(&evidence, DisplayEvidence::Earlier { .. }));
        assert!(earlier_notice(&evidence).is_some_and(|notice| notice.contains("read-only")));
        assert!(matches!(
            display_evidence(&value, root(1), true, |value| *value == "package B"),
            DisplayEvidence::WrongIdentity,
        ));
        assert!(matches!(
            display_evidence(&value, root(1), true, |value| *value == "package A"),
            DisplayEvidence::Current(_),
        ));
    }

    #[test]
    fn failed_current_read_cannot_promote_a_saved_value() {
        let value = Resource::loaded_at("earlier", VersionedRoot::unserved())
            .mark_error(FaultCode::Transport, "owner stopped");
        let evidence = display_evidence(&value, root(3), false, |value| *value == "earlier");
        assert!(matches!(&evidence, DisplayEvidence::Earlier { failed: true, .. }));
        assert!(earlier_notice(&evidence).is_some_and(|notice| notice.contains("failed") && notice.contains("read-only")));
    }
}

/// The page for a resource with no value: `what` names it ("RelationLabel").
pub(crate) fn not_ready<T>(
    shown: &Shown<'_, T>,
    key: &PageKey,
    what: &str,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
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
            let retry_key = key.clone();
            let id: SharedString = format!("retry-{key:?}").into();
            let act: Act = Rc::new(move |_, cx| links.retry(retry_key.clone(), cx));
            let act = ctx.native_local_action(act, cx);
            ctx.targets.push(Target { id: id.clone(), label: "Try again".into(), act: Rc::clone(&act), peek: None, source: None });
            let focus = ctx.native_handle(&id, cx);
            let mut control = facet::controls::button(id.clone(), "Try again", &measure)
                .primary().on_click(move |window, cx| act(window, cx));
            if let Some(focus) = focus { control = control.focus_handle(focus); }
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
                            ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(control)),
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
