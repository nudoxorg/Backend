//! Reading a drawn frame the way a person does: what is on screen and where
//! (areas from the shell's resolved frame), what the words point at, rows of
//! words as one line, and routes as exact words.

use super::script::{Area, Pick, glob};
use crate::navigation::{OrbitRoute, PackageLane, Route};
use crate::shell::Frame;
use backend_gui_harness::{Drawn, Viewport};
use facet::probe::{BoundsSample, Ledger, TargetSample, TextSample};

/// The frame just drawn.
pub(super) struct Seen {
    /// What the frame carried.
    pub drawn: Drawn,
    /// What the probe saw.
    pub ledger: Ledger,
    /// Drawn with every read landed, and its render issued none.
    pub complete: bool,
    /// The shell's resolved layout (bars, shelf, pins).
    pub frame: Option<Frame>,
}

/// `(left, top, right, bottom)` of `area` in logical px.
fn rect(area: Area, frame: Option<&Frame>, viewport: Viewport) -> (f32, f32, f32, f32) {
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (viewport.width as f32, viewport.height as f32);
    let Some(frame) = frame else {
        return (0.0, 0.0, width, height);
    };
    let (top, bottom) = (frame.titlebar, height - frame.status);
    let (shelf, pins) = (frame.shelf_width, width - frame.pins_width);
    match area {
        Area::Titlebar => (0.0, 0.0, width, top),
        Area::Status => (0.0, bottom, width, height),
        Area::Shelf => (0.0, top, shelf, bottom),
        Area::Reader => (shelf, top, pins, bottom),
        Area::Pins => (pins, top, width, bottom),
    }
}

/// Whether any of the box shows inside the window.
pub(super) fn visible(bounds: &BoundsSample, viewport: Viewport) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (viewport.width as f32, viewport.height as f32);
    bounds.width > 0.0
        && bounds.height > 0.0
        && bounds.x < width
        && bounds.y < height
        && bounds.x + bounds.width > 0.0
        && bounds.y + bounds.height > 0.0
}

fn centre(bounds: &BoundsSample) -> (f32, f32) {
    (bounds.x + bounds.width / 2.0, bounds.y + bounds.height / 2.0)
}

fn contains(bounds: &BoundsSample, (x, y): (f32, f32)) -> bool {
    x >= bounds.x && x <= bounds.x + bounds.width && y >= bounds.y && y <= bounds.y + bounds.height
}

impl Seen {
    fn viewport(&self) -> Viewport {
        self.drawn.viewport
    }

    /// Whether the box's centre lies in `area` (any area: on screen).
    pub(super) fn within(&self, bounds: &BoundsSample, area: Option<Area>) -> bool {
        if !visible(bounds, self.viewport()) {
            return false;
        }
        area.is_none_or(|area| {
            let (left, top, right, bottom) = rect(area, self.frame.as_ref(), self.viewport());
            let (x, y) = centre(bounds);
            x >= left && x < right && y >= top && y < bottom
        })
    }

    /// The visible texts in `area`, in paint order.
    pub(super) fn texts(&self, area: Option<Area>) -> Vec<&TextSample> {
        self.ledger
            .texts
            .iter()
            .filter(|text| self.within(&text.bounds, area))
            .collect()
    }

    /// Rows of visible words in `area`, each read left to right.
    pub(super) fn lines(&self, area: Option<Area>) -> Vec<String> {
        let mut texts = self.texts(area);
        texts.sort_by(|a, b| centre(&a.bounds).1.total_cmp(&centre(&b.bounds).1));
        let mut rows: Vec<Vec<&TextSample>> = Vec::new();
        for text in texts {
            let y = centre(&text.bounds).1;
            match rows.last_mut() {
                Some(row)
                    if row.iter().any(|other| {
                        (centre(&other.bounds).1 - y).abs()
                            <= 0.5 * other.bounds.height.min(text.bounds.height)
                    }) =>
                {
                    row.push(text);
                }
                _ => rows.push(vec![text]),
            }
        }
        rows.into_iter()
            .map(|mut row| {
                row.sort_by(|a, b| a.bounds.x.total_cmp(&b.bounds.x));
                row.iter().map(|text| text.content.as_str()).collect::<Vec<_>>().join(" ")
            })
            .collect()
    }

    /// The focused targets' keys.
    pub(super) fn focused(&self) -> Vec<&TargetSample> {
        self.ledger.targets.iter().filter(|target| target.state.focused).collect()
    }

    /// The visible text a [`Pick::Text`] names, or why not.
    fn words(&self, text: &str, after: Option<&str>, area: Option<Area>) -> Result<&TextSample, String> {
        let shown = self.texts(area);
        let from = match after {
            None => 0,
            Some(anchor) => {
                shown
                    .iter()
                    .position(|candidate| candidate.content == anchor)
                    .ok_or_else(|| format!("the anchor \"{anchor}\" is not on screen{}", area_words(area)))?
                    + 1
            }
        };
        shown[from..]
            .iter()
            .find(|candidate| candidate.content == text)
            .copied()
            .ok_or_else(|| {
                format!(
                    "\"{text}\"{} is not on screen{}; {}",
                    after.map_or_else(String::new, |anchor| format!(" after \"{anchor}\"")),
                    area_words(area),
                    self.summary(area)
                )
            })
    }

    /// The innermost target containing `point`.
    fn target_at(&self, point: (f32, f32)) -> Option<&TargetSample> {
        self.ledger
            .targets
            .iter()
            .filter(|target| contains(&target.bounds, point))
            .min_by(|a, b| {
                (a.bounds.width * a.bounds.height).total_cmp(&(b.bounds.width * b.bounds.height))
            })
    }

    /// Where a person points for `pick` (logical px), and what is there.
    pub(super) fn locate(&self, pick: &Pick) -> Result<((f32, f32), String), String> {
        match pick {
            Pick::Probe(probe) => {
                let mut matches = self
                    .ledger
                    .targets
                    .iter()
                    .filter(|target| glob(probe, &target.key))
                    .collect::<Vec<_>>();
                matches.dedup_by(|a, b| a.key == b.key && a.bounds == b.bounds);
                match matches.as_slice() {
                    [target] => {
                        let b = &target.bounds;
                        if !visible(b, self.viewport()) {
                            return Err(format!(
                                "target `{}` is outside the {}x{} window at ({:.0}, {:.0}) {:.0}x{:.0}",
                                short(&target.key),
                                self.viewport().width,
                                self.viewport().height,
                                b.x,
                                b.y,
                                b.width,
                                b.height
                            ));
                        }
                        let (x, y) = centre(b);
                        Ok((
                            (x.round(), y.round()),
                            format!("target `{}` at ({:.0}, {:.0}) {:.0}x{:.0}", short(&target.key), b.x, b.y, b.width, b.height),
                        ))
                    }
                    [] => Err(format!(
                        "no target matches `{probe}`; published: [{}]",
                        self.ledger
                            .targets
                            .iter()
                            .filter(|target| visible(&target.bounds, self.viewport()))
                            .map(|target| short(&target.key))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    many => Err(format!(
                        "`{probe}` matches {} targets: [{}]",
                        many.len(),
                        many.iter().map(|target| short(&target.key)).collect::<Vec<_>>().join(", ")
                    )),
                }
            }
            Pick::Text { text, after, area } => {
                let words = self.words(text, after.as_deref(), *area)?;
                let point = centre(&words.bounds);
                let Some(target) = self.target_at(point) else {
                    return Err(format!(
                        "\"{text}\" at ({:.0}, {:.0}) is not inside any published target: the words are not a link",
                        words.bounds.x, words.bounds.y
                    ));
                };
                Ok((
                    (point.0.round(), point.1.round()),
                    format!(
                        "\"{text}\" at ({:.0}, {:.0}) in target `{}`",
                        words.bounds.x,
                        words.bounds.y,
                        short(&target.key)
                    ),
                ))
            }
        }
    }

    /// Whether the one focused target is `pick`; the evidence either way.
    pub(super) fn focus_is(&self, pick: &Pick) -> Result<String, String> {
        let focused = self.focused();
        let names = || {
            focused
                .iter()
                .map(|target| format!("`{}`", short(&target.key)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let [target] = focused.as_slice() else {
            return Err(format!("{} targets focused: [{}]", focused.len(), names()));
        };
        match pick {
            Pick::Probe(probe) if glob(probe, &target.key) => Ok(format!("`{}`", short(&target.key))),
            Pick::Probe(_) => Err(format!("focused: [{}]", names())),
            Pick::Text { text, after, area } => {
                let words = self.words(text, after.as_deref(), *area)?;
                if contains(&target.bounds, centre(&words.bounds)) {
                    Ok(format!("`{}` holds \"{text}\"", short(&target.key)))
                } else {
                    Err(format!(
                        "focused: [{}], which does not hold \"{text}\" at ({:.0}, {:.0})",
                        names(),
                        words.bounds.x,
                        words.bounds.y
                    ))
                }
            }
        }
    }

    /// The visible texts in `area`, quoted (the evidence of a failed check).
    pub(super) fn summary(&self, area: Option<Area>) -> String {
        let list = self.texts(area);
        let shown = list
            .iter()
            .take(40)
            .map(|text| format!("\"{}\"", clip(&short(&text.content), 60)))
            .collect::<Vec<_>>();
        format!(
            "on screen{} ({} texts{}): [{}]",
            area_words(area),
            list.len(),
            if list.len() > 40 { ", first 40" } else { "" },
            shown.join(", ")
        )
    }

    /// Texts close to `wanted` (containing it, or contained in it).
    pub(super) fn near(&self, wanted: &str) -> Vec<String> {
        let lower = wanted.to_lowercase();
        self.ledger
            .texts
            .iter()
            .filter(|text| {
                let content = text.content.to_lowercase();
                content != lower
                    && (content.contains(&lower) || (lower.contains(&content) && content.len() >= 3))
            })
            .take(6)
            .map(|text| {
                format!(
                    "\"{}\" at ({:.0}, {:.0}){}",
                    clip(&short(&text.content), 80),
                    text.bounds.x,
                    text.bounds.y,
                    if visible(&text.bounds, self.viewport()) { "" } else { " outside the window" }
                )
            })
            .collect()
    }

    /// Everything visible and every target: the checkpoint's inventory.
    pub(super) fn inventory(&self, route: Option<&Route>) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "  route: {}", route.map_or_else(|| "(none)".to_owned(), |route| short(&describe(route))));
        if let Some(frame) = &self.frame {
            let _ = writeln!(
                out,
                "  frame: titlebar {:.0}, status {:.0}, shelf {:?} {:.0}, pins {:.0}",
                frame.titlebar, frame.status, frame.shelf, frame.shelf_width, frame.pins_width
            );
        }
        let _ = writeln!(out, "  texts (paint order; * = outside the window):");
        for text in &self.ledger.texts {
            let _ = writeln!(
                out,
                "   {}{:>5.0},{:<5.0} {:?}",
                if visible(&text.bounds, self.viewport()) { " " } else { "*" },
                text.bounds.x,
                text.bounds.y,
                short(&text.content)
            );
        }
        let _ = writeln!(out, "  targets (paint order; F = focused, * = outside the window):");
        for target in &self.ledger.targets {
            let _ = writeln!(
                out,
                "   {}{}{:>5.0},{:<5.0} {:>4.0}x{:<4.0} {}",
                if target.state.focused { "F" } else { " " },
                if visible(&target.bounds, self.viewport()) { " " } else { "*" },
                target.bounds.x,
                target.bounds.y,
                target.bounds.width,
                target.bounds.height,
                short(&target.key)
            );
        }
        out
    }
}

pub(super) fn area_words(area: Option<Area>) -> String {
    area.map_or_else(String::new, |area| format!(" in the {}", area.name()))
}

/// Shortens machine paths in quoted evidence (comparisons stay exact).
pub(super) fn short(text: &str) -> String {
    let mut out = text.to_owned();
    if let Ok(repo) = super::super::repo().canonicalize()
        && let Some(repo) = repo.to_str()
    {
        out = out.replace(repo, "<repo>");
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|home| home.into_string().ok()) {
        out = out.replace(&format!("{home}/.cargo/registry/src/"), "<registry>/");
    }
    out
}

/// At most `limit` characters, newlines shown.
pub(super) fn clip(text: &str, limit: usize) -> String {
    let flat = text.replace('\n', "⏎");
    if flat.chars().count() <= limit {
        flat
    } else {
        format!("{}…", flat.chars().take(limit).collect::<String>())
    }
}

/// A route as exact words: what a `route` check compares.
pub(super) fn describe(route: &Route) -> String {
    let at = |at: Option<&crate::navigation::ReleaseId>| {
        at.map_or_else(String::new, |at| format!(" at={}", at.as_str()))
    };
    match route {
        Route::Orbit(OrbitRoute::Home) => "orbit".to_owned(),
        Route::Orbit(OrbitRoute::Project(id)) => format!("orbit project {id}"),
        Route::World => "world".to_owned(),
        Route::Package(package) => format!(
            "package {}{}{}",
            package.package.as_str(),
            if package.lane == PackageLane::Overview {
                String::new()
            } else {
                format!(" lane={:?}", package.lane).to_lowercase()
            },
            at(package.at.as_ref())
        ),
        Route::Symbol(symbol) => format!(
            "symbol {} view={}{}{}",
            symbol.id.as_str(),
            symbol.view.as_str(),
            at(symbol.at.as_ref()),
            symbol.line.map_or_else(String::new, |line| format!(" line={line}"))
        ),
    }
}
