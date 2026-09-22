# Nudox design adherence review v8

This is an adversarial audit against the four HTML references in `/Users/mileswirht/Downloads/backend/Nudox-Design-System`. The authoritative source is the inline CSS and artboard markup; the HTML exports were not edited. I opened each full artboard and focused crops for the header, identity mark, voice/state plates, family/kind language, descent steps, Glacier board, and comb/mosaic examples.

The native GPUI application was not built or run because the parent task withheld a build lane. The screenshot paths in this report therefore point to inspected reference pixels only. Any native pixel claim is explicitly marked unverified in the JSON ledger.

The machine-readable ledger is [design-adherence-v8.json](</Users/mileswirht/Documents/ChatGPT/backend-design-adherence-review-v8/docs/reviews/design-adherence-v8.json>). The inspected reference images are under `/tmp/nudox-design-audit-v8` and include the four full artboards plus fourteen focused crops.

## Reference contract

All four references are 1440px wide and use a natural CSS artboard rather than a responsive production viewport:

| Reference | Artboard | SHA-256 | Pixel evidence |
| --- | ---: | --- | --- |
| Colour with a job | 1440×1340 | `aea27a1d…8d2bc21` | `color-full.png`, header/voices/families/daylight crops |
| Descent | 1440×1160 | `ed20ae6d…8dc740` | `descent-full.png`, hero/steps/footer crops |
| One mark, one language | 1440×1420 | `6db84975…09462b` | `brand-full.png`, hero/marks/motion crops |
| The information language | 1440×1760 | `80365430…7da4ed4a` | `language-full.png`, hero/kinds/states/comb crops |

The ordered source digest is `dc75eeb63abeae491e55c1a81991221c325fac4b74ee818ea637834f88169f64`. The shared authoritative CSS is [Color.dc.html](</Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/colour-with-a-job/Color.dc.html:30>) and its shell rules begin at [Color.dc.html](</Users/mileswirht/Downloads/backend/Nudox-Design-System/artifacts/colour-with-a-job/Color.dc.html:267>).

The reference establishes these non-negotiable values:

- Abyss ground `#030814` through `#25406a`, Glacier ground `#dfe5ee` through `#cfdaea`, silver ink steps, four voice colors, and five family colors.
- Archivo display at 46/50, 30/36, and 20/26; Instrument Sans at 15/22 and 13.5/21; Newsreader prose at 14.5/23; Geist Mono for specimens.
- 5/8/12/16/22px radii, 54px rail, 38px rail buttons, 46px titlebar, 32px inputs, 30px omnibar, 14px chamfer, and 24/30/38/46px button heights.
- 90/160/240/380/620ms motion, the glide/snap/spring curves, and 1ms reduced-motion transitions/animations.
- Hue identifies family, shape identifies kind. Mint acts, periwinkle focuses, amber waits, coral stops. A bevel/edge carries state.

## Requirement ledger

| ID | Requirement | Current result | Evidence or gap |
| --- | --- | --- | --- |
| REF-001 | Four exact references, hashes, artboards | **Pass, static** | Contract extractor enumerates the same four artifacts; hashes and artboards are recorded in JSON. |
| TOK-001 | Abyss/Glacier palette and semantic voices | **Pass, static** | `apps/desktop/src/theme/palette.rs:76-326` matches the reference hex values. Glacier parity is pixel-inspected in `color-full-daylight.png`. |
| TOK-002 | Exact font families and type metrics | **Partial** | Embedded fonts and hashes match. The product ladder has measurable line metric differences: current 30/32 vs reference 30/36, current 14/24 vs reference 13.5/21, current 10/14 vs reference 10.5/14. |
| GEO-001 | Facet titlebar/rail/chrome geometry | **Gap** | Reference titlebar is 46px; current header is hardcoded 64px (+18px, +39.13%). Current shelf is 248px while the reference uses a 54px rail plus 280px side pane. Current content padding is 40px. |
| GEO-002 | Responsive rail/panel transformations | **Gap** | `ResponsiveLayout::resolve` exists at `apps/desktop/src/core/layout.rs:57-107`, but `render_root` never calls it. Shelf/context open flags are persisted but not projected into layout. |
| SUR-001 | Chamfer, bevel, weave, and edge state | **Gap** | Reference `.cut` uses a 14px polygon and bevel/weave layers. `surface::cut` only paints a rectangle with two 18px hairline children. |
| BRAND-001 | Mark-derived identity jobs | **Gap** | Brand crops show the mark, stone, stripes, chevron, bevel, comb, and cut corner. The product shell has only a text “Nudox” header and SVG asset helpers. |
| LANG-001 | Family hue plus kind shape | **Partial** | Palette and kind mapping exist, but the complete family/kind atlas and comb/mosaic language are not composed in route views. |
| STATE-001 | Four voice controls and seven edge states | **Partial** | ControlState is closed and semantic, but route surfaces remain generic rectangular CE controls; the reference’s edge-coded state plates are absent. |
| MOTION-001 | Shared timing and reduced motion | **Pass, static** | Product motion enum matches all five reference durations and snaps under reduced motion. |
| MOTION-002 | Visible retargetable motion | **Gap** | `UiRootEntity` advances a timeline, but inspected views do not read a track into route, panel, overlay, hover, or state geometry. Native motion pixels are unobserved. |
| HARNESS-001 | Resize/scale after first frame | **Patched, unverified** | The driver now tracks `current_viewport`; each `CaptureRecord` and `FrameArtifact` records its viewport; writers and conformance validate each frame’s physical size. A headless resize regression was added but cannot run without the build lane. |
| HARNESS-002 | Animation pixel gate with resize awareness | **Patched, unverified** | Reduced-motion inspection ignores dimension-only changes; full-motion mixed-size transitions remain measurable. Filmstrip and frame PNGs stay independently inspectable. |
| HARNESS-003 | Full capture matrix | **Gap** | The contract requires viewports, scales, themes, motion, states, routes, overlays, and keyboard evidence, but the production CLI captures one state and viewport per invocation. |
| A11Y-001 | Focus, labels, roles, order, and state evidence | **Pass static, pixels pending** | CE controls set labels, tab indexes, focus rings, and roles; ActionTree records stable metadata. Crops and native AX evidence still need a build-lane run. |
| RENDER-001 | GPUI-only visual construction | **Partial** | No canvas API was found. The current closed icon system is SVG-backed for chrome, logos, kinds, and faceted ground, which conflicts with the requested “typically without SVG” direction for routine geometry. |
| STRESS-001 | Large text, collapse, density, long labels, all states | **Partial** | Tokens expose 80–150% interface size and compact/comfortable/spacious density, but the responsive layout is not wired and no native stress captures exist. |

## Worst discrepancies

1. **Harness geometry was structurally unsafe for resize journeys.** Post-first-frame `Resize` and `Scale` were rejected, and all artifact/conformance geometry assumed one manifest viewport. The committed patch makes viewport provenance frame-local and adds a regression path for a 64×32 → 32×16 resize.
2. **The main shell misses the reference geometry.** The authoritative titlebar is 46px, while the current header is 64px. The current shelf is fixed at 248px and the content pad is 40px; reference chrome is rail-first and uses a 280px side pane, 300px context pane, and 28/40px reading measure rules.
3. **The responsive resolver is disconnected.** Four width classes and panel modes are implemented as data, but `render_root` always emits one shelf and one body. This makes narrow/collapsed behavior a source-level gap even before pixels are captured.
4. **The Facet surface grammar is missing from shared primitives.** The reference’s 14px chamfer, bevel edge, hatch/weave, and state edge are the dominant visual signal. `surface::cut` currently draws two accent hairlines on a rectangular panel.
5. **Motion tokens exist without visible product bindings.** Timing matches the reference, but the timeline’s values do not drive inspected view geometry, so a manifest could have motion metadata without route/panel/overlay pixels changing.

## Harness change in this commit

The low-conflict harness fix is intentionally scoped to capture provenance and verification:

- `CaptureRecord.viewport` is now required for every in-memory frame.
- `FrameArtifact.viewport` is optional on disk for backward compatibility; old manifests fall back to the manifest viewport, new captures serialize the exact per-frame viewport.
- The GPUI driver updates viewport state after every resize or scale action, normalizes the image against that state, and records it.
- Artifact verification and conformance validate each frame against its own physical dimensions.
- Reduced-motion animation checks ignore dimension-only transitions caused by a resize.
- A headless resize regression covers frame geometry before and after a scheduled resize.

`git diff --check` passed. No compiler, Cargo, Nix, or native capture was run under the parent’s resource ceiling. The patch must be built and exercised before it is considered runtime-verified.

## Required next capture pass

When a build lane opens, capture each route at 1440×900, 1024×768, and 640×480 at 1× and 2× in Abyss and Glacier, both motion modes, and all interaction states. Add a resize sequence with frames before/after each change and inspect the exact PNGs and crops. Stress 150% interface size, spacious density, long labels, collapsed shelf/context, loading/stale/offline/error, focus-visible, tab/shift-tab, Enter/Escape, and overlay restoration. Do not promote a manifest without inspecting its pixels.
