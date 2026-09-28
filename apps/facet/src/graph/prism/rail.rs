//! Bounded native overflow plan and exact measured projection.
use super::super::{
    camera::View,
    model::{Kind, NodeId},
    scene::{EdgeFamily, Scene},
};
use super::{
    Head, Prism, PrismFrame, Slot, SlotKey, ease, fit_columns, layout_with_room, thread_tone,
};
use crate::motion::Camera;
use crate::semantics::{self, Column};
use gpui::{Bounds, SharedString, Window, point, px, size};

/// One bounded native relation-rail child. Only real Row keys navigate.
#[derive(Clone, Debug)]
pub(crate) enum RailEntry {
    Head {
        word: semantics::Word,
        more: usize,
    },
    Row {
        word: semantics::Word,
        key: Option<SlotKey>,
        kind: Option<Kind>,
        text: SharedString,
        note: Option<SharedString>,
        more: usize,
    },
}
/// Exclusive overflow presentation, prepared from the same semantic columns.
#[derive(Clone, Debug)]
pub(crate) struct RailPlan {
    pub owner: NodeId,
    pub viewport: Bounds<gpui::Pixels>,
    pub entries: Vec<RailEntry>,
    pub e: f32,
}
impl RailPlan {
    pub(crate) fn child_index(&self, key: SlotKey) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| matches!(entry, RailEntry::Row { key: Some(k), .. } if *k == key))
    }
}
/// Actual native child geometry from this layout transaction.
#[derive(Clone, Debug)]
pub(crate) struct RailRowBounds {
    pub entry_index: usize,
    pub row: Bounds<gpui::Pixels>,
    pub proxy: Option<Bounds<gpui::Pixels>>,
}
#[derive(Clone, Debug)]
pub(crate) struct RailGeometry {
    pub owner: NodeId,
    pub viewport: Bounds<gpui::Pixels>,
    pub rows: Vec<RailRowBounds>,
}

/// Preserve a family's hue when it can carry normal text; otherwise the
/// relation word stays readable and its strand retains the semantic hue.
pub(crate) fn rail_head_tone(
    word: semantics::Word,
    palette: &crate::tokens::Palette,
) -> crate::tokens::Tone {
    let family = thread_tone(palette, EdgeFamily::of_word(word).slot());
    if text_contrast(family, palette.g0) >= 4.5 {
        family
    } else {
        palette.ink1
    }
}

fn text_contrast(ink: crate::tokens::Tone, ground: crate::tokens::Tone) -> f32 {
    let ink = ink.rgba();
    let ground = ground.rgba();
    let linear = |channel: f32| {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = |r, g, b| 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    let foreground = luminance(
        ink.color.red * ink.alpha + ground.color.red * (1.0 - ink.alpha),
        ink.color.green * ink.alpha + ground.color.green * (1.0 - ink.alpha),
        ink.color.blue * ink.alpha + ground.color.blue * (1.0 - ink.alpha),
    );
    let background = luminance(ground.color.red, ground.color.green, ground.color.blue);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

/// Right-side reading space with actual chrome removed. The interval scan is
/// bounded by chrome count, not world size, and never guesses a Find height.
fn rail_viewport(
    view: &View,
    occupied: Option<Bounds<gpui::Pixels>>,
    reserved: &[Bounds<gpui::Pixels>],
) -> Option<Bounds<gpui::Pixels>> {
    let free = Scene::free_view(view, occupied);
    let (anchor, _) = Scene::focus_anchor(&free);
    let x0 = (anchor + 64.0)
        .min(free.x + free.w - 96.0)
        .max(free.x + 12.0);
    let x1 = free.x + free.w - 12.0;
    let top = free.y + 12.0;
    let bottom = free.y + free.h - 12.0;
    if ![x0, x1, top, bottom].iter().all(|x| x.is_finite()) || x1 <= x0 || bottom <= top {
        return None;
    }
    let mut blocked: Vec<(f32, f32)> = reserved
        .iter()
        .filter_map(|b| {
            let a = f32::from(b.origin.x);
            let c = a + f32::from(b.size.width);
            let y = f32::from(b.origin.y);
            let z = y + f32::from(b.size.height);
            if ![a, c, y, z].iter().all(|v| v.is_finite())
                || c <= x0
                || a >= x1
                || z <= top
                || y >= bottom
            {
                return None;
            }
            Some(((y - 6.0).max(top), (z + 6.0).min(bottom)))
        })
        .collect();
    blocked.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut start = top;
    let mut best = (top, top);
    for (a, b) in blocked {
        if a - start > best.1 - best.0 {
            best = (start, a);
        }
        start = start.max(b);
    }
    if bottom - start > best.1 - best.0 {
        best = (start, bottom);
    }
    if best.1 <= best.0 {
        return None;
    }
    Some(Bounds {
        origin: point(px(x0), px(best.0)),
        size: size(px(x1 - x0), px(best.1 - best.0)),
    })
}

/// Full bounded rows survive whenever compact columns would lose a group's
/// only real navigation action. Comfortable ordinary columns stay unchanged.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn rail_plan(
    prism: &Prism,
    view: &View,
    occupied: Option<Bounds<gpui::Pixels>>,
    reserved: &[Bounds<gpui::Pixels>],
    text_scale: f32,
) -> Option<RailPlan> {
    let free = Scene::free_view(view, occupied);
    let top = free.y + if free.y > view.y { 24.0 } else { 70.0 };
    let height = (free.y + free.h - 28.0 - top).max(0.0);
    let scale = text_scale.max(1.0);
    let loses_action = |cols: &[Column]| {
        let fit = fit_columns(cols, height, scale);
        cols.iter().zip(&fit.columns).any(|(original, fitted)| {
            original.rows.iter().any(|r| r.node.is_some())
                && !fitted.rows.iter().any(|r| r.node.is_some())
        })
    };
    let needs = if Scene::prism_narrow(&free) {
        let all: Vec<Column> = prism.left.iter().chain(&prism.right).cloned().collect();
        loses_action(&all)
    } else {
        loses_action(&prism.left) || loses_action(&prism.right)
    };
    if !needs {
        return None;
    }
    let viewport = rail_viewport(view, occupied, reserved)?;
    let mut entries = Vec::new();
    for (side, columns) in [(-1, &prism.left), (1, &prism.right)] {
        for c in columns {
            entries.push(RailEntry::Head {
                word: c.word,
                more: 0,
            });
            for row in &c.rows {
                entries.push(RailEntry::Row {
                    word: c.word,
                    key: row.node.map(|node| SlotKey {
                        node,
                        side,
                        word: c.word,
                    }),
                    kind: row.kind,
                    text: row.text.clone().into(),
                    note: row.note.clone().map(Into::into),
                    more: 0,
                });
            }
            if c.more > 0 {
                entries.push(RailEntry::Row {
                    word: c.word,
                    key: None,
                    kind: None,
                    text: format!("and {} more", c.more).into(),
                    note: None,
                    more: c.more,
                });
            }
        }
    }
    Some(RailPlan {
        owner: prism.node,
        viewport,
        entries,
        e: ease(prism.g.clamp(0.0, 1.0)),
    })
}

fn bounds_rect(b: Bounds<gpui::Pixels>) -> [f32; 4] {
    let x = f32::from(b.origin.x);
    let y = f32::from(b.origin.y);
    [
        x,
        y,
        x + f32::from(b.size.width),
        y + f32::from(b.size.height),
    ]
}
fn visible_rect(rect: [f32; 4], viewport: [f32; 4]) -> Option<[f32; 4]> {
    if !rect.iter().chain(&viewport).all(|v| v.is_finite()) {
        return None;
    }
    let clipped = [
        rect[0].max(viewport[0]),
        rect[1].max(viewport[1]),
        rect[2].min(viewport[2]),
        rect[3].min(viewport[3]),
    ];
    (clipped.iter().all(|v| v.is_finite()) && clipped[2] > clipped[0] && clipped[3] > clipped[1])
        .then_some(clipped)
}

/// Native geometry is authoritative for the exclusive rail presentation.
/// Every semantic slot survives for keyboard identity; only actual visible
/// intersections become pointer/curve targets. The native child owns its text
/// and proxy paint, so this frame only paints source/visible strands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_with_rail(
    prism: &Prism,
    scene: &Scene,
    view: &View,
    cam: &Camera,
    text_scale: f32,
    occupied: Option<Bounds<gpui::Pixels>>,
    rail: Option<(&RailPlan, &RailGeometry)>,
    window: &Window,
) -> PrismFrame {
    let Some((plan, geometry)) =
        rail.filter(|(plan, geometry)| plan.owner == prism.node && geometry.owner == prism.node)
    else {
        return layout_with_room(prism, scene, view, cam, text_scale, occupied, window);
    };
    let room = bounds_rect(geometry.viewport);
    let lay = &*scene.layout;
    let (fx, fy) = view.to_screen(cam, lay.x[prism.node as usize], lay.y[prism.node as usize]);
    let mut slots = Vec::new();
    let mut heads = Vec::new();
    for (i, entry) in plan.entries.iter().enumerate() {
        let measured = geometry.rows.iter().find(|row| row.entry_index == i);
        match entry {
            RailEntry::Head { word, more } => {
                if let Some(label) = measured.and_then(|m| visible_rect(bounds_rect(m.row), room)) {
                    heads.push(Head {
                        word: *word,
                        text: word.text(),
                        label_text: word.text().into(),
                        x: label[0],
                        y: (label[1] + label[3]) * 0.5,
                        side: 1,
                        more: *more,
                        label,
                    });
                }
            }
            RailEntry::Row {
                word,
                key,
                kind,
                text,
                note,
                more,
            } => {
                let row = measured.map(|m| bounds_rect(m.row));
                let proxy = measured.and_then(|m| m.proxy).map(bounds_rect);
                let visible_proxy = proxy.and_then(|r| visible_rect(r, room));
                let target = visible_proxy.or(row);
                let (x, y) = target.map_or((room[0], room[1]), |r| {
                    ((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5)
                });
                let label = row.and_then(|r| visible_rect(r, room));
                let (hx, hy) = key.map_or((x, y), |key| {
                    view.to_screen(cam, lay.x[key.node as usize], lay.y[key.node as usize])
                });
                slots.push(Slot {
                    word: *word,
                    key: *key,
                    node: key.map(|k| k.node),
                    kind: *kind,
                    text: text.clone(),
                    note: note.clone(),
                    side: 1,
                    x,
                    y,
                    px: x,
                    py: y,
                    proxy_visible: visible_proxy.is_some(),
                    proxy_radius: visible_proxy
                        .map_or(0.0, |r| ((r[2] - r[0]).min(r[3] - r[1])) * 0.5),
                    hx,
                    hy,
                    home_on: false,
                    label,
                    more: *more,
                });
            }
        }
    }
    PrismFrame {
        fx,
        fy,
        slots,
        heads,
        e: plan.e,
        narrow: true,
        node: prism.node,
        room,
        rail: true,
    }
}

#[cfg(test)]
mod rail_tests {
    use super::*;
    use crate::semantics::PrismRow;

    #[test]
    fn family_head_text_is_readable_at_native_and_normal_column_opacities() {
        use crate::tokens::{ABYSS, GLACIER};
        for palette in [&ABYSS, &GLACIER] {
            for word in [
                semantics::Word::MadeOf,
                semantics::Word::MadeBy,
                semantics::Word::Is,
                semantics::Word::Calls,
            ] {
                let color = rail_head_tone(word, palette);
                assert!(text_contrast(color, palette.g0) >= 4.5);
                assert!(text_contrast(color.alpha(0.75), palette.g0) >= 4.5);
                let semantic = thread_tone(palette, EdgeFamily::of_word(word).slot());
                if text_contrast(semantic, palette.g0) >= 4.5 {
                    assert_eq!(color, semantic, "readable family hues remain intact");
                }
            }
        }
    }
    fn column(word: semantics::Word, base: u32) -> Column {
        Column {
            word,
            rows: (0..2)
                .map(|i| PrismRow {
                    node: Some(base + i),
                    kind: Some(Kind::Struct),
                    text: format!("row{base}-{i}").into(),
                    note: None,
                })
                .collect(),
            more: 7,
        }
    }
    fn prism() -> Prism {
        use semantics::Word;
        Prism {
            node: 0,
            left: vec![column(Word::MadeOf, 10), column(Word::MadeBy, 20)],
            right: vec![
                column(Word::TakenBy, 30),
                column(Word::HeldBy, 40),
                column(Word::CallsIt, 50),
            ],
            g: 1.0,
            target: 1.0,
        }
    }
    #[test]
    fn overflow_retains_full_identity_and_honest_remainders() {
        let prism = prism();
        let view = View {
            x: 0.0,
            y: 0.0,
            w: 480.0,
            h: 600.0,
        };
        let card = Bounds {
            origin: point(px(12.0), px(290.0)),
            size: size(px(456.0), px(270.0)),
        };
        let find = Bounds {
            origin: point(px(16.0), px(17.0)),
            size: size(px(448.0), px(56.0)),
        };
        let plan = rail_plan(&prism, &view, Some(card), &[card, find], 2.0)
            .expect("native constrained rail");
        assert_eq!(plan.entries.len(), 20);
        assert_eq!(plan.e, 1.0);
        assert!(f32::from(plan.viewport.origin.y) >= 79.0);
        assert!(f32::from(plan.viewport.bottom()) <= 278.0);
        let mut real = 0;
        let mut remainder = 0;
        for entry in &plan.entries {
            if let RailEntry::Row { key, more, .. } = entry {
                remainder += more;
                if let Some(key) = key {
                    real += 1;
                    assert!(plan.child_index(*key).is_some());
                }
            }
        }
        assert_eq!((real, remainder), (10, 35));
        let larger = View { h: 900.0, ..view };
        let card2 = Bounds {
            origin: point(px(12.0), px(610.0)),
            size: card.size,
        };
        if let Some(p2) = rail_plan(&prism, &larger, Some(card2), &[find, card2], 2.0) {
            let keys = |p: &RailPlan| {
                p.entries
                    .iter()
                    .filter_map(|e| {
                        if let RailEntry::Row { key, .. } = e {
                            *key
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(keys(&plan), keys(&p2));
        }
    }
    #[test]
    fn comfortable_columns_and_text_only_groups_do_not_get_a_rail() {
        let mut prism = prism();
        let view = View {
            x: 0.0,
            y: 0.0,
            w: 1440.0,
            h: 900.0,
        };
        let card = Bounds {
            origin: point(px(1080.0), px(20.0)),
            size: size(px(340.0), px(175.0)),
        };
        assert!(rail_plan(&prism, &view, Some(card), &[card], 1.0).is_none());
        for c in prism.left.iter_mut().chain(&mut prism.right) {
            for row in &mut c.rows {
                row.node = None;
            }
        }
        let small = View {
            w: 480.0,
            h: 150.0,
            ..view
        };
        assert!(rail_plan(&prism, &small, None, &[], 2.0).is_none());
    }
    #[test]
    fn visible_native_intersections_are_exact_and_contained() {
        let viewport = [198.0, 79.0, 468.0, 266.0];
        assert_eq!(visible_rect([f32::NAN; 4], viewport), None);
        assert_eq!(visible_rect([200.0, 40.0, 450.0, 70.0], viewport), None);
        assert_eq!(
            visible_rect([200.0, 250.0, 450.0, 300.0], viewport),
            Some([200.0, 250.0, 450.0, 266.0])
        );
        for x in (150..550).step_by(7) {
            for y in (0..350).step_by(11) {
                if let Some(r) = visible_rect(
                    [x as f32, y as f32, x as f32 + 80.0, y as f32 + 44.0],
                    viewport,
                ) {
                    assert!(
                        r[0] >= viewport[0]
                            && r[1] >= viewport[1]
                            && r[2] <= viewport[2]
                            && r[3] <= viewport[3]
                    );
                    assert!(r[2] > r[0] && r[3] > r[1]);
                }
            }
        }
    }

    #[test]
    fn hidden_rows_keep_keyboard_identity_without_ghost_or_adjacent_hits() {
        let slot = |node: NodeId, y: f32, label: Option<[f32; 4]>| Slot {
            word: semantics::Word::MadeOf,
            key: Some(SlotKey {
                node,
                side: -1,
                word: semantics::Word::MadeOf,
            }),
            node: Some(node),
            kind: Some(Kind::Struct),
            text: format!("row{node}").into(),
            note: None,
            side: 1,
            x: 220.0,
            y,
            px: 220.0,
            py: y,
            proxy_visible: label.is_some(),
            proxy_radius: 4.2,
            hx: 0.0,
            hy: 0.0,
            home_on: false,
            label,
            more: 0,
        };
        let frame = PrismFrame {
            fx: 134.0,
            fy: 139.0,
            slots: vec![
                slot(10, 101.0, Some([198.0, 79.0, 468.0, 123.0])),
                slot(20, 145.0, Some([198.0, 123.0, 468.0, 167.0])),
                slot(30, 300.0, None),
            ],
            heads: Vec::new(),
            e: 1.0,
            narrow: true,
            node: 0,
            room: [198.0, 79.0, 468.0, 266.0],
            rail: true,
        };
        assert_eq!(
            frame.pick(220.0, 123.0),
            Some(1),
            "adjacent boundary belongs to the next native row"
        );
        assert_eq!(
            frame.pick(220.0, 240.0),
            None,
            "hidden row cannot acquire a ghost target"
        );
        assert_eq!(frame.walk(Some(1), 0, 1), Some(2));
        assert_eq!(
            frame.locate(frame.slots[2].key.expect("hidden key")),
            Some(2)
        );
        assert_eq!(frame.slots[2].key.expect("semantic key").side, -1);
    }
}
