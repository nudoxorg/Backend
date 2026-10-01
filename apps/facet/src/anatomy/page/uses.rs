//! Who uses it: up to three real call sites, yours first (the caller's name,
//! where, and the hit line cropped around the name, underlined), then the
//! verb rows: how it is used (called by, taken by, held by, used by), how
//! many, how many are yours, the first three names and "and N more in K
//! packages". Every name is a door. Each row is fitted to its line.

use super::{Doors, Geometry, mono_w, named, said};
use crate::anatomy::plan::{Site, Uses, VerbRow};
use crate::hover;
use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, rhythm, scale};
use gpui::{
    AnyElement, ElementId, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, UnderlineStyle, div, px,
};

/// Names a verb row shows before "and N more".
const NAMES: usize = 3;
/// Call sites shown.
const SITES: usize = 3;

/// The body of Who uses it, when there is anything to say.
#[must_use]
pub fn uses(
    uses: &Uses,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> Option<AnyElement> {
    if uses.sites.is_empty() && uses.rows.is_empty() {
        return None;
    }
    let s = geo.scale;
    let mut body = div().flex().flex_col().gap(px(rhythm::GROUP * s));
    if !uses.sites.is_empty() {
        let mut sites = uses.sites.iter().collect::<Vec<_>>();
        sites.sort_by_key(|site| !site.yours);
        let sites = &sites[..sites.len().min(SITES)];
        let columns = geo.margins() && sites.len() > 1;
        let gap = px(20.0 * s);
        let column_w = if columns {
            (geo.col_w - gap * (sites.len() - 1) as f32) / sites.len() as f32
        } else {
            geo.col_w
        };
        let mut strip = if columns {
            div().flex().items_start().gap(gap)
        } else {
            div().flex().flex_col().gap(px(10.0 * s))
        };
        for (n, site) in sites.iter().enumerate() {
            strip = strip.child(site_element(n, site, column_w, m, palette, doors));
        }
        body = body.child(strip);
    }
    if !uses.rows.is_empty() {
        let mut rows = div().flex().flex_col();
        for (n, row) in uses.rows.iter().enumerate() {
            rows = rows.child(verb_row(n, row, geo, m, palette, doors));
        }
        body = body.child(rows);
    }
    Some(body.into_any_element())
}

/// A call site: the caller, where, and its line cropped to the column with
/// the name kept in view and underlined in periwinkle.
fn site_element(
    n: usize,
    site: &Site,
    width: gpui::Pixels,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let s = m.scale();
    let key = format!("page-site-{n}");
    doors.say(&site.caller);
    let (mm, pal, caller, name_key) = (*m, *palette, site.caller.clone(), format!("{key}-caller"));
    let rest = if site.yours {
        palette.mint.base
    } else {
        palette.ink1
    };
    let caller = named(
        format!("{key}-door"),
        &site.caller,
        site.link.as_deref(),
        palette.f_call.hue.hsla(),
        doors,
        move |lit| {
            said(
                name_key,
                caller,
                scale::MONO_NAME,
                hover::ink(rest, lit, &pal),
                &mm,
            )
        },
    );
    let mut column = div()
        .w(width)
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.0 * s))
        .child(caller);
    if !site.place.is_empty() {
        doors.say(&site.place);
        column = column.child(said(
            format!("{key}-place"),
            site.place.clone(),
            scale::LABEL,
            palette.ink3,
            m,
        ));
    }
    if let Some(code) = &site.code {
        // Crop to what fits, keeping the name in view: an ellipsis on the
        // side that was cut.
        let fits =
            ((f32::from(width) / (scale::LABEL_MONO.size * 0.6 * s)).floor() as usize).max(8);
        let chars = code.chars().collect::<Vec<_>>();
        let (from, to) = site.mark.map_or((0, 0), |(a, b)| {
            (
                code.get(..a as usize).map_or(0, |t| t.chars().count()),
                code.get(..b as usize).map_or(0, |t| t.chars().count()),
            )
        });
        let (start, end) = if chars.len() <= fits {
            (0, chars.len())
        } else {
            let keep = fits - 2;
            let start = ((from + to) / 2)
                .saturating_sub(keep / 2)
                .min(chars.len() - keep);
            (start, start + keep)
        };
        let prefix = if start > 0 { "…" } else { "" };
        let text = format!(
            "{prefix}{}{}",
            chars[start..end].iter().collect::<String>(),
            if end < chars.len() { "…" } else { "" }
        );
        let mut highlights = Vec::new();
        if to > from && from >= start && to <= end {
            let a = prefix.len()
                + chars[start..from]
                    .iter()
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
            let b = a + chars[from..to].iter().map(|c| c.len_utf8()).sum::<usize>();
            highlights.push((
                a..b,
                HighlightStyle {
                    color: Some(palette.ink1.hsla()),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.5),
                        color: Some(palette.peri.base.hsla()),
                        wavy: false,
                    }),
                    ..HighlightStyle::default()
                },
            ));
        }
        doors.say(&text);
        let shared = SharedString::from(text.clone());
        column = column.child(probe::text(
            ElementId::Name(SharedString::from(format!("{key}-code"))),
            shared.clone(),
            m.role(scale::LABEL_MONO),
            1.0,
            TextOverflow::Clip,
            div()
                .set(scale::LABEL_MONO, m)
                .text_color(palette.ink3.hsla())
                .whitespace_nowrap()
                .child(StyledText::new(shared).with_highlights(highlights)),
        ));
    }
    column.into_any_element()
}

/// One verb row, fitted: the names that fit, then "and N more".
fn verb_row(
    n: usize,
    row: &VerbRow,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let s = m.scale();
    let key = format!("page-verb-{n}");
    doors.say(&row.verb);
    let verb_w = px(96.0 * s);
    let mut line = div()
        .h(px(rhythm::ROW_PITCH * s))
        .flex()
        .items_center()
        .gap(px(10.0 * s))
        .child(div().w(verb_w).flex_none().flex().justify_end().child(said(
            format!("{key}-verb"),
            row.verb.clone(),
            scale::LABEL,
            palette.ink3,
            m,
        )))
        .child(said(
            format!("{key}-count"),
            row.count.to_string(),
            scale::LABEL_MONO,
            palette.ink2,
            m,
        ));
    doors.say(&row.count.to_string());
    if row.yours > 0 {
        let words = format!("{} yours", row.yours);
        doors.say(&words);
        line = line.child(said(
            format!("{key}-yours"),
            words,
            scale::LABEL_MONO,
            palette.mint.base,
            m,
        ));
    }
    // Fit the names: each costs its width and a gap.
    let mut room = geo.col_w - verb_w - px(120.0 * s);
    let mut shown = 0;
    for (q, (name, link, yours)) in row.names.iter().take(NAMES).enumerate() {
        let w = mono_w(name.chars().count(), scale::MONO, s) + px(14.0 * s);
        if w > room {
            break;
        }
        room -= w;
        shown += 1;
        doors.say(name);
        let (mm, pal, label, name_key) = (*m, *palette, name.clone(), format!("{key}-name-{q}"));
        let rest = if *yours {
            palette.mint.base
        } else {
            palette.ink1
        };
        line = line.child(named(
            format!("{key}-name-{q}-door"),
            name,
            link.as_deref(),
            palette.f_call.hue.hsla(),
            doors,
            move |lit| {
                said(
                    name_key,
                    label,
                    scale::MONO,
                    hover::ink(rest, lit, &pal),
                    &mm,
                )
            },
        ));
    }
    let more = (row.count as usize).saturating_sub(shown);
    if more > 0 {
        let words = if row.packages > 1 {
            format!("and {more} more in {} packages", row.packages)
        } else {
            format!("and {more} more")
        };
        doors.say(&words);
        line = line.child(said(
            format!("{key}-more"),
            words,
            scale::LABEL,
            palette.ink3,
            m,
        ));
    }
    line.into_any_element()
}
