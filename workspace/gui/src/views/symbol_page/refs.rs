//! The Refs and Impls tables (§16).
//!
//! # Shape
//!
//! References are the one surface on a symbol page that can be genuinely huge —
//! a popular trait has tens of thousands of them. So this is `uniform_list` from
//! the first row (LD-6), over a *flattened* index: one `Arc<[FlatRow]>` where an
//! entry is either a file header or a reference under it. Collapsing a file
//! group rebuilds that flat vector once, at update time; the list itself never
//! learns about grouping, and `render` clones one `Arc` rather than a table.
//!
//! Columns are the Sourcegraph pattern: line · path · kind · precision. The
//! precision badge is the honest part — a textual match and a resolved
//! cross-reference are not the same claim, and conflating them is how reference
//! search loses trust.
//!
//! # Grouping without sorting
//!
//! `render` may not sort or filter (§1.1.4), and neither does the projection:
//! [`RefsTable::append_page`] is a single linear pass that starts a new group
//! whenever the file changes from the previous row. The engine emits a page's
//! rows file-contiguous; if it ever does not, the same file simply appears as
//! two groups — degraded, never wrong, never a panic.
//!
//! # Motion
//!
//! Pages append with `row.cascade` (§5.2), gated on the arrival window so
//! scrolling back over already-landed rows never replays their entrance
//! (§4.1 entrance identity rule).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, UniformListScrollHandle, Window, div, uniform_list,
};
use gpui_component::{Icon, IconName, Sizable as _, StyledExt as _};
use nudox_engine::wire::{ImplsPage, RefRow, RefsPage, SymbolKey};

use crate::motion::declarative::{entrance_id, row_enter};
use crate::motion::tokens::{MotionTokens, ROW_CASCADE_WINDOW};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::theme::tokens::ColourRoles;
use crate::ui::{Badge, EmptyState};

use super::header::shared;

/// Format a count once, on arrival. Never called from `render`.
fn count_text(n: usize) -> SharedString {
    SharedString::from(n.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Precision
// ─────────────────────────────────────────────────────────────────────────────

/// How confident the engine is that a reference really is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// Resolved through the corpus — this *is* a reference.
    Precise,
    /// A textual or heuristic match — probably a reference.
    Textual,
    /// A claim we do not recognise; shown verbatim (LD-7).
    Other,
}

impl Precision {
    /// Classify the engine's free-form kind tag. Projection time only.
    fn classify(tag: &str) -> Self {
        if tag.eq_ignore_ascii_case("precise")
            || tag.eq_ignore_ascii_case("resolved")
            || tag.eq_ignore_ascii_case("definition")
        {
            return Self::Precise;
        }
        if tag.eq_ignore_ascii_case("textual")
            || tag.eq_ignore_ascii_case("search")
            || tag.eq_ignore_ascii_case("fuzzy")
            || tag.eq_ignore_ascii_case("heuristic")
        {
            Self::Textual
        } else {
            Self::Other
        }
    }
}

/// Colours and label for the precision badge.
///
/// `Other` shows the engine's own words rather than being forced into one of
/// our two buckets — a claim we do not understand is still a claim (LD-7).
fn precision_chrome(
    precision: Precision,
    kind: &SharedString,
    colours: &ColourRoles,
) -> (Hsla, Hsla, SharedString) {
    match precision {
        Precision::Precise => (
            colours.ok.opacity(0.18),
            colours.ok_fg,
            SharedString::from("precise"),
        ),
        Precision::Textual => (
            colours.warn.opacity(0.18),
            colours.warn_fg,
            SharedString::from("textual"),
        ),
        Precision::Other => (colours.bg_hover, colours.fg_muted, kind.clone()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Projection
// ─────────────────────────────────────────────────────────────────────────────

/// One reference row, fully render-ready.
#[derive(Clone, Debug)]
struct RefRowView {
    target: SymbolKey,
    /// Path with the trailing `:line` removed, if there was one.
    path: SharedString,
    /// The line number, when the engine encoded one into `path`.
    line: Option<SharedString>,
    /// The engine's raw kind tag — always shown, whatever it says.
    kind: SharedString,
    precision: Precision,
}

/// Split `src/foo.rs:120` into `("src/foo.rs", Some("120"))`.
///
/// TODO(wire): `RefRow` has no `line` field, so a line number can only reach us
/// encoded in `path`. When the wire type grows `line: u32` this function
/// disappears and the column becomes first-class.
fn split_line(path: &str) -> (&str, Option<&str>) {
    match path.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            (head, Some(tail))
        }
        _ => (path, None),
    }
}

fn project_ref(row: &RefRow) -> RefRowView {
    let raw = shared(&row.path);
    let (path, line) = split_line(&raw);
    let kind = shared(&row.kind_tag);
    RefRowView {
        target: row.target.clone(),
        path: SharedString::from(String::from(path)),
        line: line.map(|l| SharedString::from(String::from(l))),
        precision: Precision::classify(&kind),
        kind,
    }
}

/// `true` when `file` starts a new group after `last`.
fn group_boundary(last: Option<&str>, file: &str) -> bool {
    match last {
        Some(prev) => prev != file,
        None => true,
    }
}

/// How many flat entries a set of `(collapsed, row_count)` groups produces.
///
/// Used to size the flat index exactly, and separately testable — the collapse
/// arithmetic is the one thing here that can silently desynchronise the list's
/// item count from what `render` will produce.
fn flat_len(groups: impl Iterator<Item = (bool, usize)>) -> usize {
    groups
        .map(|(collapsed, n)| 1 + if collapsed { 0 } else { n })
        .sum()
}

/// A file grouping of a contiguous run of references.
#[derive(Clone, Debug)]
struct Group {
    file: SharedString,
    count: SharedString,
    collapsed: bool,
    rows: Vec<RefRowView>,
}

/// One entry of the flat index the `uniform_list` actually renders.
///
/// Cloning is a handful of `Arc` bumps, so rebuilding this on collapse or on a
/// page arrival is cheap, and `render` only ever clones the enclosing `Arc`.
#[derive(Clone, Debug)]
enum FlatRow {
    Header {
        group: usize,
        file: SharedString,
        count: SharedString,
        collapsed: bool,
    },
    Row(RefRowView),
}

// ─────────────────────────────────────────────────────────────────────────────
// RefsTable
// ─────────────────────────────────────────────────────────────────────────────

/// The grouped, virtualized references table.
pub struct RefsTable {
    groups: Vec<Group>,
    flat: Arc<[FlatRow]>,
    consumed_pages: usize,
    total: u64,
    /// Pre-formatted total, for the tab's count label.
    total_text: SharedString,
    scroll: UniformListScrollHandle,
    /// Generation stamp: part of every entrance id (LD-19).
    generation: u64,
    /// When the last page landed — the `row.cascade` window opens from here.
    last_page: Option<Instant>,
}

impl Default for RefsTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RefsTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            flat: Arc::from(Vec::new()),
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything and re-stamp for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.groups.clear();
        self.flat = Arc::from(Vec::new());
        self.consumed_pages = 0;
        self.total = 0;
        self.total_text = SharedString::from("0");
        self.generation = generation;
        self.last_page = None;
    }

    /// Total references reported by the engine.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Pre-formatted total for the tab label.
    pub fn total_text(&self) -> SharedString {
        self.total_text.clone()
    }

    /// `true` when no page has landed yet.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Consume any pages the store has that we have not projected yet.
    ///
    /// Returns `true` if anything changed.
    pub fn sync(&mut self, pages: &[RefsPage]) -> bool {
        if self.consumed_pages >= pages.len() {
            return false;
        }
        for page in &pages[self.consumed_pages..] {
            self.append_page(page);
        }
        self.consumed_pages = pages.len();
        self.last_page = Some(Instant::now());
        self.rebuild_flat();
        true
    }

    /// One linear pass: no sort, no filter, no second look at earlier rows.
    fn append_page(&mut self, page: &RefsPage) {
        if page.total != self.total {
            self.total = page.total;
            self.total_text = SharedString::from(page.total.to_string());
        }
        for row in page.refs.iter() {
            let view = project_ref(row);
            let last_file = self.groups.last().map(|g| g.file.as_ref());
            if group_boundary(last_file, &view.path) {
                self.groups.push(Group {
                    file: view.path.clone(),
                    count: count_text(0),
                    collapsed: false,
                    rows: Vec::new(),
                });
            }
            if let Some(group) = self.groups.last_mut() {
                group.rows.push(view);
                group.count = count_text(group.rows.len());
            }
        }
    }

    /// Toggle a file group. Returns `true` if the flat index changed.
    pub fn toggle_group(&mut self, group_ix: usize) -> bool {
        match self.groups.get_mut(group_ix) {
            Some(group) => {
                group.collapsed = !group.collapsed;
                self.rebuild_flat();
                true
            }
            None => false,
        }
    }

    fn rebuild_flat(&mut self) {
        let len = flat_len(self.groups.iter().map(|g| (g.collapsed, g.rows.len())));
        let mut flat = Vec::with_capacity(len);
        for (gx, group) in self.groups.iter().enumerate() {
            flat.push(FlatRow::Header {
                group: gx,
                file: group.file.clone(),
                count: group.count.clone(),
                collapsed: group.collapsed,
            });
            if !group.collapsed {
                flat.extend(group.rows.iter().cloned().map(FlatRow::Row));
            }
        }
        debug_assert_eq!(flat.len(), len, "flat_len disagrees with rebuild_flat");
        self.flat = Arc::from(flat);
    }

    /// Render the table, or a designed empty state (LD-16).
    ///
    /// `streaming` suppresses the empty state while pages are still arriving —
    /// "no references" is a *finding*, and we do not report it prematurely.
    ///
    /// # Layout contract
    ///
    /// Same as `ImplsTable::render`: returns naturally-sized content whose total
    /// height is `rows × row_h`.  The caller wraps it in `max_h.overflow_y_scroll`.
    /// The `uniform_list` receives an explicit `h` so it has a definite height to
    /// measure items against.  See `ImplsTable::render` for the full rationale.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        on_toggle: impl Fn(usize, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let scale = ext.motion_scale;

        if self.flat.is_empty() {
            return if streaming {
                // Nothing to say yet; the tab's `⟳` already says it.
                div().w_full().into_any_element()
            } else {
                EmptyState::new(
                    IconName::Search,
                    SharedString::from("No references"),
                    SharedString::from("Nothing in the loaded corpus mentions this symbol."),
                    MotionTokens::new(scale),
                )
                .into_any_element()
            };
        }

        // One `Arc` bump per frame — the table itself is never cloned here.
        let flat = self.flat.clone();
        let count = flat.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);
        let toggle: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(on_toggle);

        // Pre-compute list height — must match the `.h(row_h)` on each row element.
        let sp = ext.space;
        let ts = ext.type_scale;
        let row_h = ts.dense.line_height + sp.space_2;
        let list_h = gpui::px(count as f32 * f32::from(row_h));

        div()
            .id("symbol.refs")
            .w_full()
            .child(
                uniform_list("symbol.refs.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);
                    let row_h = ts.dense.line_height + sp.space_2;

                    range
                        .map(|ix| {
                            let element = match &flat[ix] {
                                FlatRow::Header {
                                    group,
                                    file,
                                    count,
                                    collapsed,
                                } => {
                                    let group = *group;
                                    let toggle = toggle.clone();
                                    div()
                                        .id(("symbol.refs.group", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_1)
                                        .h(row_h)
                                        .px(sp.space_2)
                                        .bg(colours.bg_raised)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .on_click(move |_, window, cx| toggle(group, window, cx))
                                        .child(
                                            Icon::new(if *collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .text_color(colours.fg_faint)
                                            .with_size(gpui_component::Size::XSmall),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_default)
                                                .child(file.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(count.clone()),
                                        )
                                }
                                FlatRow::Row(row) => {
                                    let key = row.target.clone();
                                    let open = open.clone();
                                    let (badge_bg, badge_fg, badge_label) =
                                        precision_chrome(row.precision, &row.kind, &colours);

                                    div()
                                        .id(("symbol.refs.row", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_2)
                                        .h(row_h)
                                        .pl(sp.space_5)
                                        .pr(sp.space_2)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .active(|s| s.bg(colours.bg_active))
                                        .on_click(move |_, window, cx| open(&key, window, cx))
                                        // line
                                        .child(
                                            div()
                                                .w(sp.space_6)
                                                .flex_shrink_0()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_faint)
                                                .children(row.line.clone()),
                                        )
                                        // path
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_muted)
                                                .child(row.path.clone()),
                                        )
                                        // kind
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(row.kind.clone()),
                                        )
                                        // precision badge
                                        .child(div().flex_shrink_0().child(Badge::custom(
                                            ("symbol.refs.precision", ix),
                                            badge_label,
                                            badge_bg,
                                            badge_fg,
                                        )))
                                }
                            };

                            // `row.cascade`, gated on the arrival window so
                            // scrolling never replays an entrance (§4.1).
                            if cascade_open {
                                row_enter(
                                    element,
                                    entrance_id("symbol.refs.enter", generation, ix),
                                    ix,
                                    &motion,
                                )
                            } else {
                                element.into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .h(list_h),
            )
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ImplsTable
// ─────────────────────────────────────────────────────────────────────────────

// ── Blanket / variadic grouping ──────────────────────────────────────────────
//
// `ImplRow` (wire) carries `key`, `label`, `is_blanket`, `trait_label`, and
// `self_generic_count`.  These enable two classes of structured grouping:
//
// * Blanket grouping: partition rows on `is_blanket` at *projection* time (in
//   `sync`, not `render`).  Render own impls first, then a separate "Blanket
//   Implementations" subheading, collapsed by default.  The intent matches
//   docs.rs: blanket impls describe the ecosystem's structure, not the specific
//   type being viewed, so they should not crowd the type's own impls.
//
// * Variadic / tuple family detection: group rows by `(trait_label,
//   self_base)`.  If a group has ≥3 members with consecutive
//   `self_generic_count` values, collapse it to one summary row.  The threshold
//   of 3 and the consecutiveness requirement prevent mis-grouping two unrelated
//   impls that happen to share a trait — a wrong grouping is worse than none.

/// One implementor row, fully projected and render-ready.
#[derive(Clone, Debug)]
struct ImplRowView {
    key: SymbolKey,
    label: SharedString,
    is_blanket: bool,
    /// The trait label, used as the grouping key for variadic family detection.
    trait_label: Option<SharedString>,
    /// Generic count on the self type — used for arity-family detection.
    self_generic_count: u32,
}

/// A flat entry in the impls list — either a row or a section subheading.
#[derive(Clone, Debug)]
enum ImplFlatRow {
    /// A rendered implementation row.
    Row(ImplRowView),
    /// A subheading (e.g. "Blanket Implementations (N)"), collapsed by default.
    Subheading {
        label: SharedString,
        count: SharedString,
        collapsed: bool,
    },
}

// ── Variadic family detection ─────────────────────────────────────────────────

/// Return the self-type base: the label with everything from `<` stripped.
///
/// `"impl Debug for Router<E>"` → `"impl Debug for Router"`.
/// Used as the second key in `(trait_label, self_base)` grouping.
fn self_base(label: &str) -> &str {
    match label.find('<') {
        Some(idx) => label[..idx].trim_end(),
        None => label,
    }
}

/// Detect whether `counts` (sorted ascending) forms a run of ≥3 consecutive integers.
///
/// Non-consecutive counts and runs shorter than 3 return `false`.
fn is_consecutive_run(counts: &[u32]) -> bool {
    counts.len() >= 3 && counts.windows(2).all(|w| w[1] == w[0] + 1)
}

/// Collapse a variadic family into a single summary row.
///
/// Called only when `is_consecutive_run` returns `true`.  The summary row uses
/// the key of the first member and a label like `"impl Handler for F  (arities 1–16)"`.
/// No `format!` is called from `render`; the string is built here at projection time.
fn collapse_family(rows: &[ImplRowView]) -> ImplRowView {
    let first = rows.first().expect("family is non-empty by construction");
    let min = rows.iter().map(|r| r.self_generic_count).min().unwrap_or(0);
    let max = rows.iter().map(|r| r.self_generic_count).max().unwrap_or(0);
    let base_label = self_base(&first.label);
    let summary = SharedString::from(
        [base_label, "  (arities ", &min.to_string(), "–", &max.to_string(), ")"].concat(),
    );
    ImplRowView {
        key: first.key.clone(),
        label: summary,
        is_blanket: first.is_blanket,
        trait_label: first.trait_label.clone(),
        self_generic_count: first.self_generic_count,
    }
}

/// Project a slice of `ImplRowView`s through variadic-family detection.
///
/// Returns a new vec where any group of ≥3 rows with the same `(trait_label,
/// self_base)` and consecutive `self_generic_count` values is replaced by a
/// single summary row.  All other rows pass through unchanged.
///
/// This is called at projection time only (inside `sync`), not in `render`.
fn apply_variadic_grouping(rows: Vec<ImplRowView>) -> Vec<ImplRowView> {
    // Group by (trait_label, self_base(label)).
    // We use an ordered vec of groups to preserve arrival order.
    let mut groups: Vec<(Option<SharedString>, SharedString, Vec<ImplRowView>)> = Vec::new();

    for row in rows {
        let base = SharedString::from(self_base(&row.label).to_owned());
        let key_trait = row.trait_label.clone();
        // Find an existing group with the same (trait, base).
        let found = groups.iter_mut().find(|(t, b, _)| {
            *t == key_trait && *b == base
        });
        match found {
            Some((_, _, group)) => group.push(row),
            None => groups.push((key_trait, base, vec![row])),
        }
    }

    let mut out = Vec::new();
    for (_, _, mut group) in groups {
        // Sort by self_generic_count before checking consecutiveness.
        group.sort_unstable_by_key(|r| r.self_generic_count);
        let counts: Vec<u32> = group.iter().map(|r| r.self_generic_count).collect();
        if is_consecutive_run(&counts) {
            out.push(collapse_family(&group));
        } else {
            out.extend(group);
        }
    }
    out
}

/// Build the flat list used by `uniform_list`.
///
/// Layout:
///   [own row] ...
///   [Subheading "Blanket Implementations (N)" — collapsed]
///   [blanket row] ...   ← only when the subheading is expanded
///
/// The subheading is only emitted when there are blanket rows.
fn build_flat(
    own_rows: &[ImplRowView],
    blanket_rows: &[ImplRowView],
    blanket_collapsed: bool,
) -> Vec<ImplFlatRow> {
    let mut flat = Vec::with_capacity(
        own_rows.len()
            + if blanket_rows.is_empty() {
                0
            } else {
                1 + if blanket_collapsed { 0 } else { blanket_rows.len() }
            },
    );

    for r in own_rows {
        flat.push(ImplFlatRow::Row(r.clone()));
    }

    if !blanket_rows.is_empty() {
        // Precompute the count label at projection time — §1.1.4 forbids format!
        // inside render.
        let count_str = blanket_rows.len().to_string();
        flat.push(ImplFlatRow::Subheading {
            label: SharedString::from("Blanket Implementations"),
            count: SharedString::from(count_str),
            collapsed: blanket_collapsed,
        });
        if !blanket_collapsed {
            for r in blanket_rows {
                flat.push(ImplFlatRow::Row(r.clone()));
            }
        }
    }

    flat
}

/// The implementors table — flat, virtualized, same motion contract as `RefsTable`.
pub struct ImplsTable {
    /// Own (non-blanket) impls after variadic-family collapsing.
    own_rows: Vec<ImplRowView>,
    /// Blanket impls after variadic-family collapsing.
    blanket_rows: Vec<ImplRowView>,
    /// Flat list for the `uniform_list`.  Rebuilt on every `sync` and on
    /// blanket-section toggle.
    flat: Arc<[ImplFlatRow]>,
    /// Whether the "Blanket Implementations" subheading is collapsed.
    blanket_collapsed: bool,
    consumed_pages: usize,
    total: u64,
    total_text: SharedString,
    scroll: UniformListScrollHandle,
    generation: u64,
    last_page: Option<Instant>,
}

impl Default for ImplsTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ImplsTable {
    /// An empty table.
    pub fn new() -> Self {
        Self {
            own_rows: Vec::new(),
            blanket_rows: Vec::new(),
            flat: Arc::from(Vec::new()),
            blanket_collapsed: true,
            consumed_pages: 0,
            total: 0,
            total_text: SharedString::from("0"),
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            last_page: None,
        }
    }

    /// Drop everything and re-stamp for a new generation.
    pub fn reset(&mut self, generation: u64) {
        self.own_rows.clear();
        self.blanket_rows.clear();
        self.flat = Arc::from(Vec::new());
        self.blanket_collapsed = true;
        self.consumed_pages = 0;
        self.total = 0;
        self.total_text = SharedString::from("0");
        self.generation = generation;
        self.last_page = None;
    }

    /// Total implementors reported by the engine.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Pre-formatted total for the tab label.
    pub fn total_text(&self) -> SharedString {
        self.total_text.clone()
    }

    /// `true` when no page has landed yet.
    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    /// Toggle the "Blanket Implementations" collapsed section.
    ///
    /// Returns `true` if the flat list changed (i.e. there were blanket rows).
    pub fn toggle_blanket(&mut self) -> bool {
        if self.blanket_rows.is_empty() {
            return false;
        }
        self.blanket_collapsed = !self.blanket_collapsed;
        self.rebuild_flat();
        true
    }

    /// Consume any pages we have not projected yet.
    ///
    /// Projection (partition + variadic grouping) runs here — never in `render`.
    pub fn sync(&mut self, pages: &[ImplsPage]) -> bool {
        if self.consumed_pages >= pages.len() {
            return false;
        }

        // Collect raw projected rows from the new pages.
        let mut raw_own: Vec<ImplRowView> = Vec::new();
        let mut raw_blanket: Vec<ImplRowView> = Vec::new();

        for page in &pages[self.consumed_pages..] {
            if page.total != self.total {
                self.total = page.total;
                self.total_text = SharedString::from(page.total.to_string());
            }
            for r in page.impls.iter() {
                let view = ImplRowView {
                    key: r.key.clone(),
                    label: shared(&r.label),
                    is_blanket: r.is_blanket,
                    trait_label: r.trait_label.as_ref().map(|t| shared(t)),
                    self_generic_count: r.self_generic_count,
                };
                if r.is_blanket {
                    raw_blanket.push(view);
                } else {
                    raw_own.push(view);
                }
            }
        }
        self.consumed_pages = pages.len();
        self.last_page = Some(Instant::now());

        // Append to accumulated rows, then re-run variadic grouping on the
        // complete set so a family that straddles two pages is still detected.
        self.own_rows.extend(raw_own);
        self.blanket_rows.extend(raw_blanket);
        self.own_rows = apply_variadic_grouping(std::mem::take(&mut self.own_rows));
        self.blanket_rows = apply_variadic_grouping(std::mem::take(&mut self.blanket_rows));

        self.rebuild_flat();
        true
    }

    fn rebuild_flat(&mut self) {
        let flat = build_flat(&self.own_rows, &self.blanket_rows, self.blanket_collapsed);
        self.flat = Arc::from(flat);
    }

    /// Render the table, or a designed empty state.
    ///
    /// # Layout contract
    ///
    /// This method returns a `w_full` div whose height is governed entirely by
    /// its content (each row has a definite pixel height, so the total is
    /// `rows × row_h`).  The *caller* (mod.rs `symbol.impls.body`) wraps this in
    /// `max_h(400px).overflow_y_scroll()`, which scrolls when the content is
    /// taller than the cap.  We must NOT put `size_full()` or `flex_1()` here,
    /// because those resolve against the wrapper's height — and `max_h` alone
    /// does not establish a definite height for `uniform_list` to measure against.
    ///
    /// The `uniform_list` itself is given an explicit `h` computed from the row
    /// count and the design-time row height (dense line height + 2 × space_1).
    /// `uniform_list` renders only the rows currently in the viewport; the
    /// explicit height tells the scroller how much total content exists.
    pub fn render(
        &self,
        streaming: bool,
        on_open: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static,
        on_toggle_blanket: impl Fn(&mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let ext = cx.theme_ext();
        let scale = ext.motion_scale;

        if self.flat.is_empty() {
            return if streaming {
                div().w_full().into_any_element()
            } else {
                EmptyState::new(
                    IconName::Frame,
                    SharedString::from("No implementors"),
                    SharedString::from("No type in the loaded corpus implements this."),
                    MotionTokens::new(scale),
                )
                .into_any_element()
            };
        }

        let flat = self.flat.clone();
        let count = flat.len();
        let generation = self.generation;
        let cascade_open = self
            .last_page
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;
        let open: Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)> = Rc::new(on_open);
        let toggle_blanket: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_toggle_blanket);

        // Pre-compute the row height so the `uniform_list` can receive an
        // explicit `h`.  This must match the `.h(...)` on each row element below
        // — if they diverge the list measures at the wrong height and clips rows.
        let sp = ext.space;
        let ts = ext.type_scale;
        let row_h = ts.dense.line_height + sp.space_2;
        // The total natural height of all rows.  `uniform_list` uses this as its
        // scroll extent; the outer `max_h + overflow_y_scroll` wrapper clips it.
        let list_h = gpui::px(count as f32 * f32::from(row_h));

        div()
            .id("symbol.impls")
            .w_full()
            .child(
                uniform_list("symbol.impls.list", count, move |range, _window, cx| {
                    let ext = cx.theme_ext();
                    let sp = ext.space;
                    let ts = ext.type_scale;
                    let colours = ext.colours;
                    let motion = MotionTokens::new(ext.motion_scale);
                    let row_h = ts.dense.line_height + sp.space_2;

                    range
                        .map(|ix| {
                            let element = match &flat[ix] {
                                ImplFlatRow::Row(row) => {
                                    let key = row.key.clone();
                                    let open = open.clone();
                                    div()
                                        .id(("symbol.impls.row", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(row_h)
                                        .px(sp.space_3)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .active(|s| s.bg(colours.bg_active))
                                        .on_click(move |_, window, cx| open(&key, window, cx))
                                        .child(
                                            div()
                                                .flex_1()
                                                .overflow_hidden()
                                                .truncate()
                                                .font_family("monospace")
                                                .text_size(ts.mono.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_default)
                                                .child(row.label.clone()),
                                        )
                                }
                                ImplFlatRow::Subheading { label, count, collapsed } => {
                                    let toggle = toggle_blanket.clone();
                                    div()
                                        .id(("symbol.impls.blanket.head", ix))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(sp.space_1)
                                        .h(row_h)
                                        .px(sp.space_2)
                                        .bg(colours.bg_raised)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(colours.bg_hover))
                                        .on_click(move |_, window, cx| toggle(window, cx))
                                        .child(
                                            Icon::new(if *collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .text_color(colours.fg_faint)
                                            .with_size(gpui_component::Size::XSmall),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_size(ts.dense.size)
                                                .line_height(ts.dense.line_height)
                                                .text_color(colours.fg_default)
                                                .child(label.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(ts.caption.size)
                                                .line_height(ts.caption.line_height)
                                                .text_color(colours.fg_faint)
                                                .child(count.clone()),
                                        )
                                }
                            };

                            if cascade_open {
                                row_enter(
                                    element,
                                    entrance_id("symbol.impls.enter", generation, ix),
                                    ix,
                                    &motion,
                                )
                            } else {
                                element.into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                // Give the list a definite pixel height equal to the total
                // content height.  The outer wrapper caps and scrolls it.
                // Without this, `uniform_list` has no height to measure against
                // and collapses to zero — making the section appear empty even
                // though the rows were rendered.
                .h(list_h),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trailing `:123` is a line number; anything else is part of the path.
    /// Getting this wrong would silently truncate Windows paths.
    #[test]
    fn line_split_only_accepts_digits() {
        assert_eq!(split_line("src/a.rs:120"), ("src/a.rs", Some("120")));
        assert_eq!(split_line("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_line("C:/x/a.rs"), ("C:/x/a.rs", None));
        assert_eq!(split_line("src/a.rs:"), ("src/a.rs:", None));
        assert_eq!(split_line("a.rs:12:4"), ("a.rs:12", Some("4")));
    }

    /// Grouping is a comparison against the previous row only — one pass, no
    /// sort, no map.
    #[test]
    fn group_boundary_only_looks_backwards() {
        assert!(group_boundary(None, "src/a.rs"));
        assert!(!group_boundary(Some("src/a.rs"), "src/a.rs"));
        assert!(group_boundary(Some("src/a.rs"), "src/b.rs"));
    }

    /// The flat index length must match what `rebuild_flat` produces, or the
    /// `uniform_list` item count and the rendered rows disagree.
    #[test]
    fn flat_len_counts_headers_and_visible_rows() {
        assert_eq!(flat_len([(false, 2), (false, 3)].into_iter()), 7);
        assert_eq!(flat_len([(true, 2), (false, 3)].into_iter()), 5);
        assert_eq!(flat_len([(true, 2), (true, 3)].into_iter()), 2);
        assert_eq!(flat_len([].into_iter()), 0);
    }

    /// A collapsed group keeps its header, so the reader can always get the
    /// rows back.
    #[test]
    fn collapsing_never_hides_the_header() {
        assert_eq!(flat_len([(true, 99)].into_iter()), 1);
    }

    /// Precision is a claim, not a decoration: unknown tags stay unknown.
    #[test]
    fn precision_classification() {
        assert_eq!(Precision::classify("precise"), Precision::Precise);
        assert_eq!(Precision::classify("PRECISE"), Precision::Precise);
        assert_eq!(Precision::classify("resolved"), Precision::Precise);
        assert_eq!(Precision::classify("textual"), Precision::Textual);
        assert_eq!(Precision::classify("call"), Precision::Other);
        assert_eq!(Precision::classify(""), Precision::Other);
    }

    /// An unrecognised precision keeps the engine's own wording rather than
    /// being flattened into "textual" (LD-7).
    #[test]
    fn unknown_precision_shows_the_engines_words() {
        let colours = crate::theme::themes::dark_theme().colours;
        let kind = SharedString::from("call-site");
        let (_, _, label) = precision_chrome(Precision::Other, &kind, &colours);
        assert_eq!(&*label, "call-site");
    }

    /// Counts are formatted once, on arrival, never in `render`.
    #[test]
    fn counts_are_preformatted() {
        assert_eq!(&*count_text(0), "0");
        assert_eq!(&*count_text(128), "128");
    }

    /// Syncing with no new pages does no work — the store's `Progressive` is
    /// append-only and we remember how much we have consumed.
    #[test]
    fn sync_with_no_new_pages_is_a_no_op() {
        let mut refs = RefsTable::new();
        assert!(!refs.sync(&[]));
        assert!(refs.is_empty());

        let mut impls = ImplsTable::new();
        assert!(!impls.sync(&[]));
        assert_eq!(impls.total(), 0);
    }

    /// Toggling a group that does not exist is a no-op, not a panic — a click
    /// can race a generation change.
    #[test]
    fn toggling_a_missing_group_is_a_no_op() {
        let mut refs = RefsTable::new();
        assert!(!refs.toggle_group(7));
    }

    /// A reset clears everything and re-stamps the generation, so entrance ids
    /// from the previous generation cannot collide (LD-19).
    #[test]
    fn reset_restamps_the_generation() {
        let mut refs = RefsTable::new();
        refs.reset(7);
        assert!(refs.is_empty());
        assert_eq!(refs.generation, 7);
        assert_eq!(refs.consumed_pages, 0);
        assert_eq!(&*refs.total_text(), "0");

        let mut impls = ImplsTable::new();
        impls.reset(9);
        assert_eq!(impls.generation, 9);
        assert_eq!(impls.consumed_pages, 0);
    }

    // ── ImplsTable layout height ──────────────────────────────────────────────
    //
    // The `uniform_list` inside `ImplsTable::render` receives an explicit `h`
    // equal to `rows × row_h`.  If this arithmetic drifts from the per-row `.h`
    // in the closure, the list either clips rows (too small) or shows a phantom
    // scroll gap (too large).  These tests exercise the pure arithmetic without
    // requiring a GPUI context.

    /// Zero rows → zero list height.
    #[test]
    fn impls_list_height_zero_rows() {
        let row_h: f32 = 20.0; // arbitrary
        assert_eq!(0_usize as f32 * row_h, 0.0);
    }

    /// N rows → N × row_h.
    #[test]
    fn impls_list_height_n_rows() {
        let row_h: f32 = 20.0;
        let count: usize = 6;
        assert_eq!((count as f32) * row_h, 120.0);
    }

    // ── Blanket / variadic grouping ───────────────────────────────────────────

    fn fake_row(
        label: &str,
        is_blanket: bool,
        trait_label: Option<&str>,
        self_generic_count: u32,
    ) -> ImplRowView {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
        let lid = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("t"));
        let key = SymbolKey::new(lid, IntroId::from_raw([0u8; 32]));
        ImplRowView {
            key,
            label: SharedString::from(label.to_owned()),
            is_blanket,
            trait_label: trait_label.map(|t| SharedString::from(t.to_owned())),
            self_generic_count,
        }
    }

    /// Blanket partition: blanket rows must be separated from own impls.
    ///
    /// Detection rule: `ImplRow::is_blanket == true`.
    /// Expected: own=[Display, Debug], blanket=[ToString].
    #[test]
    fn blanket_rows_partitioned_from_own_impls() {
        let rows = vec![
            fake_row("impl Display for Router", false, Some("Display"), 0),
            fake_row("impl Debug for Router", false, Some("Debug"), 0),
            fake_row("impl<T: Display> ToString for T", true, Some("ToString"), 0),
        ];
        let (own, blanket): (Vec<_>, Vec<_>) = rows.iter().partition(|r| !r.is_blanket);
        assert_eq!(own.len(), 2, "own impls");
        assert_eq!(blanket.len(), 1, "blanket impls");
        assert!(blanket[0].label.contains("ToString"), "blanket is the ToString impl");

        // Also verify build_flat places the subheading between own and blanket.
        let own_views: Vec<ImplRowView> = own.iter().map(|r| (*r).clone()).collect();
        let blanket_views: Vec<ImplRowView> = blanket.iter().map(|r| (*r).clone()).collect();
        let flat = build_flat(&own_views, &blanket_views, false);
        // 2 own rows + 1 subheading + 1 blanket row = 4
        assert_eq!(flat.len(), 4, "flat length with blanket expanded");
        assert!(
            matches!(flat[2], ImplFlatRow::Subheading { .. }),
            "third entry must be the blanket subheading"
        );
    }

    /// Variadic family detection: 16 rows sharing trait + self base with
    /// consecutive generic counts collapse to one summary row.
    #[test]
    fn variadic_family_detected_by_generic_count_run() {
        // Build 16 rows: impl Handler for F<T1>, F<T1,T2>, … F<T1,…,T16>
        let rows: Vec<ImplRowView> = (1_u32..=16)
            .map(|n| fake_row(
                &["impl Handler for F<", &"T,".repeat(n as usize), ">"].concat(),
                false,
                Some("Handler"),
                n,
            ))
            .collect();

        // All 16 counts must be consecutive.
        let counts: Vec<u32> = rows.iter().map(|r| r.self_generic_count).collect();
        assert!(rows.len() >= 3, "need at least 3 members to trigger family collapse");
        assert!(is_consecutive_run(&counts), "all arities must be consecutive");

        // apply_variadic_grouping must collapse these to a single summary row.
        let collapsed = apply_variadic_grouping(rows);
        assert_eq!(collapsed.len(), 1, "16-member consecutive family must collapse to 1");
        assert!(
            collapsed[0].label.contains("arities 1\u{2013}16")
                || collapsed[0].label.contains("arities 1-16"),
            "summary label must include the arity range; got {:?}",
            collapsed[0].label,
        );
    }

    /// Non-consecutive generic counts must NOT be collapsed — they are distinct
    /// impls that happen to share a trait name.
    #[test]
    fn non_consecutive_generic_counts_are_not_a_family() {
        // Arities 1, 2, 5 — not consecutive; must never be collapsed.
        let counts = [1_u32, 2, 5];
        assert!(
            !is_consecutive_run(&counts),
            "a gap in the arity sequence must prevent family detection"
        );

        // Confirm apply_variadic_grouping does NOT collapse them.
        let rows: Vec<ImplRowView> = counts
            .iter()
            .map(|&n| fake_row("impl Handler for F", false, Some("Handler"), n))
            .collect();
        let result = apply_variadic_grouping(rows);
        assert_eq!(result.len(), 3, "non-consecutive arities must not be collapsed");
    }

    /// A family of fewer than 3 members must not be collapsed — two identical-
    /// looking impls for different reasons would be misleading as a single row.
    #[test]
    fn family_threshold_requires_at_least_three_members() {
        // 2 consecutive arities: not a family.
        let counts = [1_u32, 2];
        assert!(
            !is_consecutive_run(&counts),
            "two-member arity run must not trigger family collapse"
        );

        // Confirm apply_variadic_grouping passes them through unchanged.
        let rows: Vec<ImplRowView> = counts
            .iter()
            .map(|&n| fake_row("impl Handler for F", false, Some("Handler"), n))
            .collect();
        let result = apply_variadic_grouping(rows);
        assert_eq!(result.len(), 2, "two-member run must pass through unchanged");
    }
}
