//! Real pixels: does a repaired link actually *look* different from an
//! authored one?
//!
//! # What this proves that a unit test cannot
//!
//! `repaired_link_projects_to_its_own_run_style_and_note` asserts that a
//! repaired link projects to `RunStyle::RepairedLink`. That is a statement
//! about a Rust enum. It stays green even if `RunStyle::RepairedLink` and
//! `RunStyle::Link` happen to rasterise to the same picture — for instance if
//! `warn` and `accent` are the same hue in some theme, or if `wavy` turns out
//! to be a no-op at hairline thickness, or if someone "simplifies"
//! `run_highlight_style` by folding the two arms together.
//!
//! In every one of those cases the mark the reader is supposed to see does not
//! exist, and the whole point of this feature is gone, silently. Doctrine §8:
//! two byte-identical frames with different captions is the exact failure a
//! screenshot suite exists to catch.
//!
//! So this file rasterises two paragraphs through the *same* projection
//! (`RichText::from_runs`) and the *same* paint (`run_highlight_style`) the
//! app uses — no duplicated style map, nothing re-derived — and asserts the
//! frames differ by more than a rounding error.
//!
//! # Why `harness = false`
//!
//! Same reason as `shot_probe.rs` and `screenshots.rs`: libtest runs `#[test]`
//! bodies on spawned worker threads, and constructing the macOS platform
//! touches AppKit, which aborts off the main thread. With no harness this file
//! *is* `fn main`.
//!
//! # Running
//!
//! ```text
//! cargo test --manifest-path workspace/gui/Cargo.toml --test repair_paint
//! ```
//!
//! It is filtered out of nextest's `default-filter` alongside the other
//! `harness = false` binaries (doctrine §8: one of them poisons the whole
//! nextest invocation).

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, HighlightStyle, IntoElement, ParentElement as _, Render, Styled as _,
    StyledText, div, px, size,
};
use image::RgbaImage;

use lindsey::theme::ext::{NudoxThemeExt, ThemeExtAccessor as _};
use lindsey::views::symbol_page::docs::{RichText, run_highlight_style};
use nudox_engine::wire::{
    InlineRun, LinkOrigin, LinkRepair, LinkRepairKind, LinkTarget, SymbolKey,
};
use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName};

/// A paragraph rendered exactly the way the Docs tab renders one: a single
/// `StyledText` plus `(byte range, HighlightStyle)` pairs.
struct Paragraph {
    runs: Vec<InlineRun>,
}

impl Render for Paragraph {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rich = RichText::from_runs(&self.runs);
        let (border_width, colours) = {
            let ext = cx.theme_ext();
            (ext.space.border_width, ext.colours)
        };
        let styles: Vec<(Range<usize>, HighlightStyle)> = rich
            .styles()
            .iter()
            .map(|(range, style)| {
                (
                    range.clone(),
                    run_highlight_style(*style, &colours, border_width),
                )
            })
            .collect();

        div()
            .size_full()
            .bg(colours.bg_base)
            .child(
                div()
                    .m(px(40.))
                    .p(px(24.))
                    // A large type size so the hairline underline is several
                    // pixels of ink rather than one anti-aliased row that a
                    // difference threshold could plausibly dismiss as noise.
                    .text_size(px(32.))
                    .child(StyledText::new(rich.text().clone()).with_highlights(styles)),
            )
    }
}

fn key() -> SymbolKey {
    SymbolKey::new(
        PackageLineageId::new(EcosystemId::new("test"), PackageName::new("repair-paint")),
        IntroId::from_raw([9; 32]),
    )
}

/// The two paragraphs are **identical in text** and differ only in `origin`.
///
/// That is deliberate and load-bearing: if the text differed, the frames would
/// differ for a reason that has nothing to do with the repair mark, and the
/// assertion below would prove nothing at all.
fn authored_runs() -> Vec<InlineRun> {
    vec![
        InlineRun::Text {
            text: "created by ".into(),
        },
        InlineRun::Link {
            text: "memrchr_iter".into(),
            target: LinkTarget::Symbol { key: key() },
            origin: LinkOrigin::Authored,
        },
    ]
}

fn repaired_runs() -> Vec<InlineRun> {
    vec![
        InlineRun::Text {
            text: "created by ".into(),
        },
        InlineRun::Link {
            text: "memrchr_iter".into(),
            target: LinkTarget::Symbol { key: key() },
            origin: LinkOrigin::Repaired(LinkRepair {
                kind: LinkRepairKind::TransposedOpenDelimiter,
                raw: "`[memrchr_iter`]".into(),
                resolved: "memrchr_iter".into(),
                note: "Repaired link.".into(),
            }),
        },
    ]
}

/// Pixels that differ between two frames of identical dimensions.
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

/// Opaque, non-flat: the frame actually painted something.
fn assert_painted(image: &RgbaImage, what: &str) {
    let total = (image.width() as u64 * image.height() as u64).max(1);
    let opaque = image.pixels().filter(|p| p.0[3] == u8::MAX).count() as u64;
    assert!(
        opaque as f64 / total as f64 > 0.95,
        "{what}: only {opaque}/{total} pixels are opaque — the scene did not paint"
    );
    let mut palette = std::collections::HashSet::with_capacity(256);
    for p in image.pixels() {
        if palette.len() < 256 {
            palette.insert(p.0);
        }
    }
    assert!(
        palette.len() >= 8,
        "{what}: only {} distinct colours — a flat fill means nothing rendered",
        palette.len()
    );
}

fn main() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.shots");
    std::fs::create_dir_all(&out).expect("create .shots");

    let platform = gpui_platform::current_platform(true);
    let text_system = platform.text_system();

    let mut cx = gpui::HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_component_assets::Assets),
        || gpui_platform::current_headless_renderer(),
    );

    cx.update(|cx| {
        gpui_component::init(cx);
        // `ThemeMode::default()` is Light and nothing here chooses, so the mode
        // is stated explicitly — and *before* `NudoxThemeExt::init`, which
        // picks its palette by asking `cx.theme().is_dark()`.
        gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
        NudoxThemeExt::init(cx).expect("bundled themes parse and install");
    });

    let mut shoot = |runs: Vec<InlineRun>, slug: &str| -> RgbaImage {
        let window = cx
            .open_window(size(px(900.), px(220.)), |_, cx| {
                cx.new(|_| Paragraph { runs })
            })
            .expect("open headless window");
        cx.run_until_parked();
        let image = cx
            .capture_screenshot(window.into())
            .expect("capture_screenshot must work");
        let path = out.join(format!("repair-paint-{slug}.png"));
        image.save(&path).expect("save png");
        println!("SHOT {slug} {}x{} -> {}", image.width(), image.height(), path.display());
        image
    };

    let authored = shoot(authored_runs(), "authored");
    let repaired = shoot(repaired_runs(), "repaired");

    assert_painted(&authored, "authored frame");
    assert_painted(&repaired, "repaired frame");

    let changed = differing_pixels(&authored, &repaired)
        .expect("both frames are the same size, so they must be comparable");

    println!("DIFF {changed} pixels differ between authored and repaired");

    // The two paragraphs carry the *same text*, so every differing pixel is
    // the repair mark. A hairline wavy underline under twelve characters at
    // 32 px is on the order of a few hundred pixels; a floor of 40 is far
    // above the anti-aliasing jitter of an identical scene (which is zero —
    // the renderer is deterministic) and far below what the mark produces.
    assert!(
        changed >= 40,
        "SILENT REPAIR: the repaired-link paint is invisible. Only {changed} \
         pixels differ between a paragraph whose link is authored and one whose \
         link is repaired, and the two carry identical text — so the wavy warn \
         underline is not reaching the screen. A mark the reader cannot see is \
         the same as no mark at all."
    );

    println!("REPAIR_PAINT_OK");
}
