//! Render-path instrumentation (GUI-PLAN §26).
//!
//! # Why this module had to exist before any optimisation could be claimed
//!
//! It was a one-line skeleton. Nothing in this crate measured anything, and the
//! only wall-clock number the project produced near rendering was the
//! screenshot harness's `wall_ms` — which starts its timer *inside* `shoot()`,
//! after the settle loop and every `window.refresh()` have already returned. It
//! therefore times `capture_screenshot` + the frame-stats scan + PNG encode of a
//! 2880×1800 image, and nothing at all about layout or paint. Reading it as
//! "how long the frame took" is off by the whole subject.
//!
//! AGENTS-DOCTRINE §6 forbids describing intended behaviour as completed
//! behaviour, and §8 requires verifying that the thing being measured actually
//! loaded. A perf claim taken from `wall_ms` would have failed both. So the
//! measurement plane is built first, the baseline is taken through it, and only
//! then is anything optimised.
//!
//! # What it measures
//!
//! Wall time spent inside named regions of the render path, aggregated by
//! label across every frame since the last [`drain`]. A GPUI view's `render`
//! runs many times per user-visible interaction (the screenshot harness alone
//! draws ~22 frames per scene), so a single frame's number is noise; count,
//! total and max together are not.
//!
//! ```rust,ignore
//! impl Render for OmniSearch {
//!     fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
//!         let _span = perf::scope(perf::Region::OmniSearch);
//!         // …
//!     }
//! }
//! ```
//!
//! # Why a region is an enum and not a `&'static str`
//!
//! This module's first version keyed the ledger on the *pointer* of a
//! `pub const LABEL: &str`, on the theory that identical consts share an
//! address and a pointer compare beats a string hash on a hot path. That
//! theory is false, and the compiler said so within a minute: a span recorded
//! under `SHELL_RENDER` could not be found again by looking it up with
//! `SHELL_RENDER`, because a `const` is inlined at each use site and the two
//! materialisations of the same literal did not share an address.
//!
//! It is exactly the failure mode the design was trying to avoid, arriving
//! from the other direction — not "two labels merge into one row" but "one
//! label splits into rows that cannot find each other". A measurement plane
//! that silently drops half its samples is worse than none, because the
//! numbers still look like numbers.
//!
//! [`Region`] is the fix doctrine §3 asks for: a closed enum instead of a
//! stringly-typed key. The ledger becomes a fixed array indexed by
//! discriminant — O(1), no search, no linker behaviour to depend on — and
//! adding a region is a compile error until every `match` here handles it.
//!
//! # Why it is off by default
//!
//! [`enabled`] gates every sample behind one relaxed atomic load. Off, a scope
//! costs that load and nothing else — no clock read, no allocation. The
//! harness turns it on explicitly; a user's session never pays for a
//! measurement nobody will read. `NUDOX_PERF=1` turns it on for a real run.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Regions
// ─────────────────────────────────────────────────────────────────────────────

/// A named region of the render path.
///
/// Closed on purpose (no `#[non_exhaustive]`): this enum is internal to one
/// package, and the value of it being exhaustive is that adding a region
/// breaks [`Region::ALL`] and [`Region::label`] at compile time rather than
/// producing a row nobody named. Same reasoning as `ProducerError` in
/// AGENTS-DOCTRINE §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Region {
    /// `Shell::render` — the whole window, once per frame.
    Shell,
    /// `OmniSearch::render` — the search overlay, including every visible row.
    OmniSearch,
    /// One search result row (`omni_search::render_row`), per visible row.
    OmniSearchRow,
    /// `SymbolPage::render` — header, document body, sections, outline rail.
    SymbolPage,
    /// One documentation section (`DocsBody::render_section`).
    DocsSection,
    /// `CommandOverlay::render` — the palette / cheat sheet.
    CommandOverlay,
    /// `ProjectPanel::render` — the left dock.
    ProjectPanel,
}

impl Region {
    /// Every region, in the order a report should read them.
    pub const ALL: [Region; 7] = [
        Region::Shell,
        Region::ProjectPanel,
        Region::OmniSearch,
        Region::OmniSearchRow,
        Region::SymbolPage,
        Region::DocsSection,
        Region::CommandOverlay,
    ];

    /// The name this region is reported under.
    pub fn label(self) -> &'static str {
        match self {
            Region::Shell => "shell.render",
            Region::OmniSearch => "omni_search.render",
            Region::OmniSearchRow => "omni_search.row",
            Region::SymbolPage => "symbol_page.render",
            Region::DocsSection => "symbol_page.docs_section",
            Region::CommandOverlay => "command_overlay.render",
            Region::ProjectPanel => "project_panel.render",
        }
    }

    /// Index into the ledger array.
    #[inline(always)]
    fn slot(self) -> usize {
        self as usize
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Enablement
// ─────────────────────────────────────────────────────────────────────────────

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether sampling is on.
///
/// Read once per [`scope`]. `Relaxed` is the correct ordering: a sample taken
/// one frame either side of the flag flipping is not a correctness question,
/// and anything stronger would put a fence on the render path to protect a
/// measurement.
#[inline(always)]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turn sampling on or off. Called by the screenshot harness and by
/// [`init_from_env`].
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Enable sampling if `NUDOX_PERF` is set to anything other than `0`.
///
/// Called from `main.rs` so a real session can be profiled without a rebuild.
pub fn init_from_env() {
    if let Ok(value) = std::env::var("NUDOX_PERF") {
        set_enabled(value != "0" && !value.is_empty());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ledger
// ─────────────────────────────────────────────────────────────────────────────

/// One label's accumulated cost since the last [`drain`].
///
/// `total` and `count` are kept rather than a running mean because a mean
/// cannot be re-aggregated across drains and a total can. `max` is kept
/// separately because the tail is what a reader feels: a 60 fps mean with one
/// 40 ms frame in it is a visible hitch, and the mean hides it.
///
/// # Why `self_total` exists, and why it is the number to compare
///
/// Spans nest, and a refactor can move a region from outside a parent to
/// inside it without changing any real cost. That happened here: the symbol
/// page's sections used to be built by the virtualized `list()`'s render
/// closure, which GPUI invokes during *layout*, outside `SymbolPage::render`;
/// in flow mode they are built by `page_body`, inside it. Comparing `total`
/// across that change reports a large regression in `symbol_page.render` and a
/// large improvement in `symbol_page.docs_section`, and both are artefacts.
///
/// `self_total` excludes time attributed to nested spans, so it answers "how
/// much work does this region itself do" regardless of who calls whom. That is
/// the figure a before/after belongs on. `total` is kept because it is what
/// answers "how long does opening this page take", which is a different and
/// also real question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Which region this row is for.
    pub region: Region,
    /// How many times the region was entered.
    pub count: u32,
    /// Summed wall time across all entries, nested spans included.
    pub total: Duration,
    /// Summed wall time with nested spans' time subtracted out.
    pub self_total: Duration,
    /// The slowest single entry, nested spans included.
    pub max: Duration,
}

impl Sample {
    /// Mean inclusive time per entry, in milliseconds. Zero when nothing was
    /// recorded.
    pub fn mean_ms(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total.as_secs_f64() * 1000.0 / f64::from(self.count)
    }

    /// Mean *exclusive* time per entry — the comparable figure. See the type's
    /// documentation for why.
    pub fn self_mean_ms(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.self_total.as_secs_f64() * 1000.0 / f64::from(self.count)
    }

    /// Slowest single entry, in milliseconds.
    pub fn max_ms(&self) -> f64 {
        self.max.as_secs_f64() * 1000.0
    }
}

thread_local! {
    /// Samples for the thread that renders.
    ///
    /// Thread-local rather than a `Mutex`: GPUI renders on one thread, so a
    /// lock would be uncontended overhead protecting nothing. The consequence
    /// is stated rather than hidden — [`drain`] returns what *this* thread
    /// recorded, which for the render path is all of it.
    static LEDGER: RefCell<[Slot; Region::ALL.len()]> =
        const { RefCell::new([Slot::EMPTY; Region::ALL.len()]) };
}

/// One ledger cell: the accumulated cost of one [`Region`].
///
/// A plain array indexed by discriminant rather than a map, so recording is an
/// index and three adds. `count == 0` means "not entered since the last
/// drain", which is what keeps an unvisited region out of the report rather
/// than printing a row of zeroes at it.
#[derive(Clone, Copy, Default)]
struct Slot {
    count: u32,
    total: Duration,
    self_total: Duration,
    max: Duration,
}

impl Slot {
    const EMPTY: Slot = Slot {
        count: 0,
        total: Duration::ZERO,
        self_total: Duration::ZERO,
        max: Duration::ZERO,
    };
}

thread_local! {
    /// Time spent inside spans opened *since the currently-open span started*.
    ///
    /// Each [`Span`] saves this on entry, zeroes it, and on drop reads it back
    /// as "my children's time", then restores the parent's value plus its own
    /// full elapsed. That is the whole nesting model: one `Cell<Duration>` and
    /// no stack, because a span's own frame already stores the parent's value.
    static CHILD_TIME: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

fn record(region: Region, elapsed: Duration, self_time: Duration) {
    LEDGER.with(|ledger| {
        // `try_borrow_mut` rather than `borrow_mut`: a scope opened inside
        // another scope's drop would panic on a re-entrant borrow, and dropping
        // one sample is the right price for never taking down a window over
        // instrumentation. The borrow is held for a handful of instructions, so
        // this cannot happen today — but it is exactly the kind of thing a
        // future nested span would discover the hard way.
        let Ok(mut ledger) = ledger.try_borrow_mut() else {
            return;
        };
        let slot = &mut ledger[region.slot()];
        slot.count += 1;
        slot.total += elapsed;
        slot.self_total += self_time;
        if elapsed > slot.max {
            slot.max = elapsed;
        }
    });
}

/// Take and clear everything recorded on this thread.
///
/// Rows come back in [`Region::ALL`] order — outermost region first, which is
/// the order a reader wants them in. A region never entered is omitted rather
/// than reported as zero, so the report says what actually ran.
pub fn drain() -> Vec<Sample> {
    LEDGER.with(|ledger| {
        let Ok(mut ledger) = ledger.try_borrow_mut() else {
            return Vec::new();
        };
        let snapshot = *ledger;
        *ledger = [Slot::EMPTY; Region::ALL.len()];
        Region::ALL
            .into_iter()
            .filter_map(|region| {
                let slot = snapshot[region.slot()];
                (slot.count > 0).then_some(Sample {
                    region,
                    count: slot.count,
                    total: slot.total,
                    self_total: slot.self_total,
                    max: slot.max,
                })
            })
            .collect()
    })
}

/// Clear without reading — use between a warm-up phase and a measured one.
pub fn reset() {
    let _ = drain();
}

// ─────────────────────────────────────────────────────────────────────────────
// Scope
// ─────────────────────────────────────────────────────────────────────────────

/// An open measurement region. Records into the ledger when dropped.
///
/// Held by `let _span = …` so it lives to the end of the enclosing block. Bound
/// to `_` (not `_span`) it would drop immediately and time nothing — which is
/// silent, so [`scope`]'s `#[must_use]` makes the compiler say it out loud.
pub struct Span {
    /// `None` when sampling was off at entry, which makes drop free.
    ///
    /// The third element is the enclosing span's child-time accumulator,
    /// parked here for the duration and restored on drop.
    started: Option<(Region, Instant, Duration)>,
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((region, started, parent_children)) = self.started else {
            return;
        };
        let elapsed = started.elapsed();
        let my_children = CHILD_TIME.with(|c| c.get());
        // Saturating, not wrapping: clock granularity can make a child's
        // measured elapsed marginally exceed its parent's, and a `Duration`
        // subtraction that underflows panics. A zero self-time is the honest
        // answer for a region whose children accounted for all of it.
        record(region, elapsed, elapsed.saturating_sub(my_children));
        // Hand our whole elapsed up: from the parent's point of view we are
        // one child, and our own children's time is already inside it.
        CHILD_TIME.with(|c| c.set(parent_children + elapsed));
    }
}

/// Open a measurement region.
#[must_use = "a Span that is dropped immediately measures nothing; bind it to a named local"]
#[inline]
pub fn scope(region: Region) -> Span {
    Span {
        started: enabled().then(|| {
            let parent_children = CHILD_TIME.with(|c| c.replace(Duration::ZERO));
            (region, Instant::now(), parent_children)
        }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reporting
// ─────────────────────────────────────────────────────────────────────────────

/// Format one drained ledger as a single line, for a harness to print.
///
/// Shape: `label=count/self_mean_ms/mean_ms/max_ms`, space-separated.
/// Deliberately one line and machine-greppable: doctrine §4 wants every
/// measured region visible in the report, and a multi-line block per scene
/// would drown it. Self-mean leads because it is the comparable figure (see
/// [`Sample`]).
pub fn format_line(samples: &[Sample]) -> String {
    let mut out = String::new();
    for sample in samples {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!(
            "{}={}/{:.3}/{:.3}/{:.3}",
            sample.region.label(),
            sample.count,
            sample.self_mean_ms(),
            sample.mean_ms(),
            sample.max_ms()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`ENABLED`] is process-wide, and libtest runs these on parallel
    /// threads, so without this a test that finishes and disables sampling
    /// silences one that is still running. The symptom is a bare "no rows
    /// recorded" failure that looks like a bug in the ledger and is not.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the serial lock and turn sampling on, ignoring poisoning: a
    /// previous test panicking is what we are being run to find out about, not
    /// a reason to fail every test after it with a different message.
    fn measuring() -> std::sync::MutexGuard<'static, ()> {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        reset();
        guard
    }

    /// `ALL` lists every variant exactly once, and every variant's `slot()` is
    /// a distinct index inside the ledger array.
    ///
    /// Both properties are load-bearing and neither is checked by the
    /// compiler: `LEDGER` is `[Slot; Region::ALL.len()]` and `record` indexes
    /// it with `region.slot()`, so a variant added without being added to
    /// `ALL` indexes out of bounds, and two variants that collided on a slot
    /// would sum into one row. This is the successor to a test that asserted
    /// the *addresses* of string consts were distinct — a property that was
    /// true, and useless, while the one it was standing in for was false.
    #[test]
    fn every_region_has_its_own_ledger_slot() {
        let mut seen = vec![false; Region::ALL.len()];
        for region in Region::ALL {
            let slot = region.slot();
            assert!(
                slot < Region::ALL.len(),
                "{region:?} indexes slot {slot}, outside a {}-slot ledger — it \
                 is almost certainly missing from Region::ALL",
                Region::ALL.len(),
            );
            assert!(
                !std::mem::replace(&mut seen[slot], true),
                "{region:?} shares ledger slot {slot} with another region; \
                 their samples would be summed into one row",
            );
        }
        assert!(
            seen.iter().all(|s| *s),
            "Region::ALL does not cover every ledger slot",
        );
    }

    /// No two regions report under the same name, or the report would show one
    /// row twice with different numbers and no way to tell which was which.
    #[test]
    fn every_region_reports_under_a_distinct_name() {
        let mut labels: Vec<&str> = Region::ALL.iter().map(|r| r.label()).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "two regions share a report label");
        assert!(labels.iter().all(|l| !l.is_empty()));
    }

    /// A disabled scope records nothing at all — the off path must not leave
    /// rows behind for a later drain to report as real work.
    #[test]
    fn disabled_scope_records_nothing() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        reset();
        {
            let _span = scope(Region::Shell);
        }
        assert!(drain().is_empty());
    }

    /// Repeated entries aggregate into one row with a real count, rather than
    /// appending a row each time — the property that makes `mean_ms` meaningful.
    #[test]
    fn repeated_entries_aggregate_into_one_row() {
        let _serial = measuring();
        for _ in 0..5 {
            let _span = scope(Region::Shell);
        }
        let samples = drain();
        set_enabled(false);

        assert_eq!(samples.len(), 1, "one label must produce one row");
        assert_eq!(samples[0].count, 5);
        assert!(
            samples[0].max <= samples[0].total,
            "max cannot exceed the total it is drawn from"
        );
    }

    /// A nested span's time is charged to the child's `self_total` and removed
    /// from the parent's, so moving work between the two cannot move the
    /// numbers. This is the property the whole before/after comparison rests
    /// on, and it was not true of the first version of this module.
    #[test]
    fn a_child_span_is_excluded_from_its_parents_self_time() {
        let _serial = measuring();
        {
            let _outer = scope(Region::Shell);
            {
                let _inner = scope(Region::DocsSection);
                std::thread::sleep(Duration::from_millis(8));
            }
        }
        let samples = drain();
        set_enabled(false);

        let outer = samples
            .iter()
            .find(|s| s.region == Region::Shell)
            .expect("parent row");
        let inner = samples
            .iter()
            .find(|s| s.region == Region::DocsSection)
            .expect("child row");

        assert!(
            inner.self_total >= Duration::from_millis(7),
            "the child must own the time it actually spent, got {:?}",
            inner.self_total
        );
        assert!(
            outer.total >= inner.total,
            "the parent's inclusive time must contain the child's"
        );
        assert!(
            outer.self_total < inner.self_total,
            "the parent did nothing but call the child, so its self time must \
             be the smaller of the two — got parent {:?}, child {:?}",
            outer.self_total,
            inner.self_total
        );
    }

    /// Two sibling spans at the same level do not steal each other's time —
    /// the accumulator has to be restored, not merely zeroed.
    #[test]
    fn sibling_spans_do_not_contaminate_each_other() {
        let _serial = measuring();
        {
            let _outer = scope(Region::Shell);
            {
                let _a = scope(Region::DocsSection);
                std::thread::sleep(Duration::from_millis(4));
            }
            {
                let _b = scope(Region::OmniSearchRow);
                std::thread::sleep(Duration::from_millis(4));
            }
        }
        let samples = drain();
        set_enabled(false);

        let outer = samples
            .iter()
            .find(|s| s.region == Region::Shell)
            .expect("parent row");
        // Both siblings must have been subtracted, so ~8 ms of the parent's
        // ~8 ms is accounted for by children and its self time is near zero.
        assert!(
            outer.self_total < Duration::from_millis(4),
            "only one sibling was subtracted from the parent; self time {:?}",
            outer.self_total
        );
    }

    /// Draining clears, so two consecutive measurement windows cannot
    /// contaminate each other — the before/after comparison depends on it.
    #[test]
    fn drain_clears_the_ledger() {
        let _serial = measuring();
        {
            let _span = scope(Region::DocsSection);
        }
        assert_eq!(drain().len(), 1);
        assert!(
            drain().is_empty(),
            "a second drain must see an empty ledger"
        );
        set_enabled(false);
    }

    /// `mean_ms` on an empty sample is zero rather than a NaN from `0/0` — a
    /// NaN would propagate into the report and read as a broken measurement.
    #[test]
    fn mean_of_zero_count_is_zero_not_nan() {
        let empty = Sample {
            region: Region::Shell,
            count: 0,
            total: Duration::ZERO,
            self_total: Duration::ZERO,
            max: Duration::ZERO,
        };
        assert_eq!(empty.mean_ms(), 0.0);
        assert_eq!(empty.self_mean_ms(), 0.0);
    }
}
