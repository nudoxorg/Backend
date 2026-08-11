#!/usr/bin/env python3
"""Derive the DMG Finder-window background from the repo's one brand mark.

Run with no arguments from anywhere; paths are computed relative to this
file. Regenerate `dmg-background.svg` whenever `/logo.svg` changes — that is
the whole point of generating it instead of hand-drawing it separately.

    python3 workspace/gui/packaging/generate_dmg_background.py

The output is consumed by cargo-bundle's `osx_dmg_background` key
(`workspace/gui/Cargo.toml`), which parses this SVG itself (via `resvg`),
rasterises it to the volume's `.background/background.tiff` at 2x, and reads
the bounding boxes of the two elements named `app` and `applications` to
place the .app and /Applications icons — see
`src/bundle/dmg_finder.rs::decorate_volume` in cargo-bundle 0.11. Nothing here
invokes AppleScript or `create-dmg`; the SVG is the entire interface.

This script does no SVG rendering itself — it only extracts the logo's inner
markup (`<defs>` + drawing elements) as text and re-embeds it inside a nested
`<svg viewBox=...>` element, which lets the browser/resvg re-scale it in
place. That keeps the dependency surface at "read a file, slice a string,
write a file" — no rsvg/cairo/inkscape needed to run this script, even though
cargo-bundle itself uses `resvg` downstream to rasterise the result.
"""

from __future__ import annotations

import re
from pathlib import Path

PACKAGING_DIR = Path(__file__).resolve().parent
GUI_DIR = PACKAGING_DIR.parent
REPO_ROOT = GUI_DIR.parent.parent
LOGO_PATH = REPO_ROOT / "logo.svg"
OUTPUT_PATH = PACKAGING_DIR / "dmg-background.svg"

# Finder window / DMG canvas, in points. cargo-bundle reads this straight off
# the SVG's width/height and uses it verbatim as the mounted window's size.
WIDTH = 660
HEIGHT = 420

# Icon centers. cargo-bundle hardcodes the Finder icon size at 48pt
# (ICON_SIZE in dmg_finder.rs) regardless of what this file draws — see the
# bundling report for what that means for the art.
APP_CENTER = (180, 205)
APPLICATIONS_CENTER = (480, 205)
MARKER_SIZE = 96  # bounding box cargo-bundle measures; must exceed the 48pt icon.

# Colors lifted from logo.svg's own palette (its inner-pool + rim-grad
# stops), so the background reads as "the same object, zoomed out" rather
# than a color scheme invented separately.
BG_TOP = "#14172a"
BG_BOTTOM = "#0b0d16"
ACCENT = "#6b82d6"  # between rim-grad's #536cc6 stop and its lighter #d0d7f1 end


def extract_logo_body(svg_text: str) -> tuple[str, str]:
    """Return (viewBox, inner_markup) from the source logo SVG."""
    view_box_match = re.search(r'viewBox="([^"]+)"', svg_text)
    if not view_box_match:
        raise ValueError(f"{LOGO_PATH} has no viewBox attribute")
    view_box = view_box_match.group(1)

    open_tag_end = svg_text.index(">", svg_text.index("<svg")) + 1
    close_tag_start = svg_text.rindex("</svg>")
    inner_markup = svg_text[open_tag_end:close_tag_start].strip()
    return view_box, inner_markup


def build_background(view_box: str, logo_inner: str) -> str:
    # No <text> anywhere in this file: cargo-bundle's dmg_finder renders this
    # SVG through `resvg::Tree::from_data` with `Options::default()`, which
    # loads no font database. Confirmed by rendering — a first draft of this
    # file with <text> captions produced a background.tiff with the text
    # silently absent (checked by mounting the DMG and opening the actual
    # rendered .background/background.tiff, not by reading the SVG). The
    # drag cue is carried by the arrow alone, which is geometry, not glyphs.
    watermark_size = 300
    watermark_x = (WIDTH - watermark_size) / 2
    watermark_y = 24

    app_mark_x = APP_CENTER[0] - MARKER_SIZE / 2
    app_mark_y = APP_CENTER[1] - MARKER_SIZE / 2
    apps_mark_x = APPLICATIONS_CENTER[0] - MARKER_SIZE / 2
    apps_mark_y = APPLICATIONS_CENTER[1] - MARKER_SIZE / 2

    arrow_y = APP_CENTER[1]
    arrow_start_x = APP_CENTER[0] + MARKER_SIZE / 2 + 14
    arrow_end_x = APPLICATIONS_CENTER[0] - MARKER_SIZE / 2 - 22

    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">
  <defs>
    <linearGradient id="bg-wash" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{BG_TOP}"/>
      <stop offset="1" stop-color="{BG_BOTTOM}"/>
    </linearGradient>
    <radialGradient id="icon-plinth" cx="0.5" cy="0.5" r="0.5">
      <stop offset="0" stop-color="{ACCENT}" stop-opacity="0.24"/>
      <stop offset="1" stop-color="{ACCENT}" stop-opacity="0"/>
    </radialGradient>
    <marker id="arrowhead" viewBox="0 0 10 10" refX="8" refY="5"
            markerWidth="7" markerHeight="7" orient="auto-start-reverse">
      <path d="M0,0 L10,5 L0,10 z" fill="{ACCENT}"/>
    </marker>
  </defs>

  <rect x="0" y="0" width="{WIDTH}" height="{HEIGHT}" fill="url(#bg-wash)"/>

  <!-- Brand watermark, derived from /logo.svg verbatim (not redrawn), fully
       inside the canvas so the gem reads whole rather than cropped. -->
  <svg x="{watermark_x}" y="{watermark_y}" width="{watermark_size}" height="{watermark_size}"
       viewBox="{view_box}" opacity="0.16">
    {logo_inner}
  </svg>

  <!-- Soft plinths under each icon slot, centered on the same points named "app" / "applications" below -->
  <circle cx="{APP_CENTER[0]}" cy="{APP_CENTER[1]}" r="{MARKER_SIZE / 2}" fill="url(#icon-plinth)"/>
  <circle cx="{APPLICATIONS_CENTER[0]}" cy="{APPLICATIONS_CENTER[1]}" r="{MARKER_SIZE / 2}" fill="url(#icon-plinth)"/>

  <path d="M {arrow_start_x} {arrow_y} L {arrow_end_x} {arrow_y}"
        stroke="{ACCENT}" stroke-width="2.5" fill="none" opacity="0.9"
        marker-end="url(#arrowhead)"/>

  <!-- Position markers cargo-bundle's dmg_finder::decorate_volume reads by id.
       Invisible (opacity 0) but real geometry, so the bounding box is correct
       and nothing doubles up with the plinths drawn above. -->
  <rect id="app" x="{app_mark_x}" y="{app_mark_y}" width="{MARKER_SIZE}" height="{MARKER_SIZE}"
        fill="none" opacity="0"/>
  <rect id="applications" x="{apps_mark_x}" y="{apps_mark_y}" width="{MARKER_SIZE}" height="{MARKER_SIZE}"
        fill="none" opacity="0"/>
</svg>
"""


def main() -> None:
    if not LOGO_PATH.exists():
        raise SystemExit(f"logo not found at {LOGO_PATH}")
    svg_text = LOGO_PATH.read_text(encoding="utf-8")
    view_box, logo_inner = extract_logo_body(svg_text)
    output = build_background(view_box, logo_inner)
    OUTPUT_PATH.write_text(output, encoding="utf-8")
    print(f"wrote {OUTPUT_PATH} ({len(output)} bytes) from {LOGO_PATH}")


if __name__ == "__main__":
    main()
