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
    /// Painted text lines no probe text covers (a control's own label, such
    /// as a button's): the words are on screen, so a person reads them and a
    /// pick may name them. Built by [`painted_extras`].
    pub painted: Vec<TextSample>,
    /// What the app is doing, in words (`zone reader`, `ask open`,
    /// `held 2`, `text 125`, `clipboard nudox://…`): [`state_words`].
    pub state: Vec<(String, String)>,
}

/// What the app is doing that a key can change, in words: the shell's
/// chrome (the keyboard's zone and target, Ask, a peek, hints, the hand,
/// zen, the shelf), the overlay, how many cards are held, the text scale,
/// the theme, and what the clipboard holds.
pub(super) fn state_words(cx: &mut gpui::App) -> Vec<(String, String)> {
    use facet::ActiveFacet as _;
    let Some(booted) = cx.try_global::<super::super::Booted>() else {
        return Vec::new();
    };
    let (shell, store) = (booted.shell.clone(), booted.graph.store.clone());
    let mut words: Vec<(String, String)> = shell
        .read(cx)
        .chrome_words(cx)
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let snapshot = store.read(cx).snapshot();
    let overlay = match snapshot.overlay() {
        None => "none".to_owned(),
        Some(crate::navigation::Overlay::Settings(page)) => format!("settings {}", page.as_str()),
        Some(crate::navigation::Overlay::AddProject) => "add-project".to_owned(),
        Some(crate::navigation::Overlay::CommandPalette) => "command-palette".to_owned(),
        Some(crate::navigation::Overlay::Inbox) => "inbox".to_owned(),
    };
    words.push(("overlay".to_owned(), overlay));
    words.push((
        "held".to_owned(),
        snapshot.session().hand.held().len().to_string(),
    ));
    let facet = cx.facet();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a text scale is 50..300 %"
    )]
    words.push((
        "text".to_owned(),
        ((facet.text_scale * 100.0).round() as u32).to_string(),
    ));
    words.push((
        "theme".to_owned(),
        format!("{:?}", facet.appearance).to_lowercase(),
    ));
    let clipboard = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    words.push(("clipboard".to_owned(), clipboard));
    words
}

/// The painted lines of a frame (gpui's text trace) that no probe text
/// already names: same words, overlapping box. Faded-out ink is not text a
/// person reads.
pub(super) fn painted_extras(ledger: &Ledger, painted: &[gpui::PaintedText]) -> Vec<TextSample> {
    painted
        .iter()
        .filter(|line| line.alpha > 0.05 && !line.text.trim().is_empty())
        .map(|line| {
            let bounds = BoundsSample {
                key: format!("painted:{}", line.text),
                x: f32::from(line.bounds.origin.x),
                y: f32::from(line.bounds.origin.y),
                width: f32::from(line.bounds.size.width),
                height: f32::from(line.bounds.size.height),
            };
            TextSample {
                key: bounds.key.clone(),
                paint_clip: None,
                natural_width: bounds.width,
                overflow: facet::probe::TextOverflow::Wrap,
                content: line.text.to_string(),
                min_width: 0.0,
                line_height: bounds.height,
                size: bounds.height,
                weight: 400.0,
                region: None,
                bounds,
            }
        })
        .filter(|extra| {
            !ledger.texts.iter().any(|text| {
                text.content.trim() == extra.content.trim()
                    && text.bounds.x < extra.bounds.x + extra.bounds.width
                    && extra.bounds.x < text.bounds.x + text.bounds.width
                    && text.bounds.y < extra.bounds.y + extra.bounds.height
                    && extra.bounds.y < text.bounds.y + text.bounds.height
            })
        })
        .collect()
}

/// `(left, top, right, bottom)` of `area` in logical px.
pub(super) fn rect(area: Area, frame: Option<&Frame>, viewport: Viewport) -> (f32, f32, f32, f32) {
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (viewport.width as f32, viewport.height as f32);
    let Some(frame) = frame else {
        return (0.0, 0.0, width, height);
    };
    let (top, bottom) = (f32::from(frame.titlebar), height - f32::from(frame.status));
    let (shelf, pins) = (
        f32::from(frame.shelf_width),
        width - f32::from(frame.pins_width),
    );
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
    (
        bounds.x + bounds.width / 2.0,
        bounds.y + bounds.height / 2.0,
    )
}

fn contains(bounds: &BoundsSample, (x, y): (f32, f32)) -> bool {
    x >= bounds.x && x <= bounds.x + bounds.width && y >= bounds.y && y <= bounds.y + bounds.height
}

/// The opaque plates a dialog lays over the page, each with the region its
/// own words are built in: a dialog stack entry keyed `REGION-plate` (Ask's
/// `ask-plate`) whose region has words on screen (an empty plate is not
/// drawn).
fn plates(ledger: &Ledger) -> Vec<(&str, &BoundsSample)> {
    ledger
        .stacks
        .iter()
        .flat_map(|stack| &stack.entries)
        .filter(|entry| entry.kind == "dialog" && entry.phase != facet::probe::StackPhase::Leaving)
        .filter_map(|entry| Some((entry.key.strip_suffix("-plate")?, entry.bounds.as_ref()?)))
        .filter(|(region, _)| {
            ledger
                .texts
                .iter()
                .any(|text| text.region.as_deref() == Some(*region))
        })
        .collect()
}

/// Whether `text` lies under a dialog's opaque plate (its centre on the
/// plate, and not one of the dialog's own words): it is not on screen.
pub(super) fn occluded(plates: &[(&str, &BoundsSample)], text: &TextSample) -> bool {
    plates.iter().any(|(region, plate)| {
        text.region.as_deref() != Some(*region) && contains(plate, centre(&text.bounds))
    })
}

/// The ledger as a person sees it: without the words a dialog's plate
/// covers (they are neither read nor linted).
pub(super) fn unoccluded(ledger: &Ledger) -> Ledger {
    let plates = plates(ledger);
    let mut seen = ledger.clone();
    seen.texts.retain(|text| !occluded(&plates, text));
    seen
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

    /// The visible texts in `area`, in paint order (a dialog's plate hides
    /// the words under it).
    pub(super) fn texts(&self, area: Option<Area>) -> Vec<&TextSample> {
        let plates = plates(&self.ledger);
        self.ledger
            .texts
            .iter()
            .chain(self.painted.iter())
            .filter(|text| !occluded(&plates, text))
            // Exact text assertions may only name a string that is completely
            // visible. A partially clipped sample proves only that some glyphs
            // painted, not that the whole semantic string was readable.
            .filter(|text| {
                let viewport = self.viewport();
                if !facet::gallery::lint::fully_visible(
                    text,
                    viewport.width as f32,
                    viewport.height as f32,
                ) {
                    return false;
                }
                let Some(visible) = facet::gallery::lint::visible_bounds(
                    text,
                    viewport.width as f32,
                    viewport.height as f32,
                ) else {
                    return false;
                };
                self.within(&visible, area)
            })
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
                row.iter()
                    .map(|text| text.content.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// The focused targets' keys.
    pub(super) fn focused(&self) -> Vec<&TargetSample> {
        self.ledger
            .targets
            .iter()
            .filter(|target| target.state.focused)
            .collect()
    }

    /// The visible text a [`Pick::Text`] names, or why not: the first
    /// `text` after each anchor in turn.
    fn words(
        &self,
        text: &str,
        after: &[String],
        area: Option<Area>,
    ) -> Result<&TextSample, String> {
        let shown = self.texts(area);
        let mut from = 0;
        for anchor in after {
            from += shown[from..]
                .iter()
                .position(|candidate| &candidate.content == anchor)
                .ok_or_else(|| {
                    format!(
                        "the anchor \"{anchor}\" is not on screen{}{}; {}",
                        if from > 0 {
                            " after the anchors before it"
                        } else {
                            ""
                        },
                        area_words(area),
                        self.summary(area)
                    )
                })?
                + 1;
        }
        shown[from..]
            .iter()
            .find(|candidate| candidate.content == text)
            .copied()
            .ok_or_else(|| {
                format!(
                    "\"{text}\"{} is not on screen{}; {}",
                    if after.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " after {}",
                            after
                                .iter()
                                .map(|anchor| format!("\"{anchor}\""))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    },
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

    /// Where a person points for `pick` (logical px), what is there (in
    /// words), and the target's key.
    pub(super) fn locate(&self, pick: &Pick) -> Result<((f32, f32), String, String), String> {
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
                            format!(
                                "target `{}` at ({:.0}, {:.0}) {:.0}x{:.0}",
                                short(&target.key),
                                b.x,
                                b.y,
                                b.width,
                                b.height
                            ),
                            target.key.clone(),
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
                        many.iter()
                            .map(|target| short(&target.key))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            Pick::Text { text, after, area } => {
                let words = self.words(text, after, *area)?;
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
                    target.key.clone(),
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
            Pick::Probe(probe) if glob(probe, &target.key) => {
                Ok(format!("`{}`", short(&target.key)))
            }
            Pick::Probe(_) => Err(format!("focused: [{}]", names())),
            Pick::Text { text, after, area } => {
                let words = self.words(text, after, *area)?;
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
                    && (content.contains(&lower)
                        || (lower.contains(&content) && content.len() >= 3))
            })
            .take(6)
            .map(|text| {
                format!(
                    "\"{}\" at ({:.0}, {:.0}){}",
                    clip(&short(&text.content), 80),
                    text.bounds.x,
                    text.bounds.y,
                    if visible(&text.bounds, self.viewport()) {
                        ""
                    } else {
                        " outside the window"
                    }
                )
            })
            .collect()
    }

    /// Everything visible and every target: the checkpoint's inventory.
    pub(super) fn inventory(&self, route: Option<&Route>) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "  route: {}",
            route.map_or_else(|| "(none)".to_owned(), |route| short(&describe(route)))
        );
        if let Some(frame) = &self.frame {
            let _ = writeln!(
                out,
                "  frame: titlebar {:.0}, status {:.0}, shelf {:?} {:.0}, pins {:.0}",
                f32::from(frame.titlebar),
                f32::from(frame.status),
                frame.shelf,
                f32::from(frame.shelf_width),
                f32::from(frame.pins_width)
            );
        }
        let _ = writeln!(out, "  texts (paint order; * = outside the window):");
        for text in &self.ledger.texts {
            let _ = writeln!(
                out,
                "   {}{:>5.0},{:<5.0} {:?}",
                if visible(&text.bounds, self.viewport()) {
                    " "
                } else {
                    "*"
                },
                text.bounds.x,
                text.bounds.y,
                short(&text.content)
            );
        }
        for extra in &self.painted {
            let _ = writeln!(
                out,
                "   {}{:>5.0},{:<5.0} {:?}  (painted only: no probe text)",
                if visible(&extra.bounds, self.viewport()) {
                    " "
                } else {
                    "*"
                },
                extra.bounds.x,
                extra.bounds.y,
                short(&extra.content)
            );
        }
        for stack in &self.ledger.stacks {
            for entry in &stack.entries {
                let _ = writeln!(
                    out,
                    "  float {} {} `{}` {:?}{}",
                    stack.layer,
                    entry.kind,
                    short(&entry.key),
                    entry.phase,
                    entry.bounds.as_ref().map_or_else(String::new, |b| format!(
                        " at ({:.0}, {:.0}) {:.0}x{:.0}",
                        b.x, b.y, b.width, b.height
                    ))
                );
            }
        }
        let _ = writeln!(
            out,
            "  targets (paint order; F = focused, * = outside the window):"
        );
        for target in &self.ledger.targets {
            let _ = writeln!(
                out,
                "   {}{}{:>5.0},{:<5.0} {:>4.0}x{:<4.0} {}",
                if target.state.focused { "F" } else { " " },
                if visible(&target.bounds, self.viewport()) {
                    " "
                } else {
                    "*"
                },
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
        Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Tree(project))) => {
            format!("tree {}", project.display_lossy())
        }
        Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::FindHome)) => "find".into(),
        Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query))) => {
            format!("find {}", query.text)
        }
        Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Compare(selection))) => {
            format!(
                "compare {}",
                selection
                    .packages()
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
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
            symbol
                .line
                .map_or_else(String::new, |line| format!(" line={line}"))
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::unoccluded;
    use facet::probe::{
        BoundsSample, Ledger, StackEntry, StackPhase, StackSample, TextOverflow, TextSample,
    };

    fn at(key: &str, x: f32, y: f32, width: f32, height: f32) -> BoundsSample {
        BoundsSample {
            key: key.to_owned(),
            x,
            y,
            width,
            height,
        }
    }

    fn text(content: &str, x: f32, y: f32, region: Option<&str>) -> TextSample {
        TextSample {
            key: format!("text:{content}"),
            bounds: at(content, x, y, 80.0, 16.0),
            paint_clip: None,
            natural_width: 80.0,
            overflow: TextOverflow::Clip,
            content: content.to_owned(),
            min_width: 40.0,
            line_height: 16.0,
            size: 13.0,
            weight: 400.0,
            region: region.map(ToOwned::to_owned),
        }
    }

    fn ledger(texts: Vec<TextSample>) -> Ledger {
        let dialog = |key: &str, bounds: BoundsSample| StackEntry {
            key: key.to_owned(),
            kind: "dialog".to_owned(),
            parent: None,
            phase: StackPhase::Open,
            pinned: false,
            bounds: Some(bounds),
        };
        Ledger {
            texts,
            stacks: vec![StackSample {
                layer: "shell".to_owned(),
                entries: vec![
                    dialog("ask-field", at("ask-field", 0.0, 0.0, 1440.0, 50.0)),
                    dialog("ask-plate", at("ask-plate", 0.0, 50.0, 440.0, 824.0)),
                ],
            }],
            ..Ledger::default()
        }
    }

    fn words(ledger: &Ledger) -> Vec<&str> {
        ledger
            .texts
            .iter()
            .map(|text| text.content.as_str())
            .collect()
    }

    /// Ask's plate covers the shelf: the shelf's words under it are not on
    /// screen, Ask's own words and the query in the titlebar are, and so is
    /// the page beside the plate.
    #[test]
    fn a_dialogs_plate_hides_the_words_under_it_and_only_those() {
        let open = ledger(vec![
            text("Library", 20.0, 90.0, None),
            text("Searching the library…", 20.0, 90.0, Some("ask")),
            text("toml Value", 560.0, 16.0, None),
            text("toml_pin", 800.0, 300.0, None),
        ]);
        assert_eq!(
            words(&unoccluded(&open)),
            ["Searching the library…", "toml Value", "toml_pin"]
        );
        // No words of Ask's own: the plate is not drawn, nothing is hidden.
        let empty = ledger(vec![
            text("Library", 20.0, 90.0, None),
            text("toml_pin", 800.0, 300.0, None),
        ]);
        assert_eq!(words(&unoccluded(&empty)), ["Library", "toml_pin"]);
    }
}
