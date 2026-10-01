//! What can go wrong: a coral tree off the spine, one branch per way it
//! can fail. A branch is who fails (a member, when not the page itself),
//! how (fails with, returns, throws, panics), with what, and the docs'
//! words, when they say. The branches' strokes are [`super::ink`]'s.

use super::{Doors, Geometry, anchor, at, said, ty_lit};
use crate::anatomy::page::Anchors;
use crate::anatomy::plan::{FailBranch, Part, SectionId};
use crate::hover::Lit;
use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, ElementId, IntoElement, ParentElement, SharedString, Styled, div, px};
use std::rc::Rc;

/// The branches, when there are any.
#[must_use]
pub fn fails(
    branches: &[FailBranch],
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> Option<AnyElement> {
    if branches.is_empty() {
        return None;
    }
    let s = geo.scale;
    let mut body = div().flex().flex_col().gap(px(4.0 * s));
    for (n, branch) in branches.iter().enumerate() {
        let key = format!("page-fail-{n}");
        let mut head = div()
            .min_h(px(rhythm::ROW_PITCH * s))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(8.0 * s));
        if let Some(subject) = &branch.subject {
            doors.say(subject);
            head = head.child(said(
                format!("{key}-subject"),
                subject.clone(),
                scale::MONO_NAME,
                palette.ink1,
                m,
            ));
        }
        doors.say(branch.verb.words());
        head = head.child(said(
            format!("{key}-verb"),
            branch.verb.words().trim_start_matches("or "),
            scale::LABEL,
            palette.coral.base,
            m,
        ));
        if let Some(ty) = &branch.ty {
            doors.say(&ty.plain());
            head = head.child(ty_lit(
                &format!("{key}-type"),
                ty,
                m,
                palette,
                m.reveal().xray,
                Lit::Rest,
            ));
        }
        let mut row = div().flex().flex_col().child(anchor(
            at(SectionId::Fails, Part::Row(n as u16)),
            anchors,
            head,
        ));
        if let Some(words) = branch
            .words
            .as_ref()
            .filter(|words| !words.trim().is_empty())
        {
            doors.say(words);
            row = row.child(
                div()
                    .max_w(geo.col_w - px(24.0 * s))
                    .pb(px(4.0 * s))
                    .child(probe::text(
                        ElementId::Name(SharedString::from(format!("{key}-words"))),
                        SharedString::from(words.clone()),
                        m.role(scale::BODY),
                        1.0,
                        TextOverflow::Wrap,
                        div()
                            .set(scale::BODY, m)
                            .text_color(palette.ink2.hsla())
                            .child(words.clone()),
                    )),
            );
        }
        body = body.child(row);
    }
    Some(body.into_any_element())
}
