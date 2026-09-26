//! Focus gathers the prism (gui-plan §8.2, app.js `buildPrism`,
//! `layoutPrism`, `drawPrism`): the focused symbol's relations fly out of
//! their homes in the map into two named columns beside it — left, what it
//! comes from; right, what it goes into — 22 px rows, at most six per group
//! plus "and N more", each proxy tethered home by a faint dashed line.
//! Below 900 px of free width the columns become one.
//!
//! The groups and their labels come from
//! [`semantics::relations_of`](crate::semantics::relations_of) and
//! [`semantics::prism`](crate::semantics::prism), the one rule the page uses
//! too.

use super::camera::{View, smooth};
use super::draw::{Look, Strokes, cubic_to, role, tone};
use super::model::{Kind, NodeId, World};
use super::scene::Scene;
use crate::data::text::{shape, shape_fit};
use crate::motion::Camera;
use crate::paint::geom::{Fill, pt};
use crate::semantics::{self, Column};
use crate::tokens::TypeRole;
use gpui::{App, Bounds, SharedString, Window, fill, point, px, size};

/// Rows per group before "and N more".
pub const PER_GROUP: usize = 6;
const ROW: f32 = 22.0;
const HEAD: f32 = 22.0;
const GROUP_GAP: f32 = 10.0;

mod roles {
    use super::role;
    use crate::tokens::{Face, TypeRole};
    pub(super) const HEAD: TypeRole = role(Face::Serif, 400.0, 13.0);
    pub(super) const NAME: TypeRole = role(Face::Mono, 500.0, 12.0);
    pub(super) const NAME_BOLD: TypeRole = role(Face::Mono, 600.0, 12.0);
    pub(super) const WHERE: TypeRole = role(Face::Mono, 400.0, 10.5);
    pub(super) const MORE: TypeRole = role(Face::Serif, 400.0, 12.5);
}

fn scaled(r: TypeRole, s: f32) -> TypeRole {
    TypeRole {
        size: r.size * s,
        line: r.line * s,
        ..r
    }
}

/// A gathered (or gathering, or releasing) prism.
#[derive(Clone, Debug)]
pub struct Prism {
    /// The focused symbol.
    pub node: NodeId,
    /// What it comes from.
    pub left: Vec<Column>,
    /// What it goes into.
    pub right: Vec<Column>,
    /// Gather progress, 0 (home) → 1 (in columns).
    pub g: f32,
    /// Where `g` is heading.
    pub target: f32,
}

impl Prism {
    /// The prism of `i`, not yet gathered.
    #[must_use]
    pub fn of(world: &World, i: NodeId) -> Self {
        let groups = semantics::except(semantics::relations_of(world, i), &[]);
        let (left, right) = semantics::prism(world, i, &groups, PER_GROUP);
        Self {
            node: i,
            left,
            right,
            g: 0.0,
            target: 1.0,
        }
    }
}

/// Stable identity of a real prism row, independent of responsive slot order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotKey {
    /// The real symbol.
    pub node: NodeId,
    /// Incoming or outgoing side.
    pub side: i8,
    /// The relation group that contains it.
    pub word: semantics::Word,
}

/// One row of a laid-out prism.
#[derive(Clone, Debug)]
pub struct Slot {
    /// Stable identity; virtual remainder rows are not selectable.
    pub key: Option<SlotKey>,
    /// The symbol (None for "and N more" or a text-only row).
    pub node: Option<NodeId>,
    /// Its kind (the proxy's shape).
    pub kind: Option<Kind>,
    /// The name shown.
    pub text: SharedString,
    /// The quiet note.
    pub note: Option<SharedString>,
    /// -1 left, 1 right.
    pub side: i8,
    /// Its column position (window px).
    pub x: f32,
    /// Its row's centre.
    pub y: f32,
    /// Where the proxy is now.
    pub px: f32,
    /// Where the proxy is now.
    pub py: f32,
    /// Its home in the map (clamped to the view).
    pub hx: f32,
    /// Its home in the map.
    pub hy: f32,
    /// Whether home is on screen.
    pub home_on: bool,
    /// Its measured label box `[x0, y0, x1, y1]`, including notes or "and N more".
    pub label: Option<[f32; 4]>,
    /// "and N more" rows carry N.
    pub more: usize,
}

/// A group head.
#[derive(Clone, Debug)]
pub struct Head {
    /// The group word.
    pub text: &'static str,
    /// Fitted display text, shared by measurement and painting.
    pub label_text: SharedString,
    /// Column x.
    pub x: f32,
    /// Baseline centre.
    pub y: f32,
    /// -1 left, 1 right.
    pub side: i8,
    /// A fully condensed group carries its honest remainder in the heading.
    pub more: usize,
    /// Measured heading bounds, reserved against map labels.
    pub label: [f32; 4],
}

/// A prism laid out for one frame.
#[derive(Clone, Debug)]
pub struct PrismFrame {
    /// The focus, on screen.
    pub fx: f32,
    /// The focus, on screen.
    pub fy: f32,
    /// Every row.
    pub slots: Vec<Slot>,
    /// Every group head.
    pub heads: Vec<Head>,
    /// Eased gather progress.
    pub e: f32,
    /// One column.
    pub narrow: bool,
    /// The focused symbol.
    pub node: NodeId,
    /// Exact free canvas rectangle in window coordinates.
    pub room: [f32; 4],
}

impl PrismFrame {
    /// The slot under window point `(x, y)` (a row's label or its proxy),
    /// only once gathered.
    #[must_use]
    pub fn pick(&self, x: f32, y: f32) -> Option<usize> {
        if self.e < 0.8
            || x < self.room[0]
            || x > self.room[2]
            || y < self.room[1]
            || y > self.room[3]
        {
            return None;
        }
        self.slots.iter().position(|sl| {
            sl.node.is_some()
                && sl.label.is_some_and(|[a, b, c, d]| {
                    (x >= a - 14.0 && x <= c + 4.0 && y >= b - 3.0 && y <= d + 3.0)
                        || (x - sl.px).hypot(y - sl.py) < 9.0
                })
        })
    }

    /// Finds a real row after resize or text-scale changes. A condensed-away
    /// selection becomes None rather than silently selecting a different node.
    #[must_use]
    pub fn locate(&self, key: SlotKey) -> Option<usize> {
        self.slots.iter().position(|slot| slot.key == Some(key))
    }

    /// The selectable rows (real symbols), in slot order.
    #[must_use]
    pub fn real(&self) -> Vec<usize> {
        (0..self.slots.len())
            .filter(|&q| self.slots[q].node.is_some() && self.slots[q].more == 0)
            .collect()
    }

    /// Arrow-key walking (app.js `prismMove`): the first row on that side,
    /// the next row in the same column, or the nearest row across.
    #[must_use]
    pub fn walk(&self, from: Option<usize>, dx: i8, dy: i8) -> Option<usize> {
        let real = self.real();
        if real.is_empty() {
            return None;
        }
        let Some(cur) = from.filter(|q| real.contains(q)) else {
            let side = if dx < 0 { -1 } else { 1 };
            return real
                .iter()
                .copied()
                .find(|&q| self.slots[q].side == side)
                .or(Some(real[0]));
        };
        let here = &self.slots[cur];
        if dy != 0 {
            let same: Vec<usize> = real
                .iter()
                .copied()
                .filter(|&q| self.slots[q].side == here.side)
                .collect();
            let at = same.iter().position(|&q| q == cur).unwrap_or(0);
            let next = (at as isize + isize::from(dy)).clamp(0, same.len() as isize - 1);
            #[allow(clippy::cast_sign_loss)]
            return Some(same[next as usize]);
        }
        let side = if dx < 0 { -1 } else { 1 };
        real.iter()
            .copied()
            .filter(|&q| self.slots[q].side == side)
            .min_by(|&a, &b| {
                (self.slots[a].y - here.y)
                    .abs()
                    .total_cmp(&(self.slots[b].y - here.y).abs())
            })
            .or(Some(cur))
    }
}

/// `(1 − cos πt) / 2`.
fn ease(t: f32) -> f32 {
    (1.0 - (std::f32::consts::PI * t).cos()) / 2.0
}

/// Condensed columns put their remainder in the heading, so every available
/// row buys a real action instead of another remainder line.
struct FittedColumns {
    columns: Vec<Column>,
    inline_more: bool,
}

fn column_height(cols: &[Column], scale: f32, inline_more: bool) -> f32 {
    let height: f32 = cols
        .iter()
        .map(|c| {
            let compact = inline_more || (c.rows.is_empty() && c.more > 0);
            let head = if compact {
                roles::HEAD.line + 2.0
            } else {
                HEAD
            };
            let gap = if compact { 4.0 } else { GROUP_GAP };
            head + ROW * (c.rows.len() + usize::from(c.more > 0 && !compact)) as f32 + gap
        })
        .sum();
    let last_gap = cols.last().map_or(0.0, |c| {
        if inline_more || (c.rows.is_empty() && c.more > 0) {
            4.0
        } else {
            GROUP_GAP
        }
    });
    (height - last_gap).max(0.0) * scale
}

/// Allocate the finite row budget round-robin. Each group receives its first
/// action before any receives a second; ordinary-size groups stay unchanged.
fn fit_columns(cols: &[Column], height: f32, scale: f32) -> FittedColumns {
    if column_height(cols, scale, false) <= height {
        return FittedColumns {
            columns: cols.to_vec(),
            inline_more: false,
        };
    }
    let mut fitted: Vec<_> = cols
        .iter()
        .map(|c| Column {
            word: c.word,
            rows: Vec::new(),
            more: c.more + c.rows.len(),
        })
        .collect();
    let overhead = column_height(&fitted, scale, true);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let budget = ((height - overhead).max(0.0) / (ROW * scale)).floor() as usize;
    let mut left = budget.min(cols.iter().map(|c| c.rows.len()).sum());
    for level in 0..cols.iter().map(|c| c.rows.len()).max().unwrap_or(0) {
        for (original, column) in cols.iter().zip(&mut fitted) {
            // Text-only rows cannot displace a group's only navigable action.
            let row = original
                .rows
                .iter()
                .filter(|row| row.node.is_some())
                .chain(original.rows.iter().filter(|row| row.node.is_none()))
                .nth(level);
            if let Some(row) = row.filter(|_| left > 0) {
                column.rows.push(row.clone());
                column.more -= 1;
                left -= 1;
            }
        }
        if left == 0 {
            break;
        }
    }
    FittedColumns {
        columns: fitted,
        inline_more: true,
    }
}

/// Lays `prism` out for this frame (app.js `layoutPrism`).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn layout(
    prism: &Prism,
    scene: &Scene,
    view: &View,
    cam: &Camera,
    text_scale: f32,
    window: &Window,
) -> PrismFrame {
    layout_with_room(prism, scene, view, cam, text_scale, None, window)
}

/// Lays the prism into the exact canvas room left by the measured focus card.
/// Camera projection always uses the full view, so paint and picking agree.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn layout_with_room(
    prism: &Prism,
    scene: &Scene,
    view: &View,
    cam: &Camera,
    text_scale: f32,
    occupied: Option<Bounds<gpui::Pixels>>,
    window: &Window,
) -> PrismFrame {
    let lay = &*scene.layout;
    let (fx, fy) = view.to_screen(cam, lay.x[prism.node as usize], lay.y[prism.node as usize]);
    let free = Scene::free_view(view, occupied);
    let room = [free.x, free.y, free.x + free.w, free.y + free.h];
    let avail = free.w;
    let narrow = Scene::prism_narrow(&free);
    let gap = if narrow {
        0.0
    } else {
        (avail * 0.2).clamp(150.0, 250.0)
    };
    let mut slots = Vec::new();
    let mut heads = Vec::new();
    let row_height = ROW * text_scale.max(1.0);
    let head_height = HEAD * text_scale.max(1.0);
    let group_gap = GROUP_GAP * text_scale.max(1.0);
    let name_role = scaled(roles::NAME, text_scale);
    let where_role = scaled(roles::WHERE, text_scale);
    let ink = gpui::black();
    let column_width = if narrow {
        let (anchor, _) = Scene::focus_anchor(&free);
        avail - (anchor - free.x) - 100.0 - 28.0
    } else {
        avail * 0.5 - gap - 32.0
    }
    .max(24.0);
    let mut place = |cols: &[Column], inline_more: bool, side: i8, x: f32, y0: f32| {
        let mut y = y0;
        for c in cols {
            // Column presentation merges on narrow screens; identity retains
            // the semantic incoming/outgoing column from the original prism.
            let semantic_side = if prism.left.iter().any(|col| col.word == c.word) {
                -1
            } else {
                1
            };
            let compact = inline_more || (c.rows.is_empty() && c.more > 0);
            let head_height = if compact {
                (roles::HEAD.line + 2.0) * text_scale.max(1.0)
            } else {
                head_height
            };
            let group_gap = if compact {
                4.0 * text_scale.max(1.0)
            } else {
                group_gap
            };
            let heading = scaled(roles::HEAD, text_scale);
            let label_text = if compact && c.more > 0 {
                // Keep the exact remainder visible even when a long relation
                // word must shorten at large text scale in a narrow room.
                let suffix = format!(" · {} more", c.more);
                let suffix_width =
                    shape(SharedString::from(suffix.clone()), heading, ink, window).width();
                let word = shape_fit(
                    c.word.text(),
                    heading,
                    ink,
                    (column_width - suffix_width).max(12.0),
                    window,
                )
                .text();
                SharedString::from(format!("{word}{suffix}"))
            } else {
                shape_fit(c.word.text(), heading, ink, column_width, window).text()
            };
            heads.push(Head {
                text: c.word.text(),
                label_text,
                x,
                y: y + 9.0 * text_scale.max(1.0),
                side,
                more: if compact { c.more } else { 0 },
                label: [0.0; 4],
            });
            y += head_height;
            for row in &c.rows {
                // Identifier space wins over a quiet origin note. Both are
                // explicitly fitted rather than silently clipped by the canvas.
                let note = row.note.as_ref().map(|note| {
                    shape_fit(note, where_role, ink, column_width * 0.35, window).text()
                });
                let note_width = note.as_ref().map_or(0.0, |note| {
                    shape(note.clone(), where_role, ink, window).width() + 8.0
                });
                let text = shape_fit(
                    &row.text,
                    name_role,
                    ink,
                    (column_width - note_width).max(12.0),
                    window,
                )
                .text();
                slots.push(Slot {
                    key: row.node.map(|node| SlotKey {
                        node,
                        side: semantic_side,
                        word: c.word,
                    }),
                    node: row.node,
                    kind: row.kind,
                    text,
                    note,
                    side,
                    x,
                    y: y + row_height / 2.0 - 2.0,
                    px: x,
                    py: y,
                    hx: x,
                    hy: y,
                    home_on: false,
                    label: None,
                    more: 0,
                });
                y += row_height;
            }
            if c.more > 0 && !c.rows.is_empty() && !inline_more {
                slots.push(Slot {
                    key: None,
                    node: None,
                    kind: None,
                    text: SharedString::default(),
                    note: None,
                    side,
                    x,
                    y: y + row_height / 2.0 - 2.0,
                    px: x,
                    py: y,
                    hx: x,
                    hy: y,
                    home_on: false,
                    label: None,
                    more: c.more,
                });
                y += row_height;
            }
            y += group_gap;
        }
    };
    let top = free.y + if free.y > view.y { 24.0 } else { 70.0 };
    let bottom = room[3] - 28.0;
    let centre_column = |height: f32| (fy - height * 0.5).max(top).min((bottom - height).max(top));
    if narrow {
        let all: Vec<Column> = prism.left.iter().chain(&prism.right).cloned().collect();
        let all = fit_columns(&all, (bottom - top).max(0.0), text_scale.max(1.0));
        let h = column_height(&all.columns, text_scale.max(1.0), all.inline_more);
        place(
            &all.columns,
            all.inline_more,
            1,
            fx + 100.0,
            centre_column(h),
        );
    } else {
        let left = fit_columns(&prism.left, (bottom - top).max(0.0), text_scale.max(1.0));
        let right = fit_columns(&prism.right, (bottom - top).max(0.0), text_scale.max(1.0));
        place(
            &left.columns,
            left.inline_more,
            -1,
            fx - gap,
            centre_column(column_height(
                &left.columns,
                text_scale.max(1.0),
                left.inline_more,
            )),
        );
        place(
            &right.columns,
            right.inline_more,
            1,
            fx + gap,
            centre_column(column_height(
                &right.columns,
                text_scale.max(1.0),
                right.inline_more,
            )),
        );
    }
    // The gather is a glide track; the sine lays the proxies' path.
    let e = ease(prism.g.clamp(0.0, 1.0));
    let margin = 24.0;
    // Shift whole columns by their actual widest row; spacing within a
    // column stays constant and proxies keep their flight destination.
    for side in [-1, 1] {
        let width = slots
            .iter()
            .filter(|sl| sl.side == side && sl.more == 0)
            .map(|sl| {
                shape(sl.text.clone(), name_role, ink, window).width()
                    + sl.note.as_ref().map_or(0.0, |note| {
                        shape(note.clone(), where_role, ink, window).width() + 8.0
                    })
            })
            .chain(heads.iter().filter(|h| h.side == side).map(|h| {
                shape(
                    h.label_text.clone(),
                    scaled(roles::HEAD, text_scale),
                    ink,
                    window,
                )
                .width()
            }))
            .chain(
                slots
                    .iter()
                    .filter(|sl| sl.side == side && sl.more > 0)
                    .map(|sl| {
                        shape(
                            SharedString::from(format!("and {} more", sl.more)),
                            scaled(roles::MORE, text_scale),
                            ink,
                            window,
                        )
                        .width()
                    }),
            )
            .fold(0.0_f32, f32::max);
        let low = free.x + 14.0 + if side < 0 { width + 18.0 } else { 0.0 };
        let high = free.x + avail - 14.0 - if side > 0 { width + 14.0 } else { 0.0 };
        for sl in slots.iter_mut().filter(|sl| sl.side == side) {
            sl.x = sl.x.clamp(low, high.max(low));
        }
        for h in heads.iter_mut().filter(|h| h.side == side) {
            h.x = h.x.clamp(low, high.max(low));
            let text = shape(
                h.label_text.clone(),
                scaled(roles::HEAD, text_scale),
                ink,
                window,
            );
            let x = if side > 0 {
                h.x - 4.0
            } else {
                h.x + 4.0 - text.width()
            };
            let half = (text.ascent() + text.descent()) * 0.5 + 2.0;
            h.label = [x - 2.0, h.y - half, x + text.width() + 2.0, h.y + half];
        }
    }
    for sl in &mut slots {
        if sl.more > 0 {
            let text = shape(
                SharedString::from(format!("and {} more", sl.more)),
                scaled(roles::MORE, text_scale),
                ink,
                window,
            );
            let x = if sl.side > 0 {
                sl.x + 8.0
            } else {
                sl.x - 8.0 - text.width()
            };
            let half = (text.ascent() + text.descent()) * 0.5 + 2.0;
            sl.label = Some([x - 2.0, sl.y - half, x + text.width() + 2.0, sl.y + half]);
            continue;
        }
        let (mut hx, mut hy) = match sl.node {
            Some(j) => view.to_screen(cam, lay.x[j as usize], lay.y[j as usize]),
            None => (sl.x, sl.y),
        };
        sl.home_on = sl.node.is_some()
            && hx > view.x
            && hy > view.y
            && hx < view.x + view.w
            && hy < view.y + view.h;
        hx = hx.clamp(view.x + margin, view.x + view.w - margin);
        hy = hy.clamp(view.y + margin, view.y + view.h - margin);
        sl.hx = hx;
        sl.hy = hy;
        sl.px = hx + (sl.x - hx) * e;
        sl.py = hy + (sl.y - hy) * e;
        let mut w = shape(sl.text.clone(), name_role, ink, window).width();
        if let Some(note) = &sl.note {
            w += shape(note.clone(), where_role, ink, window).width() + 8.0;
        }
        let half = (name_role.size * 0.65 + 2.0).max(8.0);
        sl.label = Some(if sl.side > 0 {
            [sl.px + 8.0, sl.py - half, sl.px + 14.0 + w, sl.py + half]
        } else {
            [sl.px - 20.0 - w, sl.py - half, sl.px - 8.0, sl.py + half]
        });
    }
    PrismFrame {
        fx,
        fy,
        slots,
        heads,
        e,
        narrow,
        node: prism.node,
        room,
    }
}

/// Paints the prism over the map (app.js `drawPrism`). `sel` is the walked
/// row, `hot` the hovered one.
#[allow(clippy::too_many_lines)]
pub fn paint(
    frame: &PrismFrame,
    look: &Look<'_>,
    sel: Option<usize>,
    hot: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) {
    let world = &*look.scene.world;
    let palette = look.palette;
    let (ink, mint, peri, line) = (
        palette.ink1,
        palette.mint.base,
        palette.peri.base,
        palette.line1,
    );
    let e = frame.e;
    #[allow(clippy::cast_possible_truncation)]
    let la = smooth(f64::from(e), 0.55, 1.0) as f32;
    let ts = look.text_scale;
    let focus_yours = world.yours(frame.node);
    let clip = [
        look.view.x - 2.0,
        look.view.y - 2.0,
        look.view.x + look.view.w + 2.0,
        look.view.y + look.view.h + 2.0,
    ];
    // Tethers home, with a quiet mark where home is.
    let mut tethers = Fill::new();
    let mut homes = Fill::new();
    for sl in &frame.slots {
        if sl.more == 0 && sl.home_on && (sl.hx - sl.px).hypot(sl.hy - sl.py) > 30.0 {
            tethers.dashed_in(
                &[pt(sl.px, sl.py), pt(sl.hx, sl.hy)],
                1.0,
                2.0,
                5.0,
                0.0,
                clip,
            );
            homes.diamond_ring(sl.hx, sl.hy, 3.0, 1.0);
        }
    }
    tethers.paint(window, tone(line, 0.12 * e));
    homes.paint(window, tone(ink, 0.25 * e));
    // Strands from the focus to each proxy, with flow along them.
    let mut quiet = Fill::new();
    let mut yours = Fill::new();
    let mut lit = Fill::new();
    let mut flow_quiet = Fill::new();
    let mut flow_lit = Fill::new();
    for (q, sl) in frame.slots.iter().enumerate() {
        if sl.more > 0 {
            continue;
        }
        let out = sl.side > 0;
        let x0 = frame.fx
            + if frame.narrow {
                10.0
            } else if out {
                12.0
            } else {
                -12.0
            };
        let x1 = sl.px + if out { -7.0 } else { 7.0 };
        let c = (x1 - x0) * 0.5;
        let mut pts = vec![pt(x0, frame.fy)];
        cubic_to(
            &mut pts,
            pt(x0, frame.fy),
            pt(x0 + c, frame.fy),
            pt(x1 - c, sl.py),
            pt(x1, sl.py),
            20,
        );
        let on = sel == Some(q) || hot == Some(q);
        if on {
            lit.polyline(&pts, 1.5);
            if look.flow_alpha > 0.0 {
                flow_lit.dashed_in(
                    &pts,
                    1.0,
                    1.5,
                    9.0,
                    if out {
                        -look.flow * 1.4
                    } else {
                        look.flow * 1.4
                    },
                    clip,
                );
            }
        } else {
            if sl.node.is_some_and(|j| world.yours(j)) {
                yours.polyline(&pts, 1.0);
            } else {
                quiet.polyline(&pts, 1.0);
            }
            if look.flow_alpha > 0.0 {
                flow_quiet.dashed_in(
                    &pts,
                    1.0,
                    1.5,
                    9.0,
                    if out {
                        -look.flow * 1.4
                    } else {
                        look.flow * 1.4
                    },
                    clip,
                );
            }
        }
    }
    quiet.paint(window, tone(ink, 0.26 * e));
    yours.paint(window, tone(mint, 0.55 * e));
    lit.paint(window, tone(peri, 0.9));
    flow_quiet.paint(window, tone(ink, 0.5 * e * look.flow_alpha));
    flow_lit.paint(window, tone(peri, 0.5 * e * look.flow_alpha));
    // The focus gem.
    let mut gem = Fill::new();
    gem.diamond(frame.fx, frame.fy, 9.0);
    gem.paint(window, tone(peri, 1.0));
    let mut ring = Fill::new();
    ring.diamond_ring(frame.fx, frame.fy, 14.0, 1.0);
    ring.paint(window, tone(peri, 0.45));
    // Proxies and names.
    let mut texts = Vec::new();
    let mut halos = Vec::new();
    let mut shapes: [Fill; 3] = std::array::from_fn(|_| Fill::new());
    for (q, sl) in frame.slots.iter().enumerate() {
        if sl.more > 0 {
            if la < 0.02 {
                continue;
            }
            let t = shape(
                SharedString::from(format!("and {} more", sl.more)),
                scaled(roles::MORE, ts),
                tone(ink, 0.38 * la),
                window,
            );
            let x = if sl.side > 0 {
                sl.x + 8.0
            } else {
                sl.x - 8.0 - t.width()
            };
            let base = sl.y + (t.ascent() - t.descent()) * 0.5;
            texts.push((t, x, base));
            continue;
        }
        let on = sel == Some(q) || hot == Some(q);
        let mine = sl
            .node
            .is_some_and(|j| world.yours(j) || (world.reached(j) && focus_yours));
        let bank = if on { 2 } else { usize::from(mine) };
        let s = 4.2;
        match sl.kind {
            Some(Kind::Function | Kind::Method | Kind::Constant | Kind::Macro) => {
                shapes[bank].rect(sl.px - s * 0.7, sl.py - s * 0.7, s * 1.4, s * 1.4);
            }
            Some(Kind::Trait) | None => shapes[bank].diamond_ring(sl.px, sl.py, s, 1.2),
            Some(_) => shapes[bank].diamond(sl.px, sl.py, s),
        }
        if la < 0.02 {
            continue;
        }
        let name_role = scaled(if on { roles::NAME_BOLD } else { roles::NAME }, ts);
        let c = if on {
            tone(peri, la)
        } else if mine {
            tone(mint, 0.92 * la)
        } else {
            tone(ink, 0.92 * la)
        };
        let t = shape(sl.text.clone(), name_role, c, window);
        let w = t.width();
        let tx = if sl.side > 0 {
            sl.px + 10.0
        } else {
            sl.px - 10.0 - w
        };
        let base = sl.py + (t.ascent() - t.descent()) * 0.5;
        if let Some([a, b, c, d]) = sl.label {
            halos.push(Bounds {
                origin: point(px(a), px(b)),
                size: size(px(c - a), px(d - b)),
            });
        }
        if let Some(note) = &sl.note {
            let n = shape(
                note.clone(),
                scaled(roles::WHERE, ts),
                tone(ink, 0.32 * la),
                window,
            );
            let nx = if sl.side > 0 {
                tx + w + 8.0
            } else {
                tx - 8.0 - n.width()
            };
            let nb = sl.py + 0.5 + (n.ascent() - n.descent()) * 0.5;
            texts.push((n, nx, nb));
        }
        texts.push((t, tx, base));
    }
    let [quiet_s, mine_s, on_s] = shapes;
    quiet_s.paint(window, tone(ink, 0.95));
    mine_s.paint(window, tone(mint, 0.95));
    on_s.paint(window, tone(peri, 0.95));
    for h in &frame.heads {
        if la < 0.02 {
            continue;
        }
        let t = shape(
            h.label_text.clone(),
            scaled(roles::HEAD, ts),
            tone(ink, 0.5 * la),
            window,
        );
        let x = if h.side > 0 {
            h.x - 4.0
        } else {
            h.x + 4.0 - t.width()
        };
        let base = h.y + (t.ascent() - t.descent()) * 0.5;
        texts.push((t, x, base));
    }
    // A quiet ground under each name so strands pass behind it.
    let bounds = Bounds {
        origin: point(px(look.view.x), px(look.view.y)),
        size: size(px(look.view.w), px(look.view.h)),
    };
    window.paint_layer(bounds, |window| {
        for b in &halos {
            window.paint_quad(fill(*b, tone(palette.g0, 0.85 * la)));
        }
    });
    let sf = window.scale_factor();
    window.paint_layer(bounds, |window| {
        for (t, x, base) in &texts {
            t.paint((x * sf).round() / sf, (base * sf).round() / sf, window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{column_height, fit_columns};
    use crate::semantics::{Column, PrismRow, Word};

    #[test]
    fn short_prisms_keep_groups_and_honest_remainders() {
        let groups = vec![
            Column {
                word: Word::CalledFrom,
                rows: (0..6)
                    .map(|i| PrismRow {
                        node: Some(i),
                        kind: None,
                        text: format!("caller{i}").into(),
                        note: None
                    })
                    .collect(),
                more: 10,
            };
            3
        ];
        assert_eq!(fit_columns(&groups, 900.0, 1.0).columns, groups);
        let fit = fit_columns(&groups, 360.0, 2.0);
        assert_eq!(fit.columns.len(), 3);
        assert!(column_height(&fit.columns, 2.0, fit.inline_more) <= 360.0);
        for c in fit.columns {
            assert_eq!(c.rows.len() + c.more, 16);
        }
    }
    #[test]
    fn constrained_columns_allocate_actions_fairly_and_monotonically() {
        let groups: Vec<_> = (0..5)
            .map(|group| Column {
                word: Word::CalledFrom,
                rows: (0..(group + 2))
                    .map(|row| PrismRow {
                        node: (row != 0).then_some((group * 10 + row) as u32),
                        kind: None,
                        text: format!("row{group}/{row}").into(),
                        note: None,
                    })
                    .collect(),
                more: group + 3,
            })
            .collect();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let overhead = (5.0 * (super::roles::HEAD.line + 2.0 + 4.0) - 4.0) * scale;
            let one_each = fit_columns(&groups, overhead + 5.0 * super::ROW * scale + 0.01, scale);
            assert!(
                one_each
                    .columns
                    .iter()
                    .all(|c| c.rows.len() == 1 && c.rows[0].node.is_some())
            );
            let mut previous = 0;
            for height in (150..=1200).step_by(3) {
                let fit = fit_columns(&groups, height as f32, scale);
                let kept: usize = fit.columns.iter().map(|c| c.rows.len()).sum();
                assert!(
                    kept >= previous,
                    "lost rows at height {height}, scale {scale}"
                );
                previous = kept;
                if height as f32 >= overhead {
                    assert!(
                        column_height(&fit.columns, scale, fit.inline_more) <= height as f32 + 0.01
                    );
                }
                for (before, after) in groups.iter().zip(&fit.columns) {
                    assert_eq!(
                        before.rows.len() + before.more,
                        after.rows.len() + after.more
                    );
                }
            }
        }
    }
}
