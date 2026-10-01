//! J12's crawler: from where the journey stands, every target the reader
//! publishes is visited the way a person would.
//!
//! - **Hover**: the pointer rests on it. If a card rises (a tip, a peek, a
//!   lens, a menu: the float layer's stack), it must be showing within its
//!   rest and a quick entrance (`POPOVER_MS`), stay inside the window, and
//!   not cover the point it was raised from. When the pointer leaves, it
//!   must be gone within `GONE_MS`.
//! - **Click**: the page it opens must draw: no fault plate (`… could not be
//!   read.`, `READ-…`), not an empty reader, and no lint.
//! - **Back** (⌘[): the route comes back, and the keyboard stands on the
//!   target that was clicked.
//!
//! It goes `depth` pages deep, `per_page` targets on each, and never visits
//! a route twice. Every problem is named with the route and the target.

use super::Runner;
use super::look::Seen;
use backend_gui_harness::{Act, Button};
use facet::gallery::lint;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// A card must be showing this soon after the pointer rests (a cold card's
/// 350 ms rest, then its first 150 ms of entrance).
const POPOVER_MS: u64 = 500;
/// A card must be gone this soon after the pointer leaves it.
const GONE_MS: u64 = 400;
/// How long a hover is watched for a card.
const WATCH_MS: u64 = 700;

/// What `crawl DEPTH [per N]` asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Crawl {
    /// How many pages deep a click is followed.
    pub depth: u8,
    /// How many targets of each page are tried.
    pub per_page: usize,
}

/// One target a person can point at, as the frame published it.
#[derive(Clone, Debug)]
struct Spot {
    key: String,
    at: (f32, f32),
    bounds: facet::probe::BoundsSample,
}

/// What the crawl saw: every problem, and how much it covered.
#[derive(Debug, Default)]
pub(super) struct Crawled {
    pub problems: Vec<String>,
    pub pages: usize,
    pub hovers: usize,
    pub cards: usize,
    pub clicks: usize,
}

/// The fault plate's words, or an empty reader, or the frame's lints.
fn page_problems(seen: &Seen, image: &image::RgbaImage) -> Vec<String> {
    let mut problems = Vec::new();
    let texts = seen.texts(Some(super::Area::Reader));
    if texts.is_empty() {
        problems.push("the reader is empty".to_owned());
    }
    for text in &texts {
        if text.content.ends_with(" could not be read.") || text.content.starts_with("READ-") {
            problems.push(format!("a fault plate: \"{}\"", text.content));
        }
    }
    let linted = lint::lint(
        image,
        &super::look::unoccluded(&seen.ledger),
        seen.drawn.viewport,
    );
    for item in linted.lints.iter().take(6) {
        problems.push(format!(
            "lint {} {}: {}",
            item.rule.name(),
            super::look::short(&item.key),
            super::look::short(&item.detail)
        ));
    }
    problems
}

/// The reader's targets a pointer can reach, in paint order, once each.
fn spots(seen: &Seen) -> Vec<Spot> {
    let mut keys = BTreeSet::new();
    seen.ledger
        .targets
        .iter()
        .filter(|target| seen.within(&target.bounds, Some(super::Area::Reader)))
        .filter(|target| target.bounds.width >= 4.0 && target.bounds.height >= 4.0)
        .filter(|target| keys.insert(target.key.clone()))
        .map(|target| Spot {
            key: target.key.clone(),
            at: (
                (target.bounds.x + target.bounds.width / 2.0).round(),
                (target.bounds.y + target.bounds.height / 2.0).round(),
            ),
            bounds: target.bounds.clone(),
        })
        .collect()
}

/// The float layer's cards on screen, with their boxes.
fn cards(seen: &Seen) -> Vec<(String, facet::probe::BoundsSample)> {
    seen.ledger
        .stacks
        .iter()
        .filter(|stack| stack.layer == "float")
        .flat_map(|stack| &stack.entries)
        .filter(|entry| entry.phase != facet::probe::StackPhase::Leaving)
        .filter_map(|entry| {
            entry.bounds.clone().map(|bounds| {
                (
                    format!("{} {}", entry.kind, super::look::short(&entry.key)),
                    bounds,
                )
            })
        })
        .collect()
}

impl Runner {
    /// Crawls from where the journey stands; the report gets one line per
    /// page, and the problems come back.
    pub(super) fn crawl(&mut self, crawl: Crawl, report: &mut String) -> Result<Crawled, String> {
        let mut crawled = Crawled::default();
        let mut visited = BTreeSet::new();
        self.crawl_page(crawl, crawl.depth, &mut visited, &mut crawled, report)?;
        Ok(crawled)
    }

    fn crawl_page(
        &mut self,
        crawl: Crawl,
        depth: u8,
        visited: &mut BTreeSet<String>,
        crawled: &mut Crawled,
        report: &mut String,
    ) -> Result<(), String> {
        if let Err(why) = self.settle()? {
            crawled.problems.push(format!(
                "{}: not still: {why}",
                super::look::short(&self.route_words()?)
            ));
        }
        let route = self.route_words()?;
        if !visited.insert(route.clone()) {
            return Ok(());
        }
        crawled.pages += 1;
        let image = self
            .tick(true)?
            .ok_or_else(|| "the crawl's frame was not captured".to_owned())?;
        let found = page_problems(self.seen()?, &image);
        let _ = writeln!(
            report,
            "           crawl {}  {}",
            super::look::short(&route),
            if found.is_empty() {
                "ok".to_owned()
            } else {
                format!("{} problem(s)", found.len())
            }
        );
        crawled.problems.extend(
            found
                .into_iter()
                .map(|problem| format!("{}: {problem}", super::look::short(&route))),
        );
        if depth == 0 {
            return Ok(());
        }
        let spots: Vec<Spot> = spots(self.seen()?)
            .into_iter()
            .take(crawl.per_page)
            .collect();
        for spot in spots {
            let at = format!(
                "{} › {}",
                super::look::short(&route),
                super::look::short(&spot.key)
            );
            self.hover(&spot, &at, crawled)?;
            // Click, and read the page it opened.
            self.deliver(
                &[Act::Click {
                    x: spot.at.0,
                    y: spot.at.1,
                    button: Button::Left,
                }],
                &format!("crawl click {}", spot.key),
            )?;
            crawled.clicks += 1;
            if let Err(why) = self.settle()? {
                crawled
                    .problems
                    .push(format!("{at}: after the click, not still: {why}"));
            }
            let opened = self.route_words()?;
            if opened == route {
                // It acted in place (a fold, a toggle): put whatever it
                // opened away, and read the page again.
                self.deliver(
                    &[Act::Key {
                        chord: "escape".to_owned(),
                    }],
                    "crawl escape",
                )?;
                let _ = self.settle()?;
                let image = self
                    .tick(true)?
                    .ok_or_else(|| "the crawl's frame was not captured".to_owned())?;
                let found = page_problems(self.seen()?, &image);
                crawled.problems.extend(
                    found
                        .into_iter()
                        .map(|problem| format!("{at} (in place): {problem}")),
                );
                continue;
            }
            self.crawl_page(crawl, depth - 1, visited, crawled, report)?;
            // Back, to the route it left, standing on what was clicked.
            self.deliver(
                &[Act::Key {
                    chord: "cmd-[".to_owned(),
                }],
                "crawl back",
            )?;
            if let Err(why) = self.settle()? {
                crawled
                    .problems
                    .push(format!("{at}: after back, not still: {why}"));
            }
            let back = self.route_words()?;
            if back != route {
                crawled.problems.push(format!(
                    "{at}: back went to `{}`, not the page it was clicked on",
                    super::look::short(&back)
                ));
                // Find the way home before going on.
                return Ok(());
            }
            // The keyboard stands on it: on its own target, or on the door
            // laid over it (one box, two published targets).
            let seen = self.seen()?;
            let on_it = seen.focused().iter().any(|target| {
                target.key == spot.key || super::same_box(&target.bounds, &spot.bounds)
            });
            let focused: Vec<String> = seen
                .focused()
                .iter()
                .map(|target| target.key.clone())
                .collect();
            if !on_it {
                crawled.problems.push(format!(
                    "{at}: back did not restore the focus to it (focused: {focused:?})"
                ));
            }
        }
        Ok(())
    }

    /// Rests the pointer on a spot and judges the card it raises, if any.
    fn hover(&mut self, spot: &Spot, at: &str, crawled: &mut Crawled) -> Result<(), String> {
        let before: Vec<String> = cards(self.seen()?)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        self.deliver(
            &[Act::Move {
                x: spot.at.0,
                y: spot.at.1,
            }],
            &format!("crawl hover {}", spot.key),
        )?;
        crawled.hovers += 1;
        let started = self.now();
        let mut risen = None;
        while self.now() < started + WATCH_MS {
            self.tick(false)?;
            let now: Vec<(String, facet::probe::BoundsSample)> = cards(self.seen()?)
                .into_iter()
                .filter(|(key, _)| !before.contains(key))
                .collect();
            if let Some((key, bounds)) = now.into_iter().next() {
                risen = Some((key, bounds, self.now() - started));
                break;
            }
        }
        let Some((key, bounds, after)) = risen else {
            return Ok(());
        };
        crawled.cards += 1;
        if after > POPOVER_MS {
            crawled.problems.push(format!("{at}: its card `{key}` showed {after} ms after the pointer rested (at most {POPOVER_MS})"));
        }
        let viewport = self.seen()?.drawn.viewport;
        #[allow(clippy::cast_precision_loss, reason = "a window is far below 2^24 px")]
        let (width, height) = (viewport.width as f32, viewport.height as f32);
        if bounds.x < -0.5
            || bounds.y < -0.5
            || bounds.x + bounds.width > width + 0.5
            || bounds.y + bounds.height > height + 0.5
        {
            crawled.problems.push(format!("{at}: its card `{key}` leaves the {width}x{height} window: {:.0},{:.0} {:.0}x{:.0}", bounds.x, bounds.y, bounds.width, bounds.height));
        }
        let (x, y) = spot.at;
        if x > bounds.x
            && x < bounds.x + bounds.width
            && y > bounds.y
            && y < bounds.y + bounds.height
        {
            crawled.problems.push(format!(
                "{at}: its card `{key}` covers the point it was raised from"
            ));
        }
        // The pointer leaves: the card goes.
        self.deliver(&[Act::Leave], "crawl leave")?;
        let left = self.now();
        let mut gone = None;
        while self.now() < left + WATCH_MS {
            self.tick(false)?;
            if !cards(self.seen()?).iter().any(|(other, _)| *other == key) {
                gone = Some(self.now() - left);
                break;
            }
        }
        match gone {
            Some(after) if after <= GONE_MS => {}
            Some(after) => crawled.problems.push(format!("{at}: its card `{key}` was gone only {after} ms after the pointer left (at most {GONE_MS})")),
            None => crawled.problems.push(format!("{at}: its card `{key}` was still up {WATCH_MS} ms after the pointer left")),
        }
        Ok(())
    }
}
