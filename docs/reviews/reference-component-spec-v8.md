# Reference component spec v8

This is a visual-forensics contract for the Nudox package, documentation, and source views. It is a review artifact for implementation and capture work. It records the three external 1280x720 rasters, the four FACET design-system artboards, and the current weak responsive capture as a measured delta specimen. The machine-readable companion is [reference-component-ledger-v8.json](/Users/mileswirht/Downloads/backend-reference-component-spec-v8/docs/reviews/reference-component-ledger-v8.json).

The target evidence is separate from the weak capture. The external rasters establish package dossier, docs reader, and source-reader information architecture. The FACET HTML/CSS establishes Nudox geometry, palette, typography, bevel grammar, and motion tokens. The weak capture is useful for quantifying the current shell and content gaps, but its manifest has `reference:null` and its settled frame still says “Loading live package catalog…”. It cannot be used as a target screenshot.

## Evidence and coordinate rules

All coordinates use the top-left origin, CSS pixels at device scale 1, and half-open boxes `[x, y, x + width, y + height)`. A `raster-measured` value comes from a deterministic scan or fixed pixel sample. A `css-derived` value is copied from the source HTML/CSS or calculated from a fixed CSS dimension. An `inference` value is a proposed implementation constraint based on the evidence; it must be checked by the harness before being promoted to source evidence.

| evidence | absolute path | SHA-256 | dimensions | inspected crop |
| --- | --- | --- | --- | --- |
| crates.io package reference | `/Users/mileswirht/Downloads/backend/.artifacts/external-reference/crates-io-serde-cua-1280x720.jpg` | `f0114c0305ceb0ee36307e0c12ae00833a12d45227f80b2bf6b78a6a632a2ba5` | 1280x720 | `(0,0,1280,720)` |
| docs.rs package reference | `/Users/mileswirht/Downloads/backend/.artifacts/external-reference/docs-rs-serde-cua-1280x720.jpg` | `edd8a9d8d22f070b52bba6eaf723cf51df541b0c0988df286a4b92af832dcae2` | 1280x720 | `(0,0,1280,720)` |
| docs.rs source reference | `/Users/mileswirht/Downloads/backend/.artifacts/external-reference/docs-rs-serde-lib-rs-cua-1280x720.jpg` | `f7ab78f2e9964d958fce09a634608b548479a046b3d194b27f7488d1af64ffd0` | 1280x720 | `(0,0,1280,720)` |
| language artboard HTML/CSS | `/Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/the-information-language/Language.dc.html` | `803654307f626ca74eb12eb7ebd0f0ecb6842f1ddb56bb1ba257c6067da4ed4a` | root 1440x1760 | full source; PNG crop `(0,0,1440,1000)` |
| brand artboard HTML/CSS | `/Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/one-mark-one-language/Brand.dc.html` | `6db8497548d073fc36d07c899f1ec5f4a04433a1c0cbaf8465ab1228c409462b` | root 1440x1420 | full source; PNG crop `(0,0,1440,1000)` |
| colour artboard HTML/CSS | `/Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/colour-with-a-job/Color.dc.html` | `aea27a1d2cdb8f4182879868376e887af1a21802cf07129a3f95216dc8d2bc21` | root 1440x1340 | full source; PNG crop `(0,0,1440,1000)` |
| descent artboard HTML/CSS | `/Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/descent/Descent.dc.html` | `ed20ae6d351f9aa401705f63e6cee381af5371e7ba207ef05a8a3111ba8dc740` | root 1440x1160 | full source; PNG crop `(0,0,1440,1000)` |
| weak responsive delta | `/Users/mileswirht/Documents/ChatGPT/backend-responsive-shell-v8/.artifacts/gui-harness/responsive-1280-browse-live-weak/frames/browse/settled.png` | `2f3f1e99b5279e6c3d64962eb96eabe0de25c9be9259f6c0eeead81d2d044a1e` | 1280x800 | `(0,0,1280,800)` |

The four design PNGs are 1600x1000 exports with a 160 px white region to the right of the 1440 px artboard. The artboard crop is therefore part of the evidence definition. No derivative crops were added under `.artifacts/reference-component-spec-v8/`: `.artifacts/` is ignored/user-owned, and every inspected crop is reproducible from the coordinates above.

## Measured external component geometry

### Package dossier reference: crates.io

The crates raster uses a dark web surface and gives the target content structure. Its web colours are evidence for the information hierarchy, not replacements for the FACET abyss palette.

| component | measured visible box | measurement and required implication |
| --- | --- | --- |
| nav body | `(0,0,1280,64)` | dominant `#141412`; navigation occupies the first 64 rows |
| nav separator | `(0,64,1280,8)` | sampled rows: y64–65 `#151515`, y66 `#2e2e2e`, y67 `#333333`, y68 `#2e2e2e`, y69–71 `#2f2f2f`; preserve a visible chrome/body transition |
| body ground | `(0,72,1280,648)` | dominant `#303030`; body begins at y72 |
| package hero outer | `(177,84,926,150)` | measured outer edge; interior dark component begins at x183/y88 |
| package hero interior | `(183,88,914,145)` | dominant `#141412`; title, version, tagline, and chips live inside this inset |
| lens/tab region | `(177,248,926,44)` | label ink rows y265–279; active underline y290–291; one active lens is visible |
| article card, visible | `(177,309,649,411)` | card continues below the 720 px crop; do not infer a bottom edge from the viewport |
| article interior, visible | `(183,311,641,409)` | article heading rows y341–357 and y365–381; first body band y424–439 |
| metadata column, visible | `(851,309,252,411)` | metadata heading band y339–354; it is parallel to the article reader |

The dossier therefore has a clear sequence: package identity and version, short description/chips, lens tabs, then a reader and metadata column in parallel. A settled capture needs all of those regions. A centered loading well is not a substitute for them.

Fixed colour samples from the same raster are `(500,100) = #141412` in the hero, `(500,240) = #30302e` just below the hero, and `(500,300) = #303030` in the body. Text envelopes are recorded as row bands because JPEG anti-aliasing makes a glyph bounding box less stable than the component edges.

### Docs reader reference: docs.rs package page

The docs package raster separates browser chrome, navigation, and a readable measure.

| component | measured visible box | measurement and required implication |
| --- | --- | --- |
| top chrome | `(0,0,1280,32)` | dominant `#353535`; fixed 32 px chrome |
| sidebar | `(0,32,192,688)` | dominant `#505050`; the visible divider is x199 |
| lower sidebar gap | `(0,528,192,31)` | body background returns before the lower group at y559 |
| reader surface | `(200,32,1080,688)` | dominant `#353535`; content does not begin immediately at the divider |
| reader measure | `(269,182,936,538)` | derived from text/rule envelope; horizontal rules run roughly x269..1205 |
| title band | y74–93 | “Crate serde” title baseline envelope |
| toolbar/status band | y106–117 | icon/status controls remain above the article |
| body text | y209–344 | successive visible line bands are about 20 px apart |
| section heading | y370–388 | heading starts after a larger vertical rhythm step |

Pixel anchors are `(0,0) = #353535`, `(10,50) = #505050`, and `(200,50) = #353535`. The reader implementation should keep the readable column and rules aligned rather than filling the full viewport with text.

### Source reader reference: docs.rs `serde/lib.rs`

The source raster changes the navigation geometry while preserving the 32 px top chrome.

| component | measured visible box | measurement and required implication |
| --- | --- | --- |
| top chrome | `(0,0,1280,32)` | dominant `#353535` |
| source rail | `(0,32,49,688)` | dominant `#505050`; narrow source navigation |
| source divider | x49 | sampled around `#343434`; keep one visible separator |
| source reader | `(56,32,1224,688)` | reader starts after a 6 px post-divider inset |
| code surface | `(72,158,1208,562)` | dominant `#2a2a2a`; code begins after reader toolbar |
| code top transition | y152–158 | y152 `#343434`, y153 onward is predominantly `#2a2a2a` |
| code line envelope | first visible text y180–190 | subsequent lines step about 21 px |

The source crop’s line-number text begins around x104 and code text around x140. The FACET source reader has a corresponding fixed line-number gutter: `.code .ln` is 34 px wide with 12 px right padding and a -6 px left margin. Keep the gutter visually separate while allowing the code body to clip or scroll horizontally.

## FACET source contract

The four HTML files share the base tokens and the final v3 patch. The direct source anchors are `Language.dc.html:30–68` for palette, type, radii, and motion; `:96–190` for roles and controls; `:267–335` for shell; `:418–489` for document surfaces; `:561–612` and `:647–755` for v3 shell/type/plate overrides. The other artboards carry the same shared block. Their unique sections are:

| artboard | direct source geometry | derived content geometry |
| --- | --- | --- |
| Language | `.doc` padding 56px 64px; `.lsec` at lines 862+; first content row at 907 | inner frame x64..1376 (1312 px); first row has 64 px gap and flex widths 667.2 / 580.8 at x64 and x795.2 |
| Brand | `.bhero` at lines 862+; grid `330px 1fr`, gap 48 | mark column x64..394; motif column x442..1376 (934 px); motif grid is three columns with 34 px column gap and 40 px row gap |
| Colour | `.csec` at lines 862+; first row at 911 | Ground/Ink flex row has 48 px gap and widths 737.333 / 526.667; four voice panels use 26 px gaps and 308.5 px columns |
| Descent | `.rose` line 865; `.srcwrap` line 976; strip at 1025 | panel inner x92..1348 (1256 px); `.strip` is 1256 px wide in the 1440 crop and 640 px high; source grid is 64px / flexible / 270px with 22 px column gaps |

The common `.doc` inner frame is x64, y56, width1312. The design PNG crop ends at y1000, so the full source artboard height must come from the HTML root rather than the PNG viewport.

### Palette, radii, type, and baselines

The core palette is `#030814` g0, `#060d1b` g1, `#0a1424` g2, `#0f1b2f` g3, `#15243c` g4, `#1c304f` g5, and `#25406a` g6. Ink is `#f5f7fb`, `#d2d9e5`, `#9aa6ba`, `#74819a`, and `#4c5870`. Signals are mint `#6cebad`, teal `#3fcdc6`, leaf `#2fb96c`, periwinkle `#93a2fa`, amber `#f4bb6a`, and coral `#ff7a8a`. The shared radius ladder is 5, 8, 12, 16, and 22 px plus a pill radius. Lines are translucent periwinkle at 0.07, 0.12, and 0.22 alpha.

The base roles are:

| role | family | size/line | weight/tracking |
| --- | --- | --- | --- |
| display-xl | Archivo; v3 display override | 46/50 px | base 750, v3 640, v3 `-.035em` |
| display | Archivo; v3 display override | 30/36 px | base 700, v3 620, v3 `-.025em` |
| title | display | 20/26 px | 650, `-.01em` |
| head | UI | 15/22 px | 600 |
| body | UI | 13.5/21 px | normal |
| prose | UI; docs lede v3 uses Newsreader | 14.5/23 px; lede 19/28 px italic | normal |
| small | UI | 12/16 px | normal |
| micro | display; v3 UI | 10.5/14 px | 650, v3 `0.1em` |
| code | Geist Mono | 12.5/20 px | source line step 21 px |

Use the CSS line-height as the baseline grid. In raster review, accept text envelopes within ±2 px around the expected band and compare the baseline step rather than individual anti-aliased glyph pixels. The external reference bands are hero title y107–134, tagline y160–176, article headings y341–357/y365–381, docs title y74–93, docs body y209–344 at roughly 20 px steps, and source code y180–190 at roughly 21 px steps.

### Shell boxes, gutters, and hit areas

At a 1280x800 wide capture the v3 shell is derived as follows:

| region | box | direct rule |
| --- | --- | --- |
| titlebar v2 | `(0,0,1280,50)` | `.titlebar.v2 { height:50px; padding:0 12px 0 14px }` |
| body | `(0,50,1280,724)` | remaining height before status |
| rail | `(0,50,54,724)` | `.rail { width:54px; padding:10px 0; gap:4px }` |
| shelf | `(54,50,264,724)` | `.shelf { width:264px }` |
| reader | `(318,50,662,724)` | flexible remainder after fixed columns |
| context | `(980,50,300,724)` | `.ctx { width:300px }` |
| status | `(0,774,1280,26)` | `.status { height:26px; padding:0 12px; gap:14px }` |

The rail button hit area is 38x38 px with an 11 px radius. Compact icon buttons are 28x28 px (22x22 px small), keyboard caps are at least 18x18 px, chips are 22 px high, and the primary button is 30 px high. The focus-visible channel is a 2 px periwinkle outline with 2 px offset and a 5 px halo. Do not hide the focus edge in a crop.

The reader’s base measure is `max-width:960px`, centered, with 28 px top, 40 px side, and 60 px bottom padding and a 24 px vertical gap. The wide measure caps at 1180 px. The folio caps at 1080 px with 26/36/60 px padding and a 22 px gap. The source reader’s `.srcwrap` is a 64 px spine column, a flexible code column, and a 270 px note column, with 22 px column gaps and 18/30/40/18 px padding.

## State grammar and component acceptance

The capture harness should evaluate component crops, masks, edge samples, semantic region markers, and timing metadata. It should not require whole-image exact equality. A component passes only when its own geometry, surface probes, text envelope, and state fixture pass. Anti-aliasing is allowed a channel tolerance of 3 only where the surface is composited; opaque CSS fills use exact RGB probes.

### Shell and navigation

The shell gate measures the titlebar, rail, shelf, reader, context, and status boundaries. At 1280 px the required heights/widths are titlebar 50±1, rail 54±1, shelf 264±1, context 300±1, and status 26±1. The fixed boundary scan must find the same four vertical region transitions. A capture that uses the weak 280 px shelf fails the wide shell gate even though its rail and status happen to match.

The rail state matrix uses five fixtures:

| state | expected delta | screenshot checks |
| --- | --- | --- |
| rest | none | 38x38 hit area, 4 px gap, ink3 icon, no marker |
| hover | `translateY(-1px)` only for controls that lift; rail button itself gains line1 plate | geometry stays fixed; line1 and ink0 are visible |
| active | no positional change | mint icon, signal-soft plate, signal-line edge, 3x12 px left marker |
| focus-visible | no positional change | 2 px peri edge plus 5 px halo |
| pressed | `scale(.94)` | the pressed crop shows the scale; release returns to rest box |

Shelf rows are at least 28 px high, tall rows at least 44 px, and nested tree children use 14 px margin plus 7 px left padding. Active rows use a 2.5 px mint marker; current rows use peri-soft with a 1 px peri-line inset. Flexible labels must ellipsize instead of pushing the reader.

### Plates, controls, and bevels

The v3 plate is a flat `plate2` surface with one lit top-left edge and one shaded bottom-right edge. Default cuts use a 14 px chamfer; small/large cuts use 9/22 px, buttons use the 8 px `cutb` chamfer, `here` uses 10 px, tooltips 5 px, popups 7 px, nodules 6 px, and mosaic cells 3 px. State is communicated through bevel hue/thickness and the edge animation; content does not move unless a lift class explicitly moves it.

For every button and compact control, capture rest, hover, pressed, focus-visible, disabled, and busy/working where applicable. The direct deltas are:

- hover: plate2 to plate3, `translateY(-1px)`, 160 ms background/edge transition; the button sheen crosses over 380 ms;
- pressed: `translateY(1px) scale(.97)` for v3 buttons; icon buttons use `scale(.92)`;
- focus-visible: hard 2 px periwinkle inset, with no layout change;
- disabled: opacity `.42`, saturation `.4`, pointer events off;
- busy: label becomes visually hidden, the hatch runs inside the fixed button box;
- danger/amber/coral: the edge changes hue while the label remains in its declared role type.

The gate samples the four corners, top-left and bottom-right edge pixels, one interior fill, and the control’s connected mask. It does not compare the surrounding reader.

### Package dossier, loading, error, and empty variants

The package dossier gate requires these semantic regions in this order: package identity/version, short description, metadata chips, lens tabs, primary README/article reader, metadata/links column, and an install/source affordance. The package description is clamped to two lines in a card context. Version/path/metadata labels ellipsize inside flex children and expose the full value in a tooltip or detail view. One and only one lens tab is active, with a 2 px mint underline and 10 px side inset.

The settled state must contain article and metadata content. A loading well is permitted only while the data state is loading and must be marked in capture metadata. Its hatch/reveal uses the 620 ms scene token. The loading fixture passes when the well remains within the dossier box and the underlying card geometry does not jump. It must fail the settled dossier gate if the loading label remains after the settled event.

The error fixture uses the fault surface: a 16 px radius, 22 px internal padding, coral or amber line, a clear title, a reason sentence, and a retry/resolve action. The empty fixture keeps the package identity shell, replaces the reader body with one explanatory sentence and a primary next action, and retains the tabs/metadata frame so the layout does not collapse. Both error and empty states must preserve the same package hero and column boundaries as the data state.

The per-crop checks are: hero visible and bounded; tabs row 36 px high; active underline present; article body present for data; metadata column present; no settled loading text; and no card edge displacement across loading/error/empty/data fixtures beyond ±1 px.

### Docs reader and source reader

The docs reader gate uses the external measured boxes as layout anchors: top chrome 32 px; sidebar 192 px with the visible divider at x199; reader text starts around x269; horizontal rules terminate around x1205. The title band is y74–93, toolbar/status y106–117, body lines begin near y209 and step about 20 px, and the section heading band is y370–388.

The source reader gate uses top chrome 32 px, a 49 px rail and divider x49, reader x56, code surface x72, and code background `#2a2a2a` from about y158. The first code text band is y180–190 and later lines step about 21 px. The source crop must show a distinct line-number gutter around x104 and code text around x140. A fixed gutter must not move when long code lines clip or scroll.

For docs/source states, test rest, selected navigation item, hover, focus-visible, collapsed navigation, loading source, unavailable source, and empty search/results. Selected navigation uses the active signal or periwinkle edge without changing the reader x coordinate. Loading source can use the hatch/reveal; unavailable source uses an amber/coral message with retry; empty search leaves the reader chrome and gives one explanatory action. The source code surface and gutters remain present in all three variants.

## Truncation, vertical rhythm, and text acceptance

The implementation must set `min-width:0` on every flexible label container. A single-line label uses `white-space:nowrap`, `overflow:hidden`, and `text-overflow:ellipsis`. Package descriptions clamp to two lines. Source code may clip or scroll horizontally, but its line-number gutter remains fixed. The harness should render a long and a short fixture at every responsive width and detect both the ellipsis/line clamp and the absence of sibling overflow.

Text acceptance compares declared role styles and row bands. The CSS roles are 46/50, 30/36, 20/26, 15/22, 13.5/21, 14.5/23, 12/16, and 10.5/14 px. The source line step is 21 px. Raster text masks may vary by up to 2 px around glyph edges, while baseline steps and container boundaries may vary by only 1 px. This gives the harness a stable baseline test without requiring font-raster identity.

## Narrow and collapsed transformations

The HTML/CSS source does not define breakpoint media rules, so these transformations are explicit inference gates for the responsive implementation. The capture manifest should record which panes are collapsed or overlaid.

| width | required transformation |
| ---: | --- |
| 1280 | full titlebar, 54 px rail, 264 px shelf, flexible reader, 300 px context; reader and metadata parallel |
| 620 | hide context first; retain shelf only while the reader has at least 300 px; otherwise make shelf an overlay; keep tabs one row or horizontally scrollable |
| 380 | collapse shelf to rail/overlay; context hidden; reader fills body; metadata moves below the reader or into a sheet |
| 240 | icon-first titlebar and rail; shelf/context overlays closed by default; keep a 28 px primary hit area; ellipsize title/path |
| 160 | single-column reader with optional panes closed; retain identity and active lens in the first frame; no horizontal overflow |
| 90 | icon/status surface with details in an opened sheet; record collapsed state explicitly; never paint content outside the viewport |

At every width, the shell must have a visible active route, a reachable package identity, and no fixed 264/300 px panes left in flow once they would make the reader unreadable.

## Motion contract

The named durations are source tokens and are part of screenshot review. The harness should capture event timestamps or deterministic frame progress and allow at most ±16 ms at a 60 fps cadence.

| token | duration | expected use |
| --- | ---: | --- |
| `--t-micro` | 90 ms | hover lift, active scale, row background |
| `--t-quick` | 160 ms | tooltip, focus halo, control color/background, tab color |
| `--t-std` | 240 ms | switch knob, cut lift, pop/unfold detail |
| `--t-emph` | 380 ms | rise/drop-in, staggered scene item, button sheen |
| `--t-scene` | 620 ms | loading resolve/reveal and scene transition |

Use glide `cubic-bezier(.22,1,.36,1)` for ordinary movement, snap `cubic-bezier(.3,0,0,1)` for micro state changes, and bounce `cubic-bezier(.34,1.56,.64,1)` in the v3 patch where a control calls for it. `prefers-reduced-motion: reduce` changes animation and transition durations to 1 ms and limits animation iteration to one.

## Weak capture deltas

The weak capture is 1280x800 and uses the FACET abyss colours, but the visible shell and content do not meet this specification. Its measured regions are: titlebar `(0,0,1280,46)`, rail `(0,46,54,728)`, side `(54,46,280,728)`, reader `(334,46,646,728)`, context `(981,46,299,728)`, status border y774, status `(0,775,1280,25)`, package card `(362,74,590,236)`, and loading well `(391,207,532,73)`.

Against the v3 CSS-derived wide shell, the titlebar is 4 px short, the side is 16 px too wide, the reader begins 16 px too far right, and the context is one pixel left/one pixel narrow with the titlebar offset moving its y origin by 4 px. Rail and status heights happen to match. The larger failure is semantic: the package card contains a loading placeholder instead of identity, tabs, article, metadata, and install/source content. Its manifest reports `reference:null`, `adapter-required`, and no target reference. These measurements are diagnostic deltas only.

The weak palette samples are `#060d1b` for titlebar/side/context and `#030814` for the reader; the external docs references are `#353535`, `#505050`, and `#2a2a2a`. That difference is expected when comparing separate visual systems. The target gate should use the FACET palette for Nudox surfaces and the external rasters for structure and reader evidence.

## Review execution

The review artifact is documentation and JSON only. No generated crops, source artboards, or user-owned media were modified. The only permitted repository validation for this lane is `git diff --check`; no Cargo, Nix, rustc, or build command belongs in this review.
