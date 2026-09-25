# FACET fonts

Four faces, one job each: Bricolage Grotesque (display), Geist (UI), Geist
Mono (identifiers), Newsreader italic (the explaining voice). All are SIL Open
Font License 1.1; each `*-OFL.txt` here is the licence shipped by its source.

## Why static cuts

GPUI on macOS registers a variable font as one face, its default instance, and
never applies `wght`, `wdth` or `opsz` (measured: Geist at 300..800 and
Bricolage at 300..800 all resolved to one `FontId` with identical advances;
Bricolage's default instance is ExtraBold at opsz 96). So every coordinate the
boards use is cut as a static instance in `static/`, and `facet::fonts`
embeds only those (plus `GeistMono[wght].ttf` and `Newsreader[opsz,wght].ttf`,
whose default instances are the ones used). GPUI's CSS-style weight matching
then picks the cut inside a family.

| file | family | coordinates | used for |
|---|---|---|---|
| `static/BricolageGrotesque-520.ttf` | Bricolage Grotesque | opsz 18, wdth 100, wght 520 | light display heads |
| `static/BricolageGrotesque-620.ttf` | Bricolage Grotesque | opsz 18, wdth 100, wght 620 | `ty::DEPTH`, section/variant heads |
| `static/BricolageGrotesque-650.ttf` | Bricolage Grotesque | opsz 18, wdth 100, wght 650 | `ty::TITLE` |
| `static/BricolageGrotesque-720.ttf` | Bricolage Grotesque | opsz 18, wdth 100, wght 720 | heavy display (stats, package names) |
| `static/BricolageGrotesqueDisplay-620.ttf` | Bricolage Grotesque Display | opsz 96, wdth 100, wght 620 | `ty::DISPLAY` |
| `static/BricolageGrotesqueDisplay-640.ttf` | Bricolage Grotesque Display | opsz 96, wdth 100, wght 640 | `ty::DISPLAY_XL` |
| `static/BricolageGrotesqueHero-640.ttf` | Bricolage Grotesque Hero | opsz 96, wdth 92, wght 640 | `ty::HERO` (`.hero-name`) |
| `static/BricolageGrotesqueBook-640.ttf` | Bricolage Grotesque Book | opsz 17, wdth 95, wght 640 | `ty::BOOK` |
| `static/Geist-{400,500,600,700}.ttf` | Geist | wght 400/500/600/700 | UI |
| `static/Newsreader-Italic-400.ttf` | Newsreader (italic) | opsz 14, wght 400 | captions, margins, hints (12.5-16 px) |
| `static/NewsreaderLede-Italic-400.ttf` | Newsreader Lede (italic) | opsz 19, wght 400 | `ty::LEDE` |
| `GeistMono[wght].ttf` | Geist Mono | default wght 400 | identifiers |
| `static/GeistMono-{500,600}.ttf` | Geist Mono | wght 500/600 | heavier identifiers: peek and lens titles, the here capsule |
| `Newsreader[opsz,wght].ttf` | Newsreader (upright) | default opsz 18, wght 400 | never by design; registered so an upright request cannot fall back to a system face |

The coordinates are what Chrome resolves for the board CSS: the boards pin
`font-variation-settings:"opsz" 96` on `.t-display-xl`, `.t-display` and
`.hero-name`, map `font-stretch` to `wdth`, and otherwise use automatic
optical sizing (opsz = px size), which two serif cuts and one text-size
Bricolage cut bracket.

## Regenerating

The variable masters live in `masters/` (provenance and hashes:
`Nudox-Design-System/fonts/README.md`; `Geist[wght].ttf` is vercel/geist-font
v1.7.2, the other two google/fonts at `5174b333`). From the repository root,
with fontTools 4.63.0 from the nixpkgs pin the flake uses:

```sh
nix shell --impure --expr 'let p = (builtins.getFlake "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612").legacyPackages.aarch64-darwin; in p.python3.withPackages (ps: [ ps.fonttools ])' \
  -c python3 apps/facet/resources/fonts/instance.py
```

`instance.py` calls `fontTools.varLib.instancer.instantiateVariableFont` per
cut, rewrites the name table (family, subfamily, full and PostScript names),
sets `OS/2` weight/width/`fsSelection`, drops `DSIG`, and keeps timestamps, so
the output is byte-reproducible. After regenerating, update the SHA-256 pins
in `apps/facet/src/fonts.rs` (`fonts::verify` refuses mismatched bytes).

## Evidence

- Chrome, rendering the variable masters at the board coordinates, and GPUI,
  rendering these cuts (`facet-gallery capture --scene type-proof`), agree on
  every specimen's ink width to 0.0-0.5 % and ink mass to within 3.5 %.
- Geist 400 < 500 < 600 ink mass steps match Chrome's to 0.6 %.
- The hero cut's ink (14 815) is Chrome's wght 640 (15 232), not the variable
  default ExtraBold (18 640).
- Geist Mono has no `zero` feature (its default zero is slashed, like the
  boards); `facet::fonts::features` requests `zero` for parity and turns
  `liga` off, because the boards show `->` and `=>` as typed.
