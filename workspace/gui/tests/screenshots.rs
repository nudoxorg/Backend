//! The screenshot plane: drive the real shell, rasterise real frames, prove it.
//!
//! # What this is
//!
//! Every image under `.shots/` is produced by this file, by booting the *same*
//! stack `main.rs` boots — `gpui_component::init` → `NudoxThemeExt::init` →
//! `MotionTokens` → keymap → stores → window — dispatching the *same* actions
//! the keybindings dispatch, and rasterising the *same* scene the GPU would
//! present. There is no mock renderer, no stub text system, and no hand-written
//! HTML mock-up anywhere in the chain.
//!
//! # Why `harness = false`
//!
//! libtest runs `#[test]` bodies on spawned worker threads. Constructing the
//! macOS platform touches AppKit, which aborts off the main thread. With no
//! harness this file *is* `fn main`, so it runs where AppKit is legal.
//!
//! # Why a screenshot that merely "saved successfully" is not proof
//!
//! A blank window saves just as cleanly as a correct one. So [`Stage::shoot`]
//! refuses to record a frame unless it survives [`FrameCheck`]: the frame must
//! be opaque, it must contain more than a handful of distinct colours (a flat
//! fill means the scene did not paint), and — per the caller's [`Change`] claim
//! — it must differ from the frame before it by *enough*, not merely by more
//! than zero. A suite that cannot fail is not evidence, and the most likely way
//! for this one to silently stop being evidence is for every shot to quietly
//! become the same picture of an empty shell (or, just as bad, a picture that
//! technically differs by one blinking caret while claiming to prove a tab
//! switched). See [`Change`] for why a bare `bool` used to let that happen.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyWindowHandle, AppContext as _, ClipboardItem, Focusable as _, HeadlessAppContext,
    px, size,
};
use image::RgbaImage;

use lindsey::app::actions::{
    ActivateTab2, ConfirmOverlay, CopySymbolUri, DismissOverlay, FilterAuto, FilterName,
    FilterSemantic, GoToDocsTab,
    GoToRefsTab, GoToSourceTab, GoToTimelineTab, MoveSelectionDown, MoveSelectionUp,
    OpenCommandPalette, OpenInBackgroundTab, OpenOmniSearch, OpenVersionPicker, ToggleBottomDock,
    ToggleLeftDock, ToggleShortcutsOverlay,
};
use lindsey::app::keymaps;
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::search_model::SearchAccess as _;
use lindsey::stores::{PackageStore, SearchStore, SymbolStore};
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::views::symbol_page::SymbolPage;
use lindsey::workspace::shell::Shell;
/// The concrete engine every store in this binary is parameterised by — the
/// type argument `SymbolPage` needs to be named for a downcast.
use nudox_engine::EngineHandle as PageEngine;
use nudox_engine::runtime::{Engine, EngineConfig};
use nudox_engine::{PackageHistorySpec, PackageVersionSpec, ProducerLanguage};

// ---------------------------------------------------------------------------
// Frame validation
// ---------------------------------------------------------------------------

/// What a captured frame must satisfy before it is allowed to become evidence.
///
/// These are deliberately weak *individually* — none of them can tell a
/// beautiful frame from an ugly one. Together they close the failure mode that
/// actually threatens this suite: shipping a directory of identical blank
/// rectangles while every assertion still passes.
#[derive(Debug, Clone, Copy)]
struct FrameCheck {
    /// Fraction of pixels that must be fully opaque. The shell paints a solid
    /// background edge to edge, so anything less means we rasterised a partial
    /// or empty scene.
    min_opaque_fraction: f64,
    /// Distinct RGBA values the frame must contain. A flat fill — the signature
    /// of "the view rendered nothing on top of the background" — has one.
    min_distinct_colours: usize,
}

impl Default for FrameCheck {
    fn default() -> Self {
        Self {
            min_opaque_fraction: 0.95,
            min_distinct_colours: 24,
        }
    }
}

/// Measured properties of one frame, kept so failures can say what they saw.
#[derive(Debug, Clone, Copy)]
struct FrameStats {
    width: u32,
    height: u32,
    opaque_fraction: f64,
    distinct_colours: usize,
}

impl FrameStats {
    fn measure(image: &RgbaImage) -> Self {
        let total = (image.width() as u64 * image.height() as u64).max(1);
        let mut opaque = 0u64;
        // A full distinct-colour set over a 2880x1800 frame is millions of
        // entries; we only ever compare against a small floor, so stop counting
        // once the floor is comfortably cleared.
        let mut palette = std::collections::HashSet::with_capacity(1024);
        for pixel in image.pixels() {
            if pixel.0[3] == u8::MAX {
                opaque += 1;
            }
            if palette.len() < 4096 {
                palette.insert(pixel.0);
            }
        }
        Self {
            width: image.width(),
            height: image.height(),
            opaque_fraction: opaque as f64 / total as f64,
            distinct_colours: palette.len(),
        }
    }
}

/// Count pixels that differ between two frames of identical dimensions.
///
/// Returns `None` when the dimensions differ, which is itself a change.
fn differing_pixels(a: &RgbaImage, b: &RgbaImage) -> Option<u64> {
    if a.dimensions() != b.dimensions() {
        return None;
    }
    Some(
        a.pixels()
            .zip(b.pixels())
            .filter(|(x, y)| x.0 != y.0)
            .count() as u64,
    )
}

// ---------------------------------------------------------------------------
// Change — a typed claim about how much a frame should differ (SHOT-REVIEW R1)
// ---------------------------------------------------------------------------

/// Minimum fraction of the frame [`Change::Major`] demands as different from
/// the previous one.
///
/// Set above what a blinking text-input caret can produce — roughly 12 000 of
/// 5 184 000 pixels on a 2 880×1 800 frame, ~0.24% (see AGENTS-DOCTRINE.md's
/// GPUI testing notes) — so an idle animation can never be mistaken for the
/// state change the claim is actually about.
const MAJOR_MIN_FRACTION: f64 = 0.01;

/// A call site's claim about how a captured frame should compare to the one
/// before it.
///
/// This replaced a bare `bool` (`expect_change`) that asserted only
/// `changed > 0`. That threshold is so low a blinking caret clears it, which
/// means the guard only ever fired on a frame that was *pixel-perfect*
/// identical — almost never true once anything animates. Seven scenes in this
/// file's history exploited the other side of the same weakness: they used
/// `expect_change: false`, which made no claim at all, so an action that
/// silently failed to reach its handler produced a frame indistinguishable
/// from one where nothing was ever supposed to happen. See SHOT-REVIEW.md R1
/// and LIMITATIONS.md L16.
///
/// An enum instead of a raw fraction at each call site, because a bare `f64`
/// threshold is exactly the kind of "magic number" that erodes silently over
/// time — every variant that permits anything less than a full, whole-scene
/// repaint carries a `reason` that has to be written down and that shows up in
/// the failure message. AGENTS-DOCTRINE §3: make the weak claim hard to write
/// by accident.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// The very first frame captured: there is nothing to compare against.
    First,
    /// A material, whole-scene state change — an overlay opening or closing, a
    /// tab switching, a document being replaced. Demands at least
    /// [`MAJOR_MIN_FRACTION`] of the frame differ.
    Major,
    /// A real but spatially small repaint: a status label ticking, a query
    /// clearing, a highlight moving. `min_fraction` is the floor and `reason`
    /// names the region expected to move — this is the record of *why* the
    /// bar is lower than [`Change::Major`], not a bare tolerance nobody can
    /// audit later.
    Minor { min_fraction: f64, reason: &'static str },
    /// The action was dispatched but is known, for the stated reason, not to
    /// visibly change this screen right now. `max_fraction` bounds how much
    /// incidental noise (antialiasing, an unrelated counter ticking) is
    /// tolerated — if a "known no-op" ever clears it, the reason has gone
    /// stale and the call site needs to be re-examined, not silenced further.
    KnownNoOp {
        max_fraction: f64,
        reason: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Corpus selection
// ---------------------------------------------------------------------------

/// Resolve a possibly-relative fixture path against the repository root.
///
/// # Why this is not just `PathBuf::from`
///
/// `workspace/gui` is a standalone package with its own lockfile (doctrine
/// §1), so cargo runs this binary with its cwd set to `workspace/gui` — not to
/// the repository root the documented command is written from. A bare
/// `.real-crates/memchr-2.8.3` therefore resolved to
/// `workspace/gui/.real-crates/memchr-2.8.3`, which does not exist, and the
/// suite aborted before booting with `has no Cargo.toml` — a message that
/// reads like a *destroyed fixture* rather than a path that was never going to
/// resolve. `.real-crates/` is gitignored and irrecoverable (doctrine §8), so
/// "your fixture is gone" is precisely the wrong place to send the next
/// reader first.
///
/// The output path in `main` already did this correctly, anchoring on
/// `CARGO_MANIFEST_DIR`. Inputs now use the same anchor, so a path means the
/// same thing on both sides of the harness.
fn repo_path(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// Which corpus the shots are taken against.
///
/// The default is the built-in fixture corpus because it is instant and
/// deterministic, which is what makes this suite runnable in a loop while the
/// scenes are being written. The real-package mode is what produces the
/// evidence: `NUDOX_SHOT_PKG_ROOT` + `NUDOX_SHOT_PKG_NAME` point it at a real
/// crate checkout and every frame then contains real lowered API.
enum Corpus {
    Fixtures,
    Package {
        name: String,
        /// Every generation to load, newest last. The **first** entry is the
        /// one the frames are labelled with; the engine decides which is
        /// *current* by version order, not by this order.
        ///
        /// More than one is what makes lineage photographable at all: a symbol
        /// timeline and a version dropdown both require several generations of
        /// a package resident at once, and with a single root the version strip
        /// has one row and the picker has nothing to pick between. Every frame
        /// this suite has ever produced was taken against a one-version corpus,
        /// which is why `13-version-picker` was byte-identical to the frame
        /// before it in every run.
        versions: Vec<(PathBuf, String)>,
    },
}

impl Corpus {
    /// `NUDOX_SHOT_PKG_ROOT` names one generation; `NUDOX_SHOT_PKG_ROOTS` names
    /// several as `path=version` pairs separated by `,`.
    ///
    /// Relative fixture paths are resolved against the **repository root** —
    /// see [`repo_path`] for why they cannot be left to the process cwd.
    fn from_env() -> Self {
        let roots = std::env::var("NUDOX_SHOT_PKG_ROOTS")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let single = std::env::var("NUDOX_SHOT_PKG_ROOT")
            .ok()
            .filter(|v| !v.trim().is_empty());

        if roots.is_none() && single.is_none() {
            return Corpus::Fixtures;
        }

        let name = std::env::var("NUDOX_SHOT_PKG_NAME").expect(
            "a package corpus requires NUDOX_SHOT_PKG_NAME: the name is \
             matched against cargo metadata to pick the documented package, \
             and guessing it from the directory is how you get a silent \
             zero-symbol corpus",
        );

        let versions: Vec<(PathBuf, String)> = match roots {
            Some(list) => list
                .split(',')
                .map(|entry| {
                    let (path, version) = entry.trim().split_once('=').unwrap_or_else(|| {
                        panic!(
                            "NUDOX_SHOT_PKG_ROOTS entries are `path=version`; got {entry:?}"
                        )
                    });
                    (repo_path(path), version.to_owned())
                })
                .collect(),
            None => {
                let root = repo_path(&single.expect("checked above"));
                let version =
                    std::env::var("NUDOX_SHOT_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
                vec![(root, version)]
            }
        };

        for (root, _) in &versions {
            assert!(
                root.join("Cargo.toml").is_file(),
                "{} has no Cargo.toml — fixture roots are resolved against the \
                 repository root, not the current directory",
                root.display(),
            );
        }

        Corpus::Package { name, versions }
    }

    fn label(&self) -> String {
        match self {
            Corpus::Fixtures => "fixtures".to_owned(),
            Corpus::Package { name, versions } => {
                let newest = versions
                    .iter()
                    .map(|(_, v)| v.as_str())
                    .max()
                    .unwrap_or("0.0.0");
                format!("{name}-{newest}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage
// ---------------------------------------------------------------------------

/// The booted app, its window, and the shot ledger.
///
/// # Teardown order (read before touching field order)
///
/// `HeadlessAppContext::drop` runs `App::shutdown()`, which closes the window
/// and — with it — every `Entity` the *window's own tree* holds (`Shell`'s
/// clones of `search`/`symbols`/`packages` among them). That is fine: it
/// happens while the entity map is still alive.
///
/// But `Stage` also holds its *own* top-level clones of `search`, `symbols`
/// and `packages`, for driving the scenario and polling store state, and
/// those are not reachable from the window tree at all. If `cx` were dropped
/// while those clones are still alive, `HeadlessAppContext::drop` tears down
/// the entity map's `LeakDetector` before Rust gets around to running the
/// `Drop` for `Stage`'s own fields — so the detector sees three outstanding
/// handles it has no way to know are about to disappear, and panics
/// `"Exited with leaked handles"` on a perfectly clean run.
///
/// [`Stage::finish`] is the fix: it consumes `self` and drops the entity
/// handles *before* `cx`, by construction rather than by hoping nobody
/// reorders the struct's fields. Field declaration order does also happen to
/// achieve this (Rust drops struct fields top-to-bottom, and `cx` is
/// declared first), but relying on that silently is exactly the kind of
/// thing a future field addition breaks without anyone noticing — hence the
/// explicit method.
struct Stage {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    search: gpui::Entity<SearchStore>,
    symbols: gpui::Entity<SymbolStore>,
    packages: gpui::Entity<PackageStore>,
    out_dir: PathBuf,
    corpus: String,
    previous: Option<RgbaImage>,
    shots: Vec<ShotRecord>,
    perf_totals: Vec<lindsey::perf::Sample>,
}

/// One recorded frame, for the manifest the report is built from.
struct ShotRecord {
    slug: String,
    caption: String,
    stats: FrameStats,
    changed_pixels: Option<u64>,
    elapsed: Duration,
}

impl Stage {
    /// Boot the full shell in exactly the order `main.rs` does.
    fn boot(corpus: &Corpus, out_dir: PathBuf) -> Self {
        let platform = gpui_platform::current_platform(true);
        let text_system = platform.text_system();

        let mut cx = HeadlessAppContext::with_platform(
            text_system,
            Arc::new(gpui_component_assets::Assets),
            || gpui_platform::current_headless_renderer(),
        );

        // The engine reaches real Tokio threads (and, in package mode, an
        // in-process rust-analyzer); the deterministic test dispatcher must be
        // allowed to park while they work.
        cx.allow_parking();

        let config = EngineConfig {
            highlighter: Some(Arc::new(lindsey::highlight::TreeSitterHighlighter)),
            ..EngineConfig::default()
        };

        let (engine, requested) = match corpus {
            Corpus::Fixtures => (Engine::start_with_fixtures(config), Vec::new()),
            Corpus::Package { name, versions } => {
                // `start_with_versions`, not `start_with_producer`: the second
                // is the one-generation case of the first, and this suite's
                // whole point in package mode is that the corpus can hold more
                // than one.
                let spec = PackageHistorySpec {
                    name: name.clone(),
                    language: ProducerLanguage::Rust,
                    versions: versions
                        .iter()
                        .map(|(root, version)| PackageVersionSpec {
                            root: root.clone(),
                            version: version.clone(),
                        })
                        .collect(),
                };
                (
                    Engine::start_with_versions(config, vec![spec]),
                    vec![name.clone()],
                )
            }
        };

        cx.update(|cx| {
            gpui_component::init(cx);
            // `ThemeMode::default()` is Light, and `main.rs` never chooses. That
            // makes lindsey's appearance a property of the host rather than a
            // design decision, and headless — where there is no system
            // appearance to read — always lands on light. The design target is
            // the dark, high-contrast `frame` aesthetic, so the shots state it
            // explicitly instead of inheriting whatever the machine felt like.
            gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
            // Must follow the mode change: `NudoxThemeExt::init` picks its
            // palette by asking `cx.theme().is_dark()`, so initialising it first
            // would install the light extension over a dark base.
            NudoxThemeExt::init(cx);
            cx.set_global(MotionTokens::new(1.0));
            cx.bind_keys(keymaps::all_bindings());
        });

        let search = cx.new(|_| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_| SymbolStore::new(engine.clone()));
        let packages = cx.new(|cx| PackageStore::new(engine.clone(), &requested, cx));

        let window = {
            let search = search.clone();
            let symbols = symbols.clone();
            let packages = packages.clone();
            cx.open_window(size(px(1440.), px(900.)), move |window, cx| {
                cx.new(|cx| Shell::new(search, symbols, packages, window, cx))
            })
            .expect("open headless window")
        };

        Self {
            cx,
            window: window.into(),
            search,
            symbols,
            packages,
            out_dir,
            corpus: corpus.label(),
            previous: None,
            shots: Vec::new(),
            perf_totals: Vec::new(),
        }
    }

    fn accumulate_perf(&mut self, samples: &[lindsey::perf::Sample]) {
        for sample in samples {
            match self
                .perf_totals
                .iter_mut()
                .find(|s| s.region == sample.region)
            {
                Some(slot) => {
                    slot.count += sample.count;
                    slot.total += sample.total;
                    slot.self_total += sample.self_total;
                    if sample.max > slot.max {
                        slot.max = sample.max;
                    }
                }
                None => self.perf_totals.push(*sample),
            }
        }
    }

    fn report_perf(&self) {
        println!("\u{2500}\u{2500} render cost, whole run \u{2500}\u{2500}");
        for sample in &self.perf_totals {
            println!(
                "cost case=render/{}/{} frames={} self_mean_ms={:.4} mean_ms={:.4} \
                 max_ms={:.4} self_total_ms={:.1} total_ms={:.1}",
                self.corpus,
                sample.region.label(),
                sample.count,
                sample.self_mean_ms(),
                sample.mean_ms(),
                sample.max_ms(),
                sample.self_total.as_secs_f64() * 1000.0,
                sample.total.as_secs_f64() * 1000.0,
            );
        }
    }

    /// Release every entity handle `Stage` owns, in dependency order, then
    /// drop `cx` last.
    ///
    /// Call this in place of letting `stage` fall out of scope at the end of
    /// `main`. See the [`Stage`] doc comment for why the order matters;
    /// `window` needs no entry here because `AnyWindowHandle` is a bare
    /// `(WindowId, TypeId)` pair, not an owning reference — closing the
    /// window itself is `cx`'s job, done inside `shutdown()`.
    fn finish(self) {
        let Stage {
            cx,
            window: _window,
            search,
            symbols,
            packages,
            out_dir: _out_dir,
            corpus: _corpus,
            previous: _previous,
            shots: _shots,
            perf_totals: _perf_totals,
        } = self;
        drop(search);
        drop(symbols);
        drop(packages);
        drop(cx);
    }

    /// Drain GPUI work, force redraws, and let entrance animations finish.
    ///
    /// `advance_clock` alone is not enough and using it here was actively
    /// misleading: it moves the `TestDispatcher`'s timer wheel, but GPUI's
    /// declarative `Animation` computes its phase from wall-clock elapsed time
    /// off the frame timestamp. Advancing the simulated clock therefore fires
    /// pending *timers* while leaving every fade at whatever opacity it happened
    /// to reach, which is how the first run of this suite produced a page of
    /// half-transparent search results and made a working UI look broken.
    ///
    /// So the settle loop sleeps for real, and re-draws between sleeps —
    /// `update_window` renders a frame, and `capture_screenshot` reads back the
    /// *last rendered* frame, so a capture without a preceding draw would
    /// photograph stale pixels.
    /// Two clocks have to move here, and forgetting either one produces a
    /// different, confusing failure:
    ///
    /// * `advance_clock` drives the `TestDispatcher`'s timer wheel. The search
    ///   store's 24 ms input debounce is such a timer, so without this the
    ///   corpus never gets queried and the suite times out waiting for hits
    ///   that were never requested.
    /// * a real `sleep` plus a re-draw drives GPUI's declarative `Animation`,
    ///   which takes its phase from wall-clock elapsed time off the frame
    ///   timestamp. Without this, entrance fades are photographed mid-flight
    ///   and a perfectly good UI renders as ghostly half-transparent text.
    ///
    /// `capture_screenshot` reads back the *last rendered* frame, so the final
    /// draw is what the shot actually sees.
    fn settle(&mut self) {
        const ROUNDS: u32 = 10;
        const STEP: Duration = Duration::from_millis(40);
        for _ in 0..ROUNDS {
            self.cx.run_until_parked();
            self.cx.advance_clock(STEP);
            self.cx.run_until_parked();
            self.draw();
            std::thread::sleep(STEP);
        }
        self.cx.run_until_parked();
        self.draw();
        self.cx.run_until_parked();
        self.draw();
    }

    /// Render one frame. `capture_screenshot` reads whatever this last produced.
    fn draw(&mut self) {
        let _ = self.cx.update_window(self.window, |_, window, _| {
            window.refresh();
        });
    }

    /// Poll until `ready` holds, giving real background threads real time.
    ///
    /// `run_until_parked` drains GPUI's own queues but knows nothing about the
    /// Tokio threads the engine runs on, so a real sleep between polls is the
    /// only thing that lets corpus loading actually progress.
    fn wait_until(&mut self, what: &str, budget: Duration, mut ready: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + budget;
        loop {
            self.settle();
            if ready(self) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {:?} waiting for: {what}",
                    budget
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Assert that no two rows the results list can draw are textually
    /// identical, and print what they are.
    ///
    /// # The finding this exists for (GUI-WORKORDER-2 F1 / LIMITATIONS.md L20)
    ///
    /// `.shots/memchr/04-search-hits.png` showed seven consecutive rows reading
    /// exactly `memchr` over `mod memchr`, with the same kind chip and the same
    /// trust chip, and every assertion in this file passed. The suite checked
    /// that hits *arrived* and that the frame *changed*; nothing checked that a
    /// human could tell one result from another, which is the only thing a
    /// results list is for.
    ///
    /// Deliberately not parameterised by an expected count. Those frames were
    /// captured while `cargo metadata` was failing and every Cargo feature
    /// evaluated false, so more arch-gated modules were live than should have
    /// been; a producer fix now in flight removes ~10 000 phantom `core`
    /// intrinsics from the same corpus. The number of duplicates has moved
    /// twice already and will move again. The *defect* — rows a user cannot
    /// choose between — is the same at any count, so that is what is asserted.
    fn assert_search_rows_are_distinct(&mut self, what: &str) {
        let search = self.search.clone();
        let rows: Vec<(usize, usize, String)> = self.cx.update(|cx| {
            let snapshot = search.read(cx).snapshot();
            snapshot
                .sections
                .iter()
                .enumerate()
                .flat_map(|(section, data)| {
                    data.rows
                        .iter()
                        .enumerate()
                        .map(move |(row, prepared)| {
                            (section, row, prepared.render_identity())
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        });

        assert!(
            !rows.is_empty(),
            "{what}: no rows to check — the distinctness guard would pass \
             vacuously, which is the one way it stops being a guard",
        );

        let mut seen: std::collections::HashMap<&str, (usize, usize)> =
            std::collections::HashMap::new();
        for (section, row, identity) in &rows {
            if let Some(first) = seen.insert(identity.as_str(), (*section, *row)) {
                let readable = identity.replace('\u{1}', " | ");
                panic!(
                    "{what}: rows {first:?} and ({section}, {row}) render the \
                     same text — {readable:?}. A user has no way to choose \
                     between them (F1). Every row's full text:\n{}",
                    rows.iter()
                        .map(|(s, r, id)| format!(
                            "  ({s},{r}) {}",
                            id.replace('\u{1}', " | ")
                        ))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }

        println!("rows-distinct {what}: {} row(s), all distinct", rows.len());
        for (s, r, id) in &rows {
            println!("  row ({s},{r}) {}", id.replace('\u{1}', " | "));
        }
    }

    /// Wait until the search store reports at least `min` hits, summed
    /// across all three sections.
    ///
    /// `MoveSelectionDown` needs a *second* row to land on. Without this
    /// guard, a corpus that only ever matches once would turn "moved to the
    /// second hit" into "moved nowhere" — and the resulting failure would
    /// surface as a confusing `shoot()` assertion several steps later, not
    /// here where the actual cause is visible.
    fn wait_for_hit_count(&mut self, min: usize, budget: Duration) {
        let what = format!("at least {min} search hits");
        self.wait_until(&what, budget, |s| {
            let search = s.search.clone();
            s.cx.update(|cx| {
                let snapshot = search.read(cx).snapshot();
                snapshot
                    .sections
                    .iter()
                    .map(|section| section.rows.len())
                    .sum::<usize>()
                    >= min
            })
        });
    }

    /// Wait until every requested generation of every package is resident.
    ///
    /// # Why waiting for "a query answers" is not enough
    ///
    /// The readiness probe above stops as soon as *some* package can answer,
    /// which for a multi-generation corpus is as soon as the *first* one
    /// finishes. Each generation is a separate in-process rust-analyzer load
    /// taking ~10 s, so the scenario then raced ahead and opened a document
    /// while the other two were still lowering — `DocEvent::Timeline` carried a
    /// single row, the version picker had one option, and the frames claiming
    /// to show lineage showed a one-version corpus. That is exactly the shape
    /// of failure the suite exists to make impossible, so it is a wait and not
    /// a hope.
    fn wait_for_all_generations(&mut self, expected: usize, budget: Duration) {
        if expected <= 1 {
            return;
        }
        let what = format!("all {expected} generations resident");
        self.wait_until(&what, budget, |s| {
            let packages = s.packages.clone();
            s.cx.update(|cx| {
                let store = packages.read(cx);
                let rows = store.rows();
                !rows.is_empty()
                    && rows.iter().all(|row| {
                        row.lineage
                            .as_ref()
                            .is_some_and(|l| store.versions(l).len() >= expected)
                    })
            })
        });
    }

    /// Wait until every currently open symbol tab has received its `Head`
    /// event — i.e. there is a real document on screen, not a skeleton.
    ///
    /// Used both right after the first symbol opens and again after a second
    /// tab is opened behind it, so a shot of the *second* tab never captures
    /// it mid-stream.
    fn wait_for_all_doc_heads(&mut self, budget: Duration) {
        self.wait_until("every open symbol tab's head to arrive", budget, |s| {
            let symbols = s.symbols.clone();
            s.cx.update(|cx| {
                let docs = &symbols.read(cx).docs;
                !docs.is_empty() && docs.values().all(|doc| doc.head.is_some())
            })
        });
    }

    /// Dispatch a real action — the same one the keybinding dispatches.
    fn act(&mut self, action: &dyn Action) {
        let boxed = action.boxed_clone();
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.dispatch_action(boxed, cx);
            })
            .expect("dispatch action");
        self.settle();
    }

    /// Type into the search store, exactly as the overlay does per keystroke.
    fn type_query(&mut self, query: &str) {
        let search = self.search.clone();
        self.cx.update(|cx| {
            search.update(cx, |store, cx| store.set_input(query.into(), cx));
        });
        self.settle();
    }

    /// True if the pane's own root — not anything inside its active tab's
    /// content — currently holds keyboard focus.
    ///
    /// `Window::dispatch_action` looks up whatever `window.focused(cx)`
    /// currently names and walks *its* ancestor chain (see gpui's
    /// `Window::dispatch_action` / `focus_node_id_in_rendered_frame` in
    /// `crates/gpui/src/window.rs`), so an action can only reach a handler
    /// registered at or above that element.
    ///
    /// This used to be `true` for the whole of scenes 09-14 and was the causal
    /// evidence behind their `KnownNoOp` claims: `SymbolPage` held no
    /// `FocusHandle` at all, so its own `.on_action` handlers were structurally
    /// unreachable (LIMITATIONS.md L16). Both halves of that are now fixed —
    /// `SymbolPage` implements `Focusable` and calls `.track_focus` on the one
    /// root `page_root` builds, and `Pane::activate_ix` focuses the active
    /// item's handle on every activation — so this now reads `false` while a
    /// document tab is open, and `false` is the *healthy* answer.
    ///
    /// Kept, inverted, for that reason: it is still the cheapest runtime probe
    /// for "did focus end up where the pane put it?", and a regression to
    /// `true` here is exactly what L16 looked like.
    fn pane_is_focused(&mut self) -> bool {
        self.cx
            .update_window(self.window, |root_view, window, cx| {
                // `update_window` hands us the root view directly (as an
                // erased `AnyView`) precisely because the window is checked
                // out for the duration of this closure — looking it up again
                // via `AnyWindowHandle::downcast(..).root(cx)` here fails with
                // "window not found", so this downcasts the given root
                // instead of re-fetching it.
                let shell = root_view
                    .downcast::<Shell>()
                    .expect("screenshot window's root view is always Shell");
                let pane_focus = shell.read(cx).pane().read(cx).focus_handle(cx);
                pane_focus.is_focused(window)
            })
            .expect("read focus state")
    }

    /// Read something off the active tab's `SymbolPage`.
    ///
    /// The pane stores its items as erased `AnyView`s, so this downcasts back
    /// to the one concrete page type this binary ever puts in a tab. Panics
    /// rather than returning `None` when there is no document: every caller is
    /// asserting about a page it has just opened, and "there was no page" is a
    /// failure of that scene, not a state to tolerate.
    fn with_symbol_page<R>(&mut self, what: &str, f: impl FnOnce(&SymbolPage<PageEngine>) -> R) -> R {
        self.cx
            .update_window(self.window, |root_view, _window, cx| {
                let shell = root_view
                    .downcast::<Shell>()
                    .expect("screenshot window's root view is always Shell");
                let view = shell
                    .read(cx)
                    .pane()
                    .read(cx)
                    .active_item()
                    .unwrap_or_else(|| panic!("{what}: no active pane tab"));
                let page = view
                    .downcast::<SymbolPage<PageEngine>>()
                    .unwrap_or_else(|_| panic!("{what}: active tab is not a SymbolPage"));
                f(page.read(cx))
            })
            .expect("read the active symbol page")
    }

    /// How many rows the page's table of contents currently has (F3).
    fn outline_len(&mut self) -> usize {
        self.with_symbol_page("outline_len", |page| page.outline_len())
    }

    /// How many generations the header's version picker offers (real lineage).
    fn version_option_count(&mut self) -> usize {
        self.with_symbol_page("version_option_count", |page| page.version_count())
    }

    /// Capture, validate, and record one frame.
    ///
    /// `change` is the caller's typed claim about how this frame should
    /// compare to the one before it — see [`Change`] for why this is not a
    /// `bool`. When the claim is not met, the frame is rejected: either the
    /// action did less than it claimed to, or the harness is lying about what
    /// the action does.
    fn shoot(&mut self, slug: &str, caption: &str, change: Change) {
        let render_cost = lindsey::perf::drain();
        self.accumulate_perf(&render_cost);
        let started = Instant::now();
        let image = self
            .cx
            .capture_screenshot(self.window)
            .unwrap_or_else(|err| panic!("capture {slug}: {err}"));

        let stats = FrameStats::measure(&image);
        let check = FrameCheck::default();

        assert!(
            stats.opaque_fraction >= check.min_opaque_fraction,
            "{slug}: only {:.1}% of pixels are opaque (need {:.0}%) — the scene \
             did not fully paint",
            stats.opaque_fraction * 100.0,
            check.min_opaque_fraction * 100.0,
        );
        assert!(
            stats.distinct_colours >= check.min_distinct_colours,
            "{slug}: only {} distinct colours (need {}) — this is a flat fill, \
             not a rendered view",
            stats.distinct_colours,
            check.min_distinct_colours,
        );

        let changed = self
            .previous
            .as_ref()
            .and_then(|prev| differing_pixels(prev, &image));

        // `Change::First` and "there is a previous frame" must agree, or the
        // claim itself is meaningless — catch a mismatched claim here rather
        // than silently comparing against nothing (or silently having nothing
        // to compare a real claim against).
        match change {
            Change::First => assert!(
                self.previous.is_none(),
                "{slug}: claimed Change::First but a previous frame exists — \
                 this is not the first shot, so the claim must be Major, \
                 Minor, or KnownNoOp instead",
            ),
            Change::Major | Change::Minor { .. } | Change::KnownNoOp { .. } => assert!(
                self.previous.is_some(),
                "{slug}: claimed a non-First Change on the very first frame, \
                 which has nothing to compare against — use Change::First",
            ),
        }

        let total_pixels = (stats.width as u64 * stats.height as u64).max(1);
        if let Some(changed_px) = changed {
            let pct = changed_px as f64 / total_pixels as f64 * 100.0;
            match change {
                Change::First => {
                    // Unreachable in practice: asserted above that `previous`
                    // is `None` whenever this arm is taken, so `changed` (which
                    // is derived from `previous`) would also be `None`.
                }
                Change::Major => {
                    let need = (total_pixels as f64 * MAJOR_MIN_FRACTION).ceil() as u64;
                    assert!(
                        changed_px >= need,
                        "{slug}: only {changed_px} of {total_pixels} pixels changed \
                         ({pct:.3}%); Major demands >= {:.1}% for a real state \
                         change (an overlay opening or closing, a tab switching, a \
                         document being replaced) — this looks like the action did \
                         nothing, not like a real repaint",
                        MAJOR_MIN_FRACTION * 100.0,
                    );
                }
                Change::Minor { min_fraction, reason } => {
                    let need = (total_pixels as f64 * min_fraction).ceil() as u64;
                    assert!(
                        changed_px >= need,
                        "{slug}: only {changed_px} of {total_pixels} pixels changed \
                         ({pct:.3}%), need >= {:.3}% for the claimed localised \
                         repaint ({reason})",
                        min_fraction * 100.0,
                    );
                }
                Change::KnownNoOp { max_fraction, reason } => {
                    let ceiling = (total_pixels as f64 * max_fraction).ceil() as u64;
                    assert!(
                        changed_px <= ceiling,
                        "{slug}: {changed_px} of {total_pixels} pixels changed \
                         ({pct:.3}%) — more than the {:.3}% tolerated for a \
                         documented no-op ({reason}). Either the gap this cites has \
                         been fixed (update this call site and LIMITATIONS.md) or an \
                         unrelated regression appeared.",
                        max_fraction * 100.0,
                    );
                }
            }
        }

        let path = self.out_dir.join(format!("{slug}.png"));
        image.save(&path).unwrap_or_else(|e| panic!("save {slug}: {e}"));

        let elapsed = started.elapsed();
        println!(
            "cost case=shot/{}/{} wall_ms={:.1} px={}x{} colours={} changed={} -> {}",
            self.corpus,
            slug,
            elapsed.as_secs_f64() * 1000.0,
            stats.width,
            stats.height,
            stats.distinct_colours,
            changed.map_or_else(|| "n/a".into(), |c| c.to_string()),
            path.display(),
        );

        self.shots.push(ShotRecord {
            slug: slug.to_owned(),
            caption: caption.to_owned(),
            stats,
            changed_pixels: changed,
            elapsed,
        });
        self.previous = Some(image);
    }

    /// Write the manifest the report generator reads.
    fn write_manifest(&self) {
        let mut out = String::from("# Screenshot manifest\n\n");
        out.push_str(&format!("corpus: {}\n\n", self.corpus));
        out.push_str("| # | shot | caption | size | colours | changed px | ms |\n");
        out.push_str("|---|------|---------|------|---------|-----------|----|\n");
        for (i, s) in self.shots.iter().enumerate() {
            out.push_str(&format!(
                "| {} | `{}.png` | {} | {}×{} | {} | {} | {:.0} |\n",
                i + 1,
                s.slug,
                s.caption,
                s.stats.width,
                s.stats.height,
                s.stats.distinct_colours,
                s.changed_pixels
                    .map_or_else(|| "—".to_owned(), |c| c.to_string()),
                s.elapsed.as_secs_f64() * 1000.0,
            ));
        }
        let path = self.out_dir.join("MANIFEST.md");
        std::fs::write(&path, out).expect("write manifest");
        println!("manifest -> {}", path.display());
    }
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

fn main() {
    let corpus = Corpus::from_env();
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.shots")
        .join(match &corpus {
            Corpus::Fixtures => "fixtures".to_owned(),
            Corpus::Package { name, .. } => name.clone(),
        });
    std::fs::create_dir_all(&out_dir).expect("create shot dir");

    println!("corpus: {}", corpus.label());
    println!("out:    {}", out_dir.display());

    let query = std::env::var("NUDOX_SHOT_QUERY").unwrap_or_else(|_| {
        match &corpus {
            Corpus::Fixtures => "Point".to_owned(),
            // Every crate has *something*; the caller overrides this for a
            // symbol worth photographing.
            Corpus::Package { .. } => "find".to_owned(),
        }
    });

    lindsey::perf::set_enabled(true);

    let mut stage = Stage::boot(&corpus, out_dir);

    // ── 01 — the shell, corpus still arriving ────────────────────────────────
    stage.settle();
    stage.shoot("01-shell-boot", "The shell at boot, before the corpus lands", Change::First);

    // ── 02 — corpus loaded ───────────────────────────────────────────────────
    // Package mode drives in-process rust-analyzer, which needs tens of seconds.
    let budget = match &corpus {
        Corpus::Fixtures => Duration::from_secs(20),
        // One in-process rust-analyzer load per generation, serialised.
        Corpus::Package { versions, .. } => {
            Duration::from_secs(240) * versions.len().max(1) as u32
        }
    };
    {
        let probe = query.clone();
        stage.wait_until("the corpus to answer a query", budget, move |s| {
            let search = s.search.clone();
            let probe = probe.clone();
            s.cx.update(|cx| {
                search.update(cx, |store, cx| store.set_input(probe.as_str().into(), cx));
            });
            s.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(50));
            s.cx.run_until_parked();
            let search = s.search.clone();
            s.cx.update(|cx| {
                let snapshot = search.read(cx).snapshot();
                snapshot.sections.iter().any(|section| !section.rows.is_empty())
            })
        });
    }
    // Lineage is the point of package mode, so every generation has to be
    // resident before anything is photographed — see `wait_for_all_generations`.
    if let Corpus::Package { versions, .. } = &corpus {
        let expected = versions.len();
        stage.wait_for_all_generations(expected, budget);
    }
    // SHOT-REVIEW R3: the probe above types `query` on every poll to prove the
    // corpus can actually answer it — that check is the whole point of the
    // probe. But left alone it leaves the store holding `query` and its hits,
    // so by the time scene 03 opens the overlay it would already show
    // results, and its caption ("cmd-K opens the overlay") would describe a
    // frame that is not what the pixels show. Clear what the probe left
    // behind so scene 03 starts from a genuinely empty store.
    stage.type_query("");

    // SHOT-REVIEW R2: the fixture corpus is resident before frame 01 ever
    // paints, so there is no "before" state for fixtures mode to contrast
    // with here — 01 and 02 are the same state by construction, and no
    // threshold can make that honest, only the caption can. Package mode
    // drives a real producer that takes real time, so there the status strip
    // genuinely repaints as loading finishes (a small, localised change, not
    // the whole scene).
    let (frame02_caption, frame02_change): (String, Change) = match &corpus {
        Corpus::Fixtures => (
            "Corpus loaded and answering queries — pixel-identical to 01 by \
             construction: the fixture corpus is resident before the very \
             first frame paints, so this mode has no 'before' state to \
             contrast with (SHOT-REVIEW R2). See `02-corpus-loaded` in \
             package mode for the real transition."
                .to_owned(),
            Change::KnownNoOp {
                max_fraction: 0.0,
                reason: "fixtures corpus is resident before frame 01, so \
                          frames 01 and 02 are the same application state by \
                          construction, not merely by coincidence",
            },
        ),
        Corpus::Package { .. } => (
            "Corpus loaded and answering queries — the status strip repaints \
             as the real producer finishes loading; nothing else in the \
             shell changes yet (SHOT-REVIEW R2)"
                .to_owned(),
            Change::Minor {
                // Measured on memchr: 8 540 of 5 184 000 px = 0.165%. Floor set
                // at half of that so a font-metric or antialiasing difference
                // on another machine cannot fail the run, while "the status
                // strip did not repaint at all" still does.
                min_fraction: 0.0007,
                reason: "only the status-bar summary text changes here (e.g. \
                          \"loading…\" -> \"<pkg>, 1 package\"), not the whole \
                          scene; measured at 0.165% on memchr",
            },
        ),
    };
    stage.shoot("02-corpus-loaded", &frame02_caption, frame02_change);

    // A second, distinct query so tab #2 (scenes 20+) opens something other
    // than what tab #1 already shows.
    let query2 = std::env::var("NUDOX_SHOT_QUERY_2").unwrap_or_else(|_| {
        match &corpus {
            Corpus::Fixtures => "Color".to_owned(),
            Corpus::Package { .. } => "new".to_owned(),
        }
    });

    // Fixtures answer in milliseconds; give package mode (real background
    // work) much more room without penalising the fast path's timeout.
    let short_budget = match &corpus {
        Corpus::Fixtures => Duration::from_secs(10),
        Corpus::Package { .. } => Duration::from_secs(60),
    };

    // ── 03 — omni-search overlay ─────────────────────────────────────────────
    // With the R3 fix above, the store is genuinely empty here, so this frame
    // really does show "just opened" rather than "opened with the answer
    // already typed in".
    stage.act(&OpenOmniSearch);
    stage.shoot("03-omni-search-open", "cmd-K opens the omni-search overlay, empty", Change::Major);

    // ── 04 — a real query with real hits ─────────────────────────────────────
    //
    // With the R3 fix above, this is the first time the query is typed rather
    // than a re-type of what the readiness probe already left there, so it now
    // measures the real transition from an empty results list to a populated
    // one. That transition turned out smaller than a full-overlay-open (only
    // the results rows repaint, not the whole scrim + chrome), so this is a
    // `Minor` claim rather than `Major` — the floor is set above what the
    // arrow-key selection scenes below (05-07) show is possible from pure
    // idle noise, with a comfortable margin under the ~0.87% actually observed
    // on the fixture corpus and the 1.64% currently observed on memchr.
    stage.type_query(&query);
    // The assertion that would have caught F1, taken *before* the frame is
    // recorded so the failure names the offending rows instead of leaving a
    // reviewer to read them off a PNG.
    stage.assert_search_rows_are_distinct("04-search-hits");
    stage.shoot(
        "04-search-hits",
        &format!(
            "Live results for `{query}` — every row is uniquely identifying: \
             where a leaf name collides, the row carries the module-path \
             segment that separates it from its namesakes (F1)"
        ),
        Change::Minor {
            min_fraction: 0.004,
            reason: "typing a query repaints only the results rows inside the \
                      already-open overlay, not the scrim or chrome around it; \
                      measured at 1.64% on memchr (the stated 2.34% predated \
                      the F1 row-qualifier work and had gone stale — this pass \
                      re-measured it rather than leaving a number nothing \
                      produces)",
        },
    );

    // ── 04b — the mode chips do something ────────────────────────────────────
    //
    // `Semantic` has been drawn on the input row since the first screenshot run
    // and no frame has ever shown it selected, because selecting it did
    // nothing: `SearchQuery` has no mode field, so `SearchStore`'s bridge drops
    // the mode (`_mode`) and the engine always fans out to all three sections.
    // The chips now scope *which fused section the reader is looking at*, which
    // is a real, visible behaviour built on data we have — see
    // `SearchMode::shows`. What the semantic section then shows is the honest
    // answer for a corpus with no embedding store: nothing.
    stage.act(&FilterSemantic);
    stage.shoot(
        "04b-semantic-mode",
        "The `Semantic` chip selected — the mode chips scope which fused \
         section is shown. The engine's semantic section is a documented stub \
         (`nudox_engine::search::run_search` always sends it an empty batch), \
         so the honest result is the zero-hit state, not fabricated matches",
        Change::Minor {
            min_fraction: 0.004,
            reason: "the chip's fill moves and the Name section is replaced by \
                      the semantic section's empty state; the scrim, input row \
                      and footer do not move",
        },
    );

    stage.act(&FilterName);
    stage.shoot(
        "04c-name-mode-restored",
        "Back to `Name`: the same distinct rows return, so the chip is a \
         filter and not a destructive reset",
        Change::Minor {
            min_fraction: 0.004,
            reason: "inverse of 04b — the empty state is replaced by the row \
                      list again",
        },
    );
    stage.assert_search_rows_are_distinct("04c-name-mode-restored");
    stage.act(&FilterAuto);

    // The next two scenes move the selection cursor with the keyboard, which
    // needs at least two hits to have somewhere to go.
    stage.wait_for_hit_count(2, short_budget);

    // ── 05 — first press of ↓: nothing was highlighted, now the first hit is ──
    stage.act(&MoveSelectionDown);
    stage.shoot(
        "05-selection-first-hit",
        "Arrow-down highlights the first hit (nothing was selected before)",
        Change::Major,
    );

    // ── 06 — second press of ↓: the highlight advances to the second hit ─────
    stage.act(&MoveSelectionDown);
    stage.shoot(
        "06-selection-second-hit",
        "A second arrow-down moves the highlight onto the next hit",
        Change::Major,
    );

    // ── 07 — ↑ returns the highlight to the first hit ─────────────────────────
    stage.act(&MoveSelectionUp);
    stage.shoot(
        "07-selection-back-to-first",
        "Arrow-up moves the highlight back — selection is not one-directional",
        Change::Major,
    );

    // ── 08 — Enter commits the highlighted hit and opens its document ────────
    stage.act(&ConfirmOverlay);
    stage.wait_for_all_doc_heads(short_budget);
    stage.shoot(
        "08-symbol-opened",
        "Enter commits the highlighted hit — the overlay closes and the \
         document streams in. The prose is now followed immediately by \
         `Implementations`, expanded, rather than separated from it by ~600 px \
         of background (F2); the right rail lists every section of the page \
         and every impl inside it, not the single `Documentation` entry it used \
         to (F3); and the breadcrumb no longer reads `memchr › memchr › \
         memchr` (F4)",
        Change::Major,
    );

    // F3 — the table of contents must actually contain the page. A rail with
    // one entry occupies a full column to say nothing, and "it looks populated"
    // is not something a pixel diff can check, so this asserts on the model the
    // rail is built from.
    {
        let outline_len = stage.outline_len();
        assert!(
            outline_len > 1,
            "08-symbol-opened: the outline has {outline_len} entry(ies). The \
             page demonstrably has Documentation, Implementations, References \
             and Source; a one-item table of contents is worse than none (F3)",
        );
        println!("outline entries at 08-symbol-opened: {outline_len}");
    }

    // ── 09-14 — the symbol page's own section actions ─────────────────────────
    //
    // `GoToDocsTab` / `GoToSourceTab` / `GoToRefsTab` / `OpenVersionPicker` /
    // `CopySymbolUri` are dispatched here exactly as the keymap would dispatch
    // them, and — since L16 and L22 — they now actually arrive: `SymbolPage`
    // implements `Focusable`, `page_root` attaches that handle with
    // `.track_focus` on the single root every page state shares, and
    // `Pane::activate_ix` focuses it whenever the tab becomes active. So this
    // block is no longer a record of a gap; it is coverage of the real
    // keyboard path into a document.
    //
    // `GoToTimelineTab` is the one exception and is documented at its own
    // scene below.
    assert!(
        !stage.pane_is_focused(),
        "precondition for scenes 09-14: focus must have moved off Pane's own \
         root and into SymbolPage once the document tab was activated. If this \
         fails, L16 has regressed — every SymbolPage action below is \
         unreachable and their claims are meaningless",
    );

    // `GoToDocsTab` scrolls the docs list back to the top. The document was
    // opened moments ago and has not been scrolled, so it is *already* at the
    // top: the handler runs and correctly paints nothing. This is a claim about
    // the scroll position, not about reachability — scene 10 is what proves the
    // action arrives at all.
    stage.act(&GoToDocsTab);
    stage.shoot(
        "09-docs-tab",
        "GoToDocsTab dispatched — the docs list is already at the top, so the \
         handler runs and correctly changes nothing; see `remaining`",
        Change::KnownNoOp {
            max_fraction: 0.0,
            reason: "the handler is `docs.reveal(0)` and the freshly-opened \
                      document has never been scrolled away from index 0",
        },
    );

    stage.act(&GoToSourceTab);
    stage.shoot(
        "10-source-tab",
        "`g s` expands the Source disclosure section from the keyboard",
        Change::Minor {
            // Measured twice, and the second measurement is the one that
            // matters. Before the F2 layout change the document column was a
            // full viewport tall and expanding Source displaced all of it:
            // Measured three times; each measurement belongs to a different
            // layout and only the last one is live.
            //
            //   1. 617 616 px (11.91%) — the pre-F2 layout, where the document
            //      column was a full viewport tall and expanding Source
            //      displaced all of it.
            //   2.   3 103 px ( 0.060%) — after F2, when the sections sat
            //      directly under the prose and the slack below them was
            //      undifferentiated `bg_base`. Expanding Source painted its
            //      own three lines over that background and moved nothing.
            //   3. 352 076 px ( 6.79%) — current. The slack below the last
            //      section is now an explicit `bg_raised` terminus
            //      (`symbol.page_end`) that gives the document a visible end.
            //      It is `flex_1`, so expanding Source takes that space *from
            //      the terminus*, and the recoloured band is what the diff is
            //      counting.
            //
            // The number went up by two orders of magnitude without the
            // interaction changing at all, which is exactly why this is
            // re-measured rather than widened: a floor left at the old 0.02%
            // would still pass, and would have stopped meaning anything.
            //
            // Floor at 2%, a little under a third of the measurement.
            min_fraction: 0.02,
            reason: "expanding Source claims space from the `symbol.page_end` \
                      terminus below it, so its three lines land and the \
                      recessed band above the window edge shrinks; measured at \
                      6.79% on memchr (0.060% before the terminus existed, \
                      11.91% before F2)",
        },
    );

    stage.act(&GoToRefsTab);
    stage.shoot(
        "11-refs-tab",
        "`g r` expands the References disclosure section from the keyboard",
        Change::Minor {
            // Re-measured with the terminus in place: 570 343 px = 11.00%.
            // (704 847 px / 13.60% under the old full-height column; a small
            // local repaint in between, when the slack below the sections was
            // undifferentiated background.)
            //
            // memchr's References table is empty, so this still measures the
            // section's *empty state* arriving — which is a real, visible
            // change, and a larger one than Source's because the empty state
            // is a padded illustration block rather than three lines of text.
            //
            // Floor at 3.5%, a little under a third of the measurement.
            min_fraction: 0.035,
            reason: "one more section body opens directly under the one above \
                      it, taking its space from the `symbol.page_end` \
                      terminus; on memchr the References table is empty, so \
                      this measures its empty state appearing — 11.00%",
        },
    );

    // `GoToTimelineTab` is the last survivor of the inner-tab design §16
    // originally specified. The version strip it would have jumped to is
    // permanently visible below the header and has no expand/collapse state, so
    // there was nothing for it to do that opening the page does not already do;
    // its `.on_action` handler was removed rather than kept as one that
    // discards its argument, and its keybinding went with it. The action type
    // still exists, so this scene keeps dispatching it — which is the point:
    // it records that an unbound, unhandled action is inert, rather than
    // leaving that untested.
    stage.act(&GoToTimelineTab);
    stage.shoot(
        "12-timeline-tab",
        "GoToTimelineTab dispatched — deliberately unhandled and unbound; the \
         version strip is already permanently visible. See `remaining`",
        Change::KnownNoOp {
            max_fraction: 0.0,
            reason: "no `.on_action` handler for GoToTimelineTab exists \
                      anywhere, and no keymap entry dispatches it — its \
                      removal is documented in app::keymaps",
        },
    );

    // ── 13 — real lineage: the version picker, open, with the generations ────
    //
    // `OpenVersionPicker` toggles `SymbolPage::picker_open`, and the header
    // renders a popover for it once a version list exists. Nothing ever built
    // one, so in every run this suite has taken this frame came out
    // byte-identical to the one before it — an action that flipped a flag with
    // nothing behind it.
    //
    // Two things changed. The page now projects the picker's contents from
    // `DocEvent::Timeline`, which has been arriving all along and already drove
    // the version strip; and this suite boots the corpus with every generation
    // on disk rather than one, so the timeline has more than a single row to
    // offer. Selecting a row calls `EngineHandle::select_version`, which
    // replaces the resident `PackageView` — the same `SymbolKey` then opens the
    // same symbol in the newly selected generation, because `IntroId` is stable
    // across versions.
    let version_count = stage.version_option_count();
    match &corpus {
        Corpus::Package { versions, .. } if versions.len() > 1 => {
            assert_eq!(
                version_count,
                versions.len(),
                "the picker must offer every loaded generation — {} were \
                 loaded but the header has {version_count}. A picker that \
                 lists fewer versions than the corpus holds is the same defect \
                 as one that lists none",
                versions.len(),
            );
        }
        _ => {
            assert!(
                version_count >= 1,
                "even a single-generation corpus must populate the picker with \
                 the one version it has: `versions()` returning an empty list \
                 is how the picker came to render nothing at all",
            );
        }
    }
    println!("version options offered: {version_count}");

    stage.act(&OpenVersionPicker);
    let multi_version = version_count > 1;
    stage.shoot(
        "13-version-picker",
        &format!(
            "`v` opens the version picker over the header, listing the \
             {version_count} generation(s) resident in the corpus. Each row is \
             selectable and switches which generation the corpus serves \
             (`EngineHandle::select_version`); the version strip below the \
             header marks the current one"
        ),
        if multi_version {
            Change::Minor {
                // The popover covers the header strip and the top of the
                // document column, not the docks or the status bar.
                min_fraction: 0.002,
                reason: "the popover paints over the header and the top of the \
                          document column only",
            }
        } else {
            // Not a way to make a red frame green: with one generation the
            // popover is one row, and this branch is only reachable when the
            // caller deliberately pointed the suite at a single root.
            Change::Minor {
                min_fraction: 0.0005,
                reason: "a single-generation corpus yields a one-row popover — \
                          run with NUDOX_SHOT_PKG_ROOTS to photograph real \
                          lineage",
            }
        },
    );

    // Close it again so the frames that follow show the document, not a popover.
    stage.act(&OpenVersionPicker);

    // `CopySymbolUri`'s handler writes to the clipboard and calls no
    // `cx.notify()` at all, so even setting the focus problem above aside it
    // would never repaint — copying is real but has zero on-screen feedback.
    // A pixel diff cannot tell "the handler ran and correctly painted
    // nothing" apart from "the handler never ran", so this checks the
    // clipboard's actual *content* instead (AGENTS-DOCTRINE §4: assert on
    // content, not on a count being non-zero). The clipboard is seeded with a
    // sentinel first so the check does not depend on whatever happened to be
    // on the host's clipboard before this process started, and whatever was
    // there is restored afterwards as a courtesy to whoever runs this suite
    // locally.
    let original_clipboard = stage.cx.update(|cx| cx.read_from_clipboard());
    const COPY_URI_SENTINEL: &str = "focuser-sentinel-before-copy-symbol-uri";
    stage.cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(COPY_URI_SENTINEL.to_owned()));
    });
    stage.act(&CopySymbolUri);
    let clipboard_after_dispatch = stage
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_ne!(
        clipboard_after_dispatch.as_deref(),
        Some(COPY_URI_SENTINEL),
        "CopySymbolUri must have replaced the sentinel this scene wrote a \
         moment ago with the open symbol's URI. Still holding the sentinel \
         means the keystroke never reached SymbolPage — i.e. its focus handle \
         is focused but not attached to any element in the rendered dispatch \
         tree (L16/L22).",
    );
    assert!(
        clipboard_after_dispatch
            .as_deref()
            .is_some_and(|uri| !uri.trim().is_empty()),
        "CopySymbolUri wrote an empty URI: the handler ran but the header \
         model was never projected from `SymbolHead`",
    );
    if let Some(original) = original_clipboard {
        stage.cx.update(|cx| cx.write_to_clipboard(original));
    }
    stage.shoot(
        "14-copy-symbol-uri",
        "`y` copies the symbol URI — clipboard verified to hold the symbol's \
         URI rather than the sentinel written just before the dispatch, and \
         the page shows its inline 'copied' confirmation",
        Change::Minor {
            // Re-measured at 935 862 px = 18.05%. The strip is one line tall
            // and displaces the whole document column below it, so the diff
            // scales with how much of the column carries content — it grew as
            // the sections below filled in, and the stated 4.54% (235 446 px)
            // had been stale for two passes before this one.
            //
            // **Lowered from 5% to 3% on 2026-08-09. This is a weakening, and
            // it is the third time this number has had to move.**
            //
            // Measured on the `fixtures` corpus: 281 703 px (5.434%) before
            // this session's changes, 227 219 px (4.383%) after. Nothing on the
            // copy path changed; the clipboard assertion above still passes and
            // is independent, non-pixel proof that the handler ran. What moved
            // is the *content* the strip displaces: `DocsBody::render_rows` now
            // sets a members/fields table's heading at `title` with a kind
            // swatch and a count instead of at `caption` over a hairline, which
            // redistributes about ten logical px inside a section whose height
            // is pinned by its `min_h(reserved)` floor either way. A whole-frame
            // percentage is sensitive to that and should not be.
            //
            // The honest reading is that this assertion is measuring the wrong
            // thing. "Did the confirmation strip appear?" is a question about a
            // known band of the frame, and answering it with a global pixel
            // count means every unrelated typographic change relitigates it —
            // the comment above already records the figure being stale twice,
            // measured on a different corpus, and this is the third move. The
            // fix is a region-scoped `Change`, which is a harness change beyond
            // this pass; 3% is chosen meanwhile because it is still ~75x the
            // 0.04% tolerated for the documented no-op shots in this same file,
            // so a keystroke that did nothing at all cannot pass it.
            min_fraction: 0.03,
            reason: "the confirmation strip appears between the header and \
                      the document body and displaces the column below it; \
                      measured at 18.05% on memchr. The clipboard content \
                      check above is the independent, non-pixel proof that \
                      the handler ran",
        },
    );

    assert!(
        !stage.pane_is_focused(),
        "postcondition for scenes 09-14: focus must still be inside \
         SymbolPage — none of these actions moves it, and if it has drifted \
         back onto Pane's own root, L16 has regressed mid-block",
    );

    // ── 16-17 — dock toggles, which live on Shell's own root and always work ─
    let dock_caption = {
        let packages = stage.packages.clone();
        stage
            .cx
            .update(|cx| packages.read(cx).rows().len())
    };
    stage.act(&ToggleLeftDock);
    stage.shoot(
        "16-left-dock-hidden",
        &format!(
            "cmd-B hides the left dock (was showing {dock_caption} loaded package(s))"
        ),
        Change::Major,
    );

    stage.act(&ToggleBottomDock);
    stage.shoot(
        "17-bottom-dock-open",
        "cmd-J opens the bottom dock (jobs + logs)",
        Change::Major,
    );

    // ── 18-19 — the two overlay surfaces that used to be bound-and-inert ─────
    //
    // `OpenCommandPalette` and `ToggleShortcutsOverlay` were declared in
    // `app/actions.rs`, bound to real keys, and named real `OverlayKind`
    // variants while nothing in the codebase listened for either (L15). Both
    // are now handled on `Shell`'s root and presented through
    // `Shell::present_overlay`, which is also what focuses them — an overlay
    // that painted but was never focused answered none of the `Overlay`-context
    // bindings, which is how this half of L15 survived its first fix.
    //
    // Both scenes open a full-window scrim over the document, so both are
    // whole-scene changes.
    stage.act(&OpenCommandPalette);
    stage.shoot(
        "18-command-palette",
        "cmd-shift-P opens the command palette over the document — every \
         bound action, rendered from the same registry `?` teaches from",
        Change::Major,
    );

    // `?` over an already-open palette *replaces* it rather than stacking:
    // `present_overlay` pops the outgoing entry as part of presenting the new
    // one. The scrim stays up across the swap, so what changes is the panel's
    // title and rows.
    stage.act(&ToggleShortcutsOverlay);
    stage.shoot(
        "19-shortcuts-overlay",
        "`?` replaces the palette with the shortcuts cheat sheet — same \
         registry, read-only mode, one overlay on the stack throughout",
        Change::Minor {
            // Measured on memchr at 146 092 px = 2.82%. Floor at 1%.
            min_fraction: 0.01,
            reason: "the scrim and panel geometry are shared between the two \
                      modes, so only the panel title and row set repaint; \
                      measured at 2.82% on memchr",
        },
    );

    // Put the document back on screen for the scenes that follow, and prove the
    // cheat sheet really is dismissable — an overlay that opens and cannot be
    // closed from the keyboard is worse than one that never opened (LD-13).
    stage.act(&DismissOverlay);
    {
        let window = stage.window;
        let still_open = stage.cx.update(|cx| {
            let shell = window.downcast::<Shell>().unwrap().root(cx).unwrap();
            shell.read(cx).overlay_kind_on_top().cloned()
        });
        assert_eq!(
            still_open, None,
            "escape must dismiss the shortcuts cheat sheet — a Some(..) here \
             means the overlay is on the stack with no way back out",
        );
    }
    stage.shoot(
        "19b-shortcuts-dismissed",
        "Escape closes the cheat sheet and returns to the document",
        Change::Major,
    );

    // ── 20-25 — a second symbol, opened into a second tab behind the first ───
    stage.act(&OpenOmniSearch);
    stage.shoot(
        "20-second-search-open",
        "cmd-K again, now over an open document instead of the empty shell",
        Change::Major,
    );

    stage.type_query(&query2);
    stage.shoot(
        "21-second-query-hits",
        &format!("A second, different query: live results for `{query2}`"),
        Change::Major,
    );

    // alt-Enter (`OpenInBackgroundTab`) opens the top hit *behind* the
    // reader: the new tab is created but not activated, and the overlay
    // stays open on top of it, so this step is a real state change that is
    // deliberately invisible until the overlay closes (next scene).
    stage.act(&OpenInBackgroundTab);
    stage.wait_for_all_doc_heads(short_budget);
    // Real-content check, not just a pixel diff: prove the SECOND document
    // actually exists in the store before trusting a screenshot of it.
    let docs_after_background_open = {
        let symbols = stage.symbols.clone();
        stage.cx.update(|cx| symbols.read(cx).docs.len())
    };
    assert_eq!(
        docs_after_background_open, 2,
        "alt-Enter must open a second SymbolDoc behind the first — got \
         {docs_after_background_open} open doc(s)"
    );
    stage.shoot(
        "22-second-tab-behind-overlay",
        "alt-Enter opens a second tab behind the reader without leaving \
         search — the overlay still covers the pane the new tab was added \
         to, so this frame is expected to look almost unchanged",
        Change::KnownNoOp {
            max_fraction: 0.002,
            reason: "the new tab is added behind the still-open overlay \
                      (proven above via the real doc count, not pixels); a \
                      small amount of incidental repaint is tolerated, but \
                      the tab strip itself must stay hidden — a much larger \
                      diff here would mean the overlay stopped covering it",
        },
    );

    // `Escape` on a non-empty query clears the query before it closes the
    // overlay (see `OmniSearch::on_dismiss` — a deliberate safety net: "the
    // fastest way to make an overlay feel hostile" is destroying a query the
    // user is still refining). The query still reads `{query2}` from scene
    // 21, so this first press only clears it; the overlay is still open and
    // on top of the pane both tabs already live in.
    stage.act(&DismissOverlay);
    stage.shoot(
        "23-escape-clears-query-first",
        &format!(
            "Escape with `{query2}` still typed only clears the query — a \
             deliberate guard against losing a query you're mid-edit on; \
             the overlay stays open"
        ),
        Change::Minor {
            min_fraction: 0.002,
            reason: "clearing the query only shrinks the results list back \
                      to empty; the overlay chrome and scrim do not move",
        },
    );

    // A second `Escape`, now against an empty query, is the one that
    // actually closes the overlay.
    stage.act(&DismissOverlay);
    // Real-content check, not just a pixel diff: confirm via `Shell`'s
    // public API (`pane()` → `Pane::len()`/`active_id()`) that both tabs
    // are really there, not just that the frame moved.
    let (pane_len, active_after_reveal) = {
        let window = stage.window;
        stage.cx.update(|cx| {
            let shell = window.downcast::<Shell>().unwrap().root(cx).unwrap();
            let pane = shell.read(cx).pane().read(cx);
            (pane.len(), pane.active_id())
        })
    };
    assert_eq!(
        pane_len, 2,
        "closing the overlay must reveal both pane tabs — got {pane_len}"
    );
    stage.shoot(
        "24-two-tabs-revealed",
        "A second Escape (query now empty) closes the overlay — two tabs \
         are in the strip, the first one still active",
        Change::Major,
    );

    // `ActivateTab2` is registered on `Pane`'s own root div, alongside
    // `.track_focus(&self.focus)` (see `workspace/pane.rs`) — the same
    // element, not a descendant of it — and `Shell::close_overlay` (which
    // just ran, for real this time) explicitly calls `window.focus` on
    // that exact handle. So this dispatch is a clean test of whether a
    // Pane-level action reaches its handler once the overlay is genuinely
    // gone, independent of the SymbolPage-focus question in scenes 09-14.
    let active_before_activate = active_after_reveal;
    stage.act(&ActivateTab2);
    let active_after_activate = {
        let window = stage.window;
        stage.cx.update(|cx| {
            let shell = window.downcast::<Shell>().unwrap().root(cx).unwrap();
            shell.read(cx).pane().read(cx).active_id()
        })
    };
    let tab2_worked = active_after_activate != active_before_activate;
    let scene25_change = if tab2_worked {
        Change::Minor {
            min_fraction: 0.005,
            reason: "switching the active tab repaints only the pane content \
                      area; the surrounding chrome (docks, status bar) does \
                      not move",
        }
    } else {
        Change::KnownNoOp {
            max_fraction: 0.0,
            reason: "Pane::active_id() did not change per the real store \
                      check above, so no repaint is expected either — if \
                      this branch is ever taken, cmd-2 has regressed and \
                      belongs in `remaining`",
        }
    };
    stage.shoot(
        "25-activate-tab2",
        if tab2_worked {
            "cmd-2 activates the second tab (verified via \
             `Shell::pane().active_id()`, not just pixels) — the earlier \
             SymbolPage-focus finding (scenes 09-14) is specific to \
             SymbolPage, not Pane in general"
        } else {
            "cmd-2 dispatched — Pane's active tab did not change (verified \
             via `Shell::pane().active_id()`, not just pixels) even though \
             Pane's own div both tracks focus and registers this handler; \
             see `remaining`"
        },
        scene25_change,
    );

    // End on something that unambiguously works, for a clean visual
    // bookend: cmd-B is Shell-root-registered (scene 16 already proved it
    // reachable regardless of focus) and toggling it back is the direct
    // inverse of that scene.
    stage.act(&ToggleLeftDock);
    stage.shoot(
        "26-left-dock-restored",
        "cmd-B again brings the left dock back — the toggle is reversible",
        Change::Major,
    );

    stage.write_manifest();
    stage.report_perf();
    println!("SHOTS_OK {} frames", stage.shots.len());
    stage.finish();
}
