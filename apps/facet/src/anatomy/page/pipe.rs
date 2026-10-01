//! The pipe: a callable drawn as a chamfered plate in its hue. Its inputs
//! are ports on the left (a name and a type in words, one row each) whose
//! lines converge into the plate; what it is called on enters from above
//! with what the call does to it; what it gives back leaves to the right
//! with a chevron; each way it can fail drops below in coral. When the
//! margins carry the edges, each port's line starts at the reader's left
//! edge: an input is literally an in-edge. The strokes are [`super::ink`]'s.

use super::{Doors, Geometry, anchor, at, mono_w, said, ty_lit, words_w};
use crate::anatomy::page::Anchors;
use crate::anatomy::plan::{Callable, Effect, Part, SectionId};
use crate::hover::Lit;
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, IntoElement, ParentElement, Pixels, Styled, div, px};
use std::rc::Rc;

const fn effect_words(effect: Effect) -> &'static str {
    match effect {
        Effect::Reads | Effect::None => "reads it",
        Effect::Changes => "changes it",
        Effect::UsesUp => "uses it up",
        Effect::Makes => "makes one",
    }
}

/// The pipe for `callable`, its plate carrying `name`.
pub(super) fn pipe(
    callable: &Callable,
    name: &str,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let s = geo.scale;
    let pitch = px(rhythm::ROW_PITCH * s);
    let xray = m.reveal().xray;
    // The ports' column: as wide as its widest port.
    let port_w = callable
        .ports
        .iter()
        .map(|port| {
            mono_w(port.name.chars().count(), scale::MONO, s)
                + px(12.0 * s)
                + words_w(port.ty.plain().chars().count(), scale::BODY, s)
                + px(15.0 * s)
        })
        .fold(px(0.0), Pixels::max);
    let plate_w = mono_w(name.chars().count(), scale::MONO_NAME, s) + px(28.0 * s);
    let bend = px(44.0 * s);
    let plate_x = if callable.ports.is_empty() {
        px(0.0)
    } else {
        port_w + bend
    };

    // What it is called on, above the plate.
    let mut body = div().flex().flex_col();
    if let Some(receiver) = &callable.receiver {
        let words = effect_words(receiver.effect);
        doors.say("on");
        doors.say(&receiver.ty.plain());
        let mut row = div()
            .h(px(20.0 * s))
            .flex()
            .items_center()
            .gap(px(6.0 * s))
            .pl(plate_x);
        row = row.child(said("page-pipe-on", "on", scale::LABEL, palette.ink3, m));
        if !receiver.name.is_empty() && receiver.name != "self" {
            row = row.child(said(
                "page-pipe-on-name",
                receiver.name.clone(),
                scale::LABEL_MONO,
                palette.ink2,
                m,
            ));
        }
        row = row.child(ty_lit(
            "page-pipe-on-type",
            &receiver.ty,
            m,
            palette,
            xray,
            Lit::Rest,
        ));
        // What the call does to it, when the language says.
        if receiver.effect != Effect::None {
            doors.say(words);
            row = row.child(said(
                "page-pipe-on-effect",
                words,
                scale::LABEL,
                if receiver.effect == Effect::Changes {
                    palette.ink2
                } else {
                    palette.ink3
                },
                m,
            ));
        }
        body = body.child(anchor(
            at(SectionId::Spec, Part::Receiver),
            anchors,
            row.mb(px(10.0 * s)),
        ));
    }

    // Ports, the plate, and what it gives back.
    let rows = callable.ports.len().max(1);
    let mut ports = div().flex().flex_col().w(port_w).flex_none();
    for (n, port) in callable.ports.iter().enumerate() {
        doors.say(&port.name);
        doors.say(&port.ty.plain());
        let row = div()
            .h(pitch)
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.0 * s))
            .child(said(
                format!("page-port-{n}-name"),
                port.name.clone(),
                scale::MONO,
                palette.ink1,
                m,
            ))
            .child(ty_lit(
                &format!("page-port-{n}-type"),
                &port.ty,
                m,
                palette,
                xray,
                Lit::Rest,
            ));
        ports = ports.child(anchor(
            at(SectionId::Spec, Part::Row(n as u16)),
            anchors,
            row,
        ));
    }
    doors.say(name);
    let plate = div()
        .h(px(28.0 * s))
        .w(plate_w)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(said(
            "page-pipe-name",
            name.to_owned(),
            scale::MONO_NAME,
            palette.ink0,
            m,
        ));
    let mut gives = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.0 * s))
        .pl(px(36.0 * s))
        .min_w_0();
    match &callable.gives {
        Some(ty) => {
            doors.say("gives");
            doors.say(&ty.plain());
            gives = gives
                .child(said(
                    "page-pipe-gives",
                    "gives",
                    scale::LABEL,
                    palette.ink3,
                    m,
                ))
                .child(ty_lit(
                    "page-pipe-gives-type",
                    ty,
                    m,
                    palette,
                    xray,
                    Lit::Rest,
                ));
        }
        None => {
            let words = "gives nothing back";
            doors.say(words);
            gives = gives.child(said(
                "page-pipe-gives",
                words,
                scale::LABEL,
                palette.ink3,
                m,
            ));
        }
    }
    let mut main = div().h(pitch * rows as f32).flex().items_center();
    if !callable.ports.is_empty() {
        main = main.child(ports).child(div().w(bend).flex_none());
    }
    main = main
        .child(anchor(at(SectionId::Spec, Part::Plate), anchors, plate))
        .child(anchor(at(SectionId::Spec, Part::Gives), anchors, gives));
    body = body.child(main);

    // Each way it can fail, dropping below the plate in coral.
    if !callable.drops.is_empty() {
        let mut drops = div()
            .flex()
            .flex_col()
            .mt(px(4.0 * s))
            .pl(plate_x + px(34.0 * s));
        for (n, drop) in callable.drops.iter().enumerate() {
            doors.say(drop.verb.words());
            let mut row = div()
                .h(pitch)
                .flex()
                .items_center()
                .gap(px(6.0 * s))
                .child(said(
                    format!("page-drop-{n}-verb"),
                    drop.verb.words(),
                    scale::LABEL,
                    palette.coral.base,
                    m,
                ));
            if let Some(ty) = &drop.ty {
                doors.say(&ty.plain());
                row = row.child(ty_lit(
                    &format!("page-drop-{n}-type"),
                    ty,
                    m,
                    palette,
                    xray,
                    Lit::Rest,
                ));
            }
            drops = drops.child(anchor(
                at(SectionId::Spec, Part::Drop(n as u16)),
                anchors,
                row,
            ));
        }
        body = body.child(drops);
    }
    // Qualifiers (`waits (async)`): quiet words under the plate.
    if !callable.flags.is_empty() {
        let words = callable.flags.join(" · ");
        doors.say(&words);
        body = body.child(
            div()
                .pl(plate_x)
                .h(px(20.0 * s))
                .flex()
                .items_center()
                .child(said(
                    "page-pipe-flags",
                    words,
                    scale::LABEL,
                    palette.ink3,
                    m,
                )),
        );
    }
    body.into_any_element()
}
