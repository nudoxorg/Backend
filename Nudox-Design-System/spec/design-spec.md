# Nudox FACET v3 — GPUI Design-Token Extraction

Source: `/Users/mileswirht/Downloads/backend/Nudox-Design-System/boards/*.dc.html` (38 boards). Every board inlines **one identical 847-line shared `<style>` block** (verified byte-identical across all boards: lines 1–847 of the `<style>` contents are the same in every `.dc.html`; per-board CSS starts at line 848) followed by board-only CSS. The shared block itself is layered history, all present in the cascade of every board:

- **v1 "FACET"** (lines 1–506): base tokens, gradient-painted surfaces, `--display:Archivo`, `--ui:"Instrument Sans"`.
- **v2 "DEPTH"** (lines 507–605): adds `--bevel-hi/lo/--deep`, the chamfered `.cut` plate, titlebar-as-trail, shelf, floating dock.
- **v3 "CUT, NOT PAINTED"** (lines 606–847, wins the cascade): flat plates, hard two-tone bevel as the *only* state channel, `--display:Bricolage Grotesque`, `--ui:Geist`, adds `--serif:Newsreader`, icons-not-labels, gem/comb/mosaic/strand/seam texture system. **This is the live, rendered design language** — v1/v2 rules are still parsed but almost every visual property they set is re-declared by v3 on the same selectors (same specificity, later in source ⇒ wins).

Companion file: `R5-icons.json` — 111 entries `{group, name, viewBox, class, svg}` covering kind/modifier/capability/ui/language marks, the chevron, the Nudox logo, and the gem template.

---

## 1. Colour tokens (cascade-resolved)

All values below are the **effective** (final-cascade) value. Where v3 overrides v1/v2 on the same custom property, only the winning value is listed, with the superseded one noted as *(dead)*.

### 1.1 Ground / plates (Abyss dark, default theme `.nx`)

| Token | Dark value | Glacier (`.nx.glacier`) value | Role |
|---|---|---|---|
| `--g0` | `#030814` | `#dfe5ee` | behind the window |
| `--g1` | `#060d1b` | `#eef2f7` | window ground / `.win` background (v3: solid, no gradient) |
| `--g2` | `#0a1424` | `#f6f8fb` | ring track base |
| `--g3` | `#0f1b2f` | `#ffffff` | graph node fill |
| `--g4` | `#15243c` | `#eef2f8` | tooltip/menu chrome fallback |
| `--g5` | `#1c304f` | `#e2e9f3` | tracks (slider/switch) |
| `--g6` | `#25406a` | `#cfdaea` | pressed — **defined, never referenced** (dead token) |
| `--line1` | `rgba(158,176,255,.07)` | `rgba(30,50,110,.08)` | hairline (dividers) |
| `--line2` | `rgba(158,176,255,.12)` | `rgba(30,50,110,.14)` | control outline |
| `--line3` | `rgba(158,176,255,.22)` | `rgba(30,50,110,.26)` | hover/active outline |
| `--plate` (v3) | `#0b1526` | `#ffffff` | every cut plate's flat fill |
| `--plate2` (v3) | `#101d33` | `#f3f6fb` | controls, nodules, "two"-tone header |
| `--plate3` (v3) | `#16273f` | `#e9eef6` | hover/pressed plate, tooltip/menu bg |
| `--table` (v3) | `#050b17` | `#eef2f8` (theme override) / `#ffffff` (glacier block) | gem's recessed centre; `.cut.deep::before` target |

### 1.2 Ink (text)

| Token | Dark | Glacier | Role |
|---|---|---|---|
| `--ink0` | `#f5f7fb` | `#0a1222` | names, yours (highest ownership) |
| `--ink1` | `#d2d9e5` | `#1d2940` | body, rows |
| `--ink2` | `#9aa6ba` | `#4b5a75` | secondary |
| `--ink3` | `#74819a` | `#66758f` | not-yours, captions |
| `--ink4` | `#4c5870` | `#a3aec2` | rules, closed doors |

### 1.3 Signal voices ("mint acts, periwinkle focuses, amber waits, coral stops")

| Token | Dark | Glacier |
|---|---|---|
| `--mint` | `#6cebad` | `#0f9d6a` |
| `--teal` | `#3fcdc6` | `#0d8f8f` |
| `--leaf` | `#2fb96c` | `#0c8a4e` |
| `--mint-ink` | `#03231a` | `#ffffff` |
| `--signal` (gradient, v1) | `linear-gradient(180deg,#86f3bd 0%,#4bd9ac 52%,#36c4b6 100%)` | `linear-gradient(180deg,#24b884 0%,#139c78 55%,#0e8c80 100%)` |
| `--signal` (v3 override, **wins**) | `var(--mint)` flat | `var(--mint)` flat |
| `--signal-soft` | `rgba(98,230,166,.12)` | `rgba(15,157,106,.1)` |
| `--signal-line` | `rgba(98,230,166,.34)` | `rgba(15,157,106,.36)` |
| `--peri` | `#93a2fa` | `#4b5bd6` |
| `--peri-hi` | `#bcc6ff` | `#3443b8` |
| `--peri-soft` | `rgba(147,162,250,.13)` | `rgba(75,91,214,.1)` |
| `--peri-line` | `rgba(147,162,250,.4)` | `rgba(75,91,214,.4)` |
| `--amber` | `#f4bb6a` | `#a8650a` |
| `--amber-soft` | `rgba(244,187,106,.13)` | `rgba(168,101,10,.1)` |
| `--amber-line` | `rgba(244,187,106,.38)` | `rgba(168,101,10,.36)` |
| `--coral` | `#ff7a8a` | `#c8324a` |
| `--coral-soft` | `rgba(255,122,138,.13)` | `rgba(200,50,74,.09)` |
| `--coral-line` | `rgba(255,122,138,.4)` | `rgba(200,50,74,.36)` |

Never a dot/pip anywhere in v3 — the voice speaks through the **bevel** first, a fill second (`Color.dc.html`, "Four voices" section). "One mint action per view" is a house rule (over-use of mint is flagged as a design error in `Color.dc.html`'s "How a screen is spent").

### 1.4 Family hues ("one lightness, five hues" — hue=family, shape=kind)

| Token | Dark | Glacier | Family (from `Color.dc.html`) |
|---|---|---|---|
| `--f-ns` / `--f-ns-bg` | `#a9b6cc` / `rgba(169,182,204,.12)` | `#55647e` / `rgba(85,100,126,.1)` | Namespaces |
| `--f-type` / `--f-type-bg` | `#5fe0b4` / `rgba(95,224,180,.13)` | `#0b8f68` / `rgba(11,143,104,.1)` | Types |
| `--f-con` / `--f-con-bg` | `#d59cf5` / `rgba(213,156,245,.13)` | `#8b3fc0` / `rgba(139,63,192,.1)` | Contracts (trait/interface) |
| `--f-call` / `--f-call-bg` | `#8fa6ff` / `rgba(143,166,255,.14)` | `#3d52d0` / `rgba(61,82,208,.1)` | Callables |
| `--f-val` / `--f-val-bg` | `#f3c06e` / `rgba(243,192,110,.13)` | `#a2650c` / `rgba(162,101,12,.1)` | Values (the only warm hue, "so constants and fields pop out of a wall of types and functions") |

Screen colour budget, stated on `Color.dc.html` ("How a screen is spent"): **abyss 68% · silver(ink) 22%** captioned explicitly; two further unlabelled segments **5%** and **3%** shown as bare numbers in the same stacked bar (unlabelled in markup, contextually = signal voices + family hues, the two rationed accent layers).

### 1.5 Bevel / elevation (v2→v3)

| Token | v2 value | v3 value (**wins**) | Glacier v3 |
|---|---|---|---|
| `--bevel-hi` | `rgba(190,205,255,.22)` | `rgba(196,210,255,.26)` | `#fff` |
| `--bevel-lo` | `rgba(0,0,0,.55)` | `rgba(0,0,0,.6)` | `rgba(30,50,110,.22)` |
| `--deep` | `#02060f` | *(kept, but its sole consumer `.cut.deep::before{background:var(--deep)}` is itself overridden by the later v3 rule `.cut.deep::before{background:var(--table)}` — same selector, so* `--deep` *is referenced once per file yet the effect is dead; `--table` is what actually renders)* | `#e8edf5` (v2 value, likewise shadowed) |
| `--e1` | `0 1px 0 rgba(255,255,255,.04) inset,0 1px 2px rgba(0,0,0,.35)` | unchanged (not touched by v3) | `0 1px 2px rgba(20,40,90,.1)` |
| `--e2` | `0 1px 0 rgba(255,255,255,.06) inset,0 8px 24px -8px rgba(0,0,0,.55),0 2px 6px rgba(0,0,0,.3)` | unchanged | `0 8px 24px -8px rgba(20,40,90,.22),0 1px 3px rgba(20,40,90,.12)` |
| `--e3` | `0 1px 0 rgba(255,255,255,.08) inset,0 28px 70px -14px rgba(0,0,0,.72),0 4px 14px rgba(0,0,0,.4)` | unchanged | `0 28px 70px -14px rgba(20,40,90,.35),0 4px 14px rgba(20,40,90,.14)` |
| `--weave` | `repeating-linear-gradient(135deg,rgba(158,176,255,.028) 0 1px,transparent 1px 9px)` | unchanged | `repeating-linear-gradient(135deg,rgba(30,50,110,.03) 0 1px,transparent 1px 9px)` |
| `--shaft` | gradient (diagonal light streak) | **`none`** (v3 kills it) | `none` |
| `--atmo` | 2 radial gradients (window ambient glow) | **`none`** (v3 kills it; replaced by the `.ground` faceted-field SVG) | `none` |
| `--glass` | `linear-gradient(180deg,#1c2d4b,#0f1b30)` | `#132039` flat | `#fff` flat |
| `--card` | `linear-gradient(180deg,rgba(24,40,68,.55),rgba(14,25,44,.55))` | `#0b1526` flat (= `--plate`) | `#fff` flat |

### 1.6 Fills, radius, misc

`--well:rgba(2,7,17,.55)` `--pane:rgba(4,10,22,.35)` `--inset:rgba(0,0,0,.28)` `--tint:rgba(190,205,255,.07)` `--tint2:rgba(190,205,255,.13)` `--veil:rgba(3,8,20,.62)` `--hatch:repeating-linear-gradient(90deg,currentColor 0 1.5px,transparent 1.5px 5px)` (the universal "pending/inferred" texture).

Radius: `--r1:5px --r2:8px --r3:12px --r4:16px --r5:22px(unused) --pill:999px`. Window corner radius is a literal `12px` on `.win`, not tokenized.

### 1.7 Type family tokens (v1 → v3 override)

| Token | v1 value | v3 value (**wins, live**) |
|---|---|---|
| `--display` | `"Archivo","Avenir Next","Helvetica Neue",sans-serif` | `"Bricolage Grotesque","Avenir Next",sans-serif` |
| `--ui` | `"Instrument Sans","Avenir Next","Helvetica Neue",sans-serif` | `"Geist","Avenir Next","Helvetica Neue",sans-serif` |
| `--mono` | `"Geist Mono","SF Mono",Menlo,monospace` | unchanged (same in v1 and v3) |
| `--serif` | *(did not exist)* | `"Newsreader",Georgia,serif` — new in v3 |

**Archivo and Instrument Sans never render in any board** — they are shadowed on every page by the identical `.nx` v3 block later in the same stylesheet. Treat them as dead/historical only.

### 1.8 Motion tokens

`--t-micro:90ms --t-quick:160ms --t-std:240ms --t-emph:380ms --t-scene:620ms`
`--glide:cubic-bezier(.22,1,.36,1)` (v1, decelerate) `--snap:cubic-bezier(.3,0,0,1)` (v1, linear-ish snap) `--spring:cubic-bezier(.2,.9,.25,1.18)` (v1, overshoot) `--bounce:cubic-bezier(.34,1.56,.64,1)` (v3, stronger overshoot — the v3-era default for plate/button motion) `--drop:cubic-bezier(.5,0,.9,.6)` (v3, **defined, never referenced anywhere** — dead token, presumably reserved for a "drop" ease-in that never shipped).

### 1.9 Used-vs-defined-only audit

Of 93 custom properties defined in `.nx`/`.nx.glacier`, exactly **3 are never consumed via `var(--x)` anywhere in any of the 38 boards**: `--drop`, `--g6`, `--r5`. Everything else is referenced at least once (though `--deep` is referenced by a rule that is itself dead-overridden — see 1.5).

---

## 2. Typography

Four faces, one job each (`TypeSheet.dc.html` header). Font tokens: `--display` (Bricolage Grotesque), `--ui` (Geist), `--mono` (Geist Mono), `--serif` (Newsreader, *always italic*, never used upright).

### 2.1 Canonical type scale (class → font / weight / size / leading / tracking)

| Class | Family | Weight | Size | Line-height | Tracking | `font-stretch` | Notes |
|---|---|---|---|---|---|---|---|
| `.t-display-xl` | display | 640 | 46px (v1 46/50, v3 keeps size, changes weight/tracking) | 50px | `-.035em` | 100% (v3; v1 was 118%) | page hero titles ("Descent", "Colour with a job") |
| `.hero-name` | display | 640 | **44px** (v3 shrinks from v1's 40px) | 46px | `-.035em` | 92% | symbol name on a Page (`.leaf-row` hero) — spec sheet: "640 / 44px / -0.035em" |
| `.t-display` | display | 620 | 30px | 36px | `-.025em` | 100% (v3; v1 114%) | section heads, doc `<h2>` |
| `.t-title` | display | 650 | 20px | 26px | `-.01em` | 110% (untouched by v3) | panel titles |
| `.t-head` | ui | 600 | 15px | 22px | — | — | list/row heads |
| `.t-body` | ui (inherit) | 400 | 13.5px | 21px | — | — | default body |
| `.t-prose` | ui (inherit) | 400 | 14.5px | 23px | — | — | longer paragraphs, `text-wrap:pretty` |
| `.t-small` | ui (inherit) | 400 | 12px | 16px | — | — | meta text |
| `.t-micro` | v1: display 650/10.5px/uppercase/.09em; **v3 override**: **ui**, 600, 10.5px, `.1em`, no uppercase requirement re-stated but kept via v1 (uppercase persists since v3 doesn't unset `text-transform`) | | | | | | section eyebrows, "FACET NN" doc numbers |
| `.book .t` (shelf title) | v1: display 720/16px/-.005em; **v3**: display 640/17px/`-.02em`/stretch 95% | | | | | | package name in shelf header |
| `.serif` (generic italic) | serif | 400 | inherit | inherit | 0 | — | utility italic wrapper |
| `.doc-lede` | v1: ui inherit 15.5/25px; **v3**: **serif**, 19px/28px, italic | | | | | | documentation lede paragraph |
| `.mg` (margin note) | **serif**, italic, `13.5px !important` / `19px !important` | | | | | | margin annotations beside the reading column; `.mg b/.who/.mono/code` snap back to **ui**, non-italic; `.mg .mono/code` → **mono**, 11.5px |
| `.tag` | ui, 100% stretch, else: 9.5px/700/uppercase/.07em, height 16px | | | | | | |
| `.pkg .nm` | display, 110% stretch, 680, 16px | | | | | | package-card title |

### 2.2 UI (Geist) ramp — from `TypeSheet.dc.html` specimens

| Specimen text | weight/size | Role label |
|---|---|---|
| "Filter, or find any package" | 600 / 10.5px | field label |
| "Add package" | 500 / 12px | button |
| "3 depend on · 41,208 depend on it" | 400 / 13px | body |
| "Remove serde from backend?" | 500 / 15px | dialog title |

### 2.3 Code (Geist Mono)

12.5px / 400, **tabular figures on** (`font-feature-settings:"zero"` on `.code`/`.mono`), "the only font that ever spells an identifier." Paths and versions render as one mono run — no separator ever switches font (e.g. `RelationLabel::Typed`, `1.0.193`). `.code` line-height 20px, `tab-size:4`. Syntax palette (dark): keyword `#c5a3ff`, type `var(--f-type)`, function-name `var(--f-call)`, string `#f0c987`, number `#f39b8a`, comment `var(--ink3)` italic, punctuation `var(--ink3)`, lifetime/macro `#ff9ec4`, attribute `var(--ink2)`, param `var(--ink1)`, const `var(--f-val)`, trait `var(--f-con)`. Glacier overrides: keyword `#7b3fd1`, string `#9a5b00`, number `#b8432f`, lifetime/macro `#b8327a`.

### 2.4 Serif (Newsreader, always italic)

Roles demonstrated: **lede** ("A generic serialization and deserialization framework."), **caption** ("The readable label of one relation group."), **margin note** ("The doc follows the cursor..."), **quiet hint** ("hold ⌘ for keys"). Never set upright anywhere in the corpus.

### 2.5 Do/don't rules (verbatim from `TypeSheet.dc.html`)

1. **Do** mark a modifier with an icon + tooltip. **Don't** spell it as an uppercase label (`CONST` `ASYNC` `UNSAFE`).
2. **Do** set an identifier in mono (`RelationLabel`). **Don't** set it in the display font.
3. **Do** keep the display font ≥ **16px** ("the display font's floor"). **Don't** take it to 10px ("the grotesque goes thin and cramped").

---

## 3. Spacing / radius / geometry

### 3.1 The chamfer ("cut") — `.cut`

```
--c:14px (default); .cut.sm → --c:9px; .cut.lg → --c:22px
clip-path: polygon(var(--c) 0, 100% 0, 100% calc(100% - var(--c)), calc(100% - var(--c)) 100%, 0 100%, 0 var(--c))
```
One 45° chamfer top-left, one bottom-right (never all four corners). Inner plate (`.cut::before`) is inset 1px with a matching clip-path shrunk by `var(--c) - .5px`, so a 1px **lit rim** shows around the flat interior — that rim is `background:linear-gradient(to bottom right, var(--bevel-hi) 50%, var(--bevel-lo) 50%)` (v3: hard 50/50 split, not a gradient blend) on the outer `.cut`, with the flat `--plate` colour on `::before`. Button variant `.btn.cutb` uses an 8px chamfer.

**Bevel state table** (verbatim captions, `Plates.dc.html`):

| State | Class | Meaning | Mechanism |
|---|---|---|---|
| Rest | `.cut` | lit top-left, shaded bottom-right | `--bevel-hi`/`--bevel-lo` 50/50 split |
| Focus | `.cut.focus` / `:focus-visible` | "doubled, periwinkle" | bevel recolours to `--peri-hi`/`--peri`; `::before` inset grows `1px → 2px` (doubles the visible rim) |
| Yours | `.cut.hot` | mint — your code reaches this | 50/50 split `--mint` / `rgba(98,230,166,.25)` |
| Working | `.cut.run` | "light travels the edge" | `conic-gradient(from var(--a,0deg), var(--mint) 0 18%, var(--bevel-lo) 18% 100%)`, animated `--a` 0→360deg via `@property --a` + `bevel-run` 1.6s linear infinite |
| Waiting | `.cut.amber` | amber, still (never blinks) | 50/50 split `--amber` / `rgba(244,187,106,.25)` |
| Stopped | `.cut.coral` | coral, one reason | 50/50 split `--coral` / `rgba(255,122,138,.25)` |
| Pending | `.cut.ghost` | hatched — sampled, not sealed | `repeating-linear-gradient(135deg, var(--line3) 0 4px, transparent 4px 8px)` |
| Two | `.cut.two` | header tone over body tone | `::before` split `--plate2` 50% / `--plate` 50% |
| Deep | `.cut.deep` | recessed to table level | `::before` → `var(--table)` |
| Weave | `.cut.weave` | subtle diagonal hatch texture under plate colour | `::before` bg = `var(--weave), var(--plate)` |
| Lift (hover) | `.cut.lift:hover` / `.is-hover` | `transform:translate(-1px,-3px)` on `var(--t-std) var(--bounce)` |

`.underglow` (v2 leftover): a blurred ellipse glow behind a `.cut` plate since `clip-path` eats `box-shadow` — 22px blur, 28% opacity, colour via `--glow` (defaults `--mint`).

### 3.2 Buttons — `.btn`

Height 30px (sm 24px / lg 38px / xl 46px), padding `0 13px`, `border-radius:pill` **in v1**, but v3 doesn't chamfer the base `.btn` (pill stays unless `.btn.cutb` requests the 8px cut). v3 background: flat `--plate2`; two-tone inset bevel `inset 1px 1px 0 var(--bevel-hi), inset -1px -1px 0 var(--bevel-lo)`. **Facet-sweep** hover effect: a 35%-opacity white wedge (`clip-path:polygon(0 0,38% 0,14% 100%,0 100%)`) sits at `translateX(-45%)` at rest and slides to `translateX(250%)` over `var(--t-emph) var(--glide)` on hover — described in `Controls.dc.html` as "the facet sweep, live: three phases of one 2.6s loop, never frozen" (the demo loop is `sweepdemo` 2.4s linear infinite; production hover sweep is the one-shot 380ms glide above). States confirmed by caption: **rest → hover → press → focus → busy → disabled**. Press = `translateY(1px) scale(.97)`. Focus = doubled periwinkle inset (`inset 2px 2px 0 var(--peri-hi), inset -2px -2px 0 var(--peri)`). Busy = text hidden, an animated hatch bar (`hatch-run .5s linear infinite`) fills the button interior. Disabled = `opacity:.42; filter:saturate(.4)`.
Variants: `.primary` (mint fill), `.edge` (periwinkle fill), `.ghost` (transparent), `.danger` (coral-tinted).

### 3.3 Toggles — no pills, no pucks; one geometry: the diamond

- `.switch`: 32×18px track, 14×14px circular knob (an exception — the *knob* is still round; v3 prose says "no pills, no rounded pucks" applies to the checkbox/radio family, not the switch thumb itself, which is unchanged from v1) — actually per `Controls.dc.html` copy the diamond is the check/radio shape; slides along a track when on (`translateX(14px)`).
- `.check`: 16×16px, 5px radius, fills `--signal` when on; `.mixed` state = periwinkle fill.
- `.radio`: 16×16px circle, `on` = inset 5px solid mint ring.
- **Slider = a comb**: "the filled ticks are the value, the tall one is the thumb" (`Controls.dc.html`) — reuses `.comb` geometry (§3.6), not a classic track+thumb.

### 3.4 Inputs

`.input` 32px tall (sm 26px, xl 60px), `border-radius:var(--r2)=8px` (xl: 20px), inset ring `1px var(--line2)`. States: **rest → focus (periwinkle ring, `inset 0 0 0 1.5px var(--peri)` + 4px `--peri-soft` halo) → bad (same shape, coral) → ok (signal-line ring)**. Select trigger reuses `.input`; "open" state shown in `Controls.dc.html` distinctly from "rest" (dropdown open).

### 3.5 `.here` / `.alt` / `.float` / `.floatwrap`

- `.here` (titlebar "current place" capsule): height 34px, flex-basis 430px, `border-radius:pill` in v1/v2; **v3**: `border-radius:0`, 10px chamfer `clip-path:polygon(10px 0,100% 0,100% calc(100% - 10px),calc(100% - 10px) 100%,0 100%,0 10px)`, flat `--plate2` + bevel inset.
- `.alt` (altimeter, 4 depths: Orbit/Package/Page/Source): height 30px pill in v1; **v3**: pill chrome stripped (`background:none;box-shadow:none`), each `.on` item rendered as plain display-font text (620/13px/-.01em) with a small rotated-45° diamond (`.d`, 9×9px in `.on`, else 7×7px) instead of a pill highlight.
- `.float` / `.floatwrap`: floating glass capsule (used for the Ask palette, hover popovers, drop shadows). v3: `border-radius:0`, same 10px chamfer as `.here`; the **shadow migrates to the wrapper** (`.floatwrap{filter:drop-shadow(0 18px 30px rgba(0,0,0,.7))}`) because `clip-path` on the inner `.float` eats `box-shadow` — this is the stated reason in `Plates.dc.html`: "Floating things carry their own shadow."

### 3.6 Gem — the kind mark as a cut stone (12 flat facets)

`viewBox="0 0 48 48"`. Fixed geometry, identical for every kind/family — only `currentColor` (family hue) and the centred glyph change:

- Outer diamond stroke: `M24 2 46 24 24 46 2 24z` (`stroke-width:1`).
- Inner "table" stroke: `M24 10 38 24 24 38 10 24z` (`stroke-width:.7`, `stroke-opacity:.55`, class `.tb`, `fill:var(--table)`).
- **12 facet triangles** (`class="fc"`, `fill:currentColor`, `opacity:var(--t)`), constant `points` per index 0–11:
  `0:"24,2 35,13 24,10"` `1:"24,10 35,13 38,24"` `2:"35,13 46,24 38,24"` `3:"46,24 35,35 38,24"` `4:"38,24 35,35 24,38"` `5:"35,35 24,46 24,38"` `6:"24,46 13,35 24,38"` `7:"24,38 13,35 10,24"` `8:"13,35 2,24 10,24"` `9:"2,24 13,13 10,24"` `10:"10,24 13,13 24,10"` `11:"13,13 24,2 24,10"`.
- Fixed **top-left lighting map** observed identically on every gem instance sampled: `--t` per facet index 0…11 = `0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66` (brightest near the top-left edge, dimmest at bottom-right — deterministic, not random).
- Centred kind glyph: `<g transform="translate(17.4 17.4) scale(.55)" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">` containing that kind's 24×24 icon paths (reused verbatim from the `.k` kind-mark set).
- States (from `DataMarks.dc.html`, "the stone is progress"): `.gem.hollow` (0 of 12 lit, `.fc{opacity:.05}` via `.gem.todo`), partial (`4 of 12`), `.gem.working` (facets sweep clockwise, `facet-run 2.4s linear infinite`, staggered `animation-delay:calc(var(--i)*200ms)`), `12 of 12` (fully lit / complete), `.gem.stalled` (forces `color:amber`), `.gem.cracked` (forces `color:coral`), `.gem.glint` (idle sparkle, `facet-glint 5s ease-in-out infinite`, staggered by `--i*70ms`). "A gem does not sit beside a progress bar, the gem *is* the progress bar, facet by facet."

### 3.7 Tick comb — `.comb`

Row of 1.5px-wide ticks, default height `var(--ch,40px)`; a "major" tick (`.maj`) is 2px wide/opacity .95; a hatched tick (`.hat`) uses the repeating-gradient texture instead of a solid fill. Each tick's own height is `var(--h, 10px)` (set per-tick inline, e.g. release-size or file-symbol-count encoded as bar height). **Hover wave** (no JS — pure `:has()`/adjacent-sibling CSS): hovering tick *n* raises it and its 3 neighbours on each side by a decaying amount:

| Position relative to hovered tick | transform |
|---|---|
| hovered (n0) | `translateY(-9px) scaleY(1.5)`, opacity 1 |
| n±1 | `translateY(-6px) scaleY(1.3)`, opacity 1 |
| n±2 | `translateY(-3.5px) scaleY(1.16)`, opacity .9 |
| n±3 | `translateY(-1.5px) scaleY(1.06)` |

Popover (`.comb .pop`) appears 12px above the hovered tick, chamfered (7px), shows a bold mono headline + detail line; transition `opacity .14s, transform .22s var(--bounce)`. Vertical variant `.comb.v` mirrors the same 4-tier falloff horizontally (`translateX`/`scaleX`). `.comb.down` flips the popover below instead of above.

### 3.8 Mosaic — `.mosaic`

11×11px stones, 3px gap, each an 11×11 mini-chamfer (`clip-path:polygon(3px 0,100% 0,100% calc(100% - 3px),calc(100% - 3px) 100%,0 100%,0 3px)`), base opacity .3, filled states: `.y` (public, opacity 1), `.new` (mint fill), `.gone` (coral diagonal hatch), `.gate` (feature-gated, own-colour horizontal hatch). Hover: the touched stone `translateY(-3px) scale(1.55)` opacity 1 (`z-index:5`); its immediate neighbour lifts `-1.5px` (`:has(+.st:hover)`). `.mosaic.dimmed` fades every stone not `.lit` to opacity .1 (the "ask a question of a module" effect in `DataMarks.dc.html`).

### 3.9 Seam — `.seam`

A 2px-tall flex row of segments (`flex:var(--w,1)`), states `.done` (mint), `.now` (soft mint fill + animated hatch overlay, `seam-run .5s linear infinite`), `.stall` (amber hatch), `.bad` (coral), default/`.todo` = `var(--line2)`. Used as the "staged bar under the thing being worked on" (index pipeline stages Fetch/Unpack/Parse/Resolve/Seal in `Onboarding.dc.html`'s footer).

### 3.10 Strands / nodule — the Rose relation graph

`.strands` = absolute full-bleed SVG `<path>` overlay, `stroke-width:1.4`, base `var(--ink4)`; `.w` (written, solid, opacity .75), `.via` (arrives through blanket/auto impl, `stroke-dasharray:2 4`), `.flow` (value in flight, dashed **and animated**, `stroke-dasharray:3 9`, `flow 1.4s linear infinite` moving `stroke-dashoffset` to `-24`). `.nodule` = a small mono-label chip (24px tall, 6px chamfer) placed at absolute `left/top` and `translate(-50%,-50%)`-centred on the strand endpoint; `.via` and `.quiet` nodule variants drop the plate entirely (outline-dashed or bare text). Main-board-specific `.rose` container: `height:318px`, centre `.hub` at `left:50%;top:160px`, a `.hubring` (64×64px, 1px `--f-type` ring) pulses `ripple 3.6s var(--glide) infinite` (rotate 45° scale .4→3.2, fade out), `.axis` labels are italic serif 13px positioned around the hub. Keyboard board's `.krose` (up/down/left/right relation compass) reuses the same idea at `height:322px, max-width:660px` with `.karm` (absolutely positioned arm) up/dn/lf/rt.

### 3.11 Capability marks — `.cap`

30×30px hit target, 18×18px icon, colour `--ink4` at rest → `--f-con` when `.on`; `.via` (arrives through blanket, same colour, dashed stroke `stroke-dasharray:2.2 2.2`). Hover: `translateY(-3px) scale(1.15)`; adjacent sibling lifts `-1.5px` (same `:has()` neighbour-wave idiom as comb/mosaic).

### 3.12 Window / shell metrics

| Region | Size | Notes |
|---|---|---|
| `.win` | fills its `.nx` canvas | `border-radius:12px` (untokenized literal); v3 background flattens to `var(--g1)` + `.ground` SVG overlay |
| `.titlebar` (v1) | 46px tall | `padding:0 12px 0 14px` |
| `.titlebar.v2` (live, "the titlebar is the trail") | 50px tall | `padding:0 12px 0 14px`; v3 strips its gradient background, leaves a 1px bottom hairline only |
| `.rail` (icon dock, when present) | 54px wide | `.rail-btn` 38×38px, 11px radius |
| `.side` | 280px wide | generic secondary panel |
| `.ctx` | 300px wide | generic context/inspector panel |
| `.shelf` (v2 package tree nav) | 264px wide | header `.book` 16/17px title; `.shelf .li` min-height 27px |
| `.kspine` (Collapse board's collapsed shelf) | 42px wide | icon-only spine of kind marks |
| `.pane-head` | 40px tall | `.pane-scroll` padding `2px 8px 12px` |
| `.status` (footer) | 26px tall | 14px gap between items, 11px text |
| `.omni` (titlebar search, Orbit) | flex-basis 560px, 30px tall | pill, `--tint` fill |
| `.measure` (doc reading column) | max-width 960px (`.wide` 1180px) | padding `28px 40px 60px`, gap 24px |
| `.folio` (app reading column) | max-width 1080px (per-page overrides, see §7) | padding `26px 36px 60px`, gap 22px (per-page overrides) |
| `.leaf-row` (reading col + margin) | grid `minmax(0,1fr) 250px` | gap `0 34px` → **margin column = 250px, gutter = 34px** |
| `.doc` (documentation-board canvas) | fills `.nx` | padding `56px 64px`, gap 36px |
| `.split` (resizer) | 9px hit zone, 1px hairline that grows to 3px + periwinkle glow on hover/drag; grip `5×28px` | `.split.h` is the horizontal(row-resize) mirror, 9px tall |
| `.dock` (bottom app-switcher strip in shelf) | items 32×32px, 9px radius | hover: `translateY(-3px) scale(1.1)` |

**Responsive breakpoints** (verbatim rules, `Resize.dc.html`): `< 1100px` → margin note column folds under its paragraph; `< 900px` → shelf collapses to a 42px `.kspine` icon spine; `< 640px` → the spine itself is replaced by an on-request overlay; at `480px` (narrowest) → the rose relation compass becomes a vertical list and the titlebar thread drops every bead except "here." Demo frames shown at scale 0.32: 1440×900 ("full width"), 1100×900 ("margin folds"), 900-equivalent frame ("shelf becomes a spine," rendered at 760px doc width), 480×— ("narrowest").

### 3.13 Faceted ground (`.ground`)

Absolute full-bleed background SVG (`viewBox="0 0 1440 900"`, `preserveAspectRatio="xMidYMid slice"`), filled with **procedurally-triangulated polygons** (a Delaunay-like low-poly field), each `<polygon>` carrying inline `--o` (opacity, ranging roughly 0.006–0.09) and `--d` (negative animation-delay, roughly -0.2s to -6.6s) custom properties; fill is a flat `#9eb0ff` (dark) / `#1e326e` (glacier). A minority are tagged `.tw` (twinkle-eligible) and a rarer subset `.tw.mintf` (twinkles in mint instead of periwinkle). `@keyframes twinkle` (7s ease-in-out infinite) multiplies the base opacity ×3.2 at the 50% mark. This is the entire "atmosphere" in v3 — `--atmo`/`--shaft` gradients are turned off (see §1.5); depth comes only from this triangulated starfield plus the plate bevels.

---

## 4. Motion

### 4.1 Easing tokens

`--glide:cubic-bezier(.22,1,.36,1)` `--snap:cubic-bezier(.3,0,0,1)` `--spring:cubic-bezier(.2,.9,.25,1.18)` `--bounce:cubic-bezier(.34,1.56,.64,1)` `--drop:cubic-bezier(.5,0,.9,.6)` (defined, unused). Durations `--t-micro:90ms --t-quick:160ms --t-std:240ms --t-emph:380ms --t-scene:620ms`.

### 4.2 Shared `@keyframes` (defined once, used everywhere)

| Name | Keyframes |
|---|---|
| `fade` | `from{opacity:0} to{opacity:1}` |
| `peek` | `from{opacity:0;transform:scale(.95) translateY(4px)} to{opacity:1;transform:none}` |
| `rise` (v1) | `from{opacity:0;transform:translateY(18px) scale(.985)} to{opacity:1;transform:none}` |
| `rise` (v3 redefinition, wins) | `0%{transform:translateY(10px);opacity:0} 100%{transform:none;opacity:1}` |
| `toast` | `from{opacity:0;transform:translateY(14px) scale(.94)} to{opacity:1;transform:none}` |
| `unfold` | `from{opacity:0;transform:translateY(-5px)} to{opacity:1;transform:none}` |
| `blink` | `50%{opacity:0}` |
| `breathe` | `0%,100%{opacity:1} 50%{opacity:.45}` |
| `hatch-run` | `from{background-position:0 0} to{background-position:10px 0}` |
| `resolve` | `0%{clip-path:inset(0 0 0 0)} 100%{clip-path:inset(0 0 0 100%)}` |
| `eq` | `0%,100%{transform:scaleY(.25)} 30%{transform:scaleY(1)} 60%{transform:scaleY(.5)}` |
| `dive-in` / `dive-out` | translateX ±22px fade |
| `surface-in` | `from{opacity:0;transform:translateX(-22px)}` |
| `branch` | disclosure row grow: `from{opacity:0;transform:translateX(-10px);max-height:0} to{...;max-height:32px}` |
| `draw` | `from{stroke-dashoffset:var(--len,100)} to{stroke-dashoffset:0}` |
| `pop` (v1) | `0%{scale(.6);opacity:0} 70%{scale(1.08);opacity:1} 100%{scale(1)}` |
| `pop` (v3 redefinition, wins) | `0%{scale(.6);opacity:0} 70%{scale(1.08);opacity:1} 100%{transform:none;opacity:1}` |
| `sweep` (v1) | `from{translateX(-100%)} to{translateX(100%)}` |
| `sweep` (v3 redefinition, wins) | `0%{translateX(-120%)} 100%{translateX(320%)}` |
| `ring-pop` | box-shadow ring expand-and-vanish (`0 0 0 0 var(--peri-line)` → `0 0 0 10px transparent`) |
| `twinkle` | `.ground` star flicker, ×3.2 opacity at 50% |
| `bevel-run` | `to{--a:360deg}` — the "running" bevel's conic sweep |
| `facet-glint` | `0%,82%,100%{opacity:var(--t)} 88%{opacity:calc(var(--t) + .4)}` |
| `facet-run` | `0%{opacity:1} 16%,100%{opacity:var(--t)}` |
| `seam-run` | `to{transform:translateX(8px)}` |
| `drop-in` | complex "land" arc: `0%` off-top squashed `scale(.6,1.3)` → `45%` overshoot `scale(1.18,.78)` at rest position → `62%` slight rebound `translateY(-16px) scale(.94,1.08)` → `78%` settle `scale(1.06,.94)` → `100%` none |
| `make-room` | neighbour displacement: `translateY(calc(var(--row,34px)*-1))` → `55%` overshoot `translateY(5px)` → `100%` none |
| `ripple` | `rotate(45deg) scale(.4)→scale(3.2)`, opacity .9→0 |
| `flow` | `to{stroke-dashoffset:-24}` (strand "value in flight") |

### 4.3 Board-specific `@keyframes` (36 extra, one demo loop per board)

All follow the identical **drop-land-settle** shape as `drop-in`/`make-room`, retimed per board and looped `infinite` for the live demo boards. Exact timings (`animation: name duration easing [iteration]`):

| Board | Keyframes | Timing |
|---|---|---|
| AddFlow | `row-drop`, `row-room`, `gem-land`, `typing` | `8s var(--glide) infinite` (row-drop/room/gem-land); `typing 8s steps(4) infinite` |
| AdminMembers | `mem-drop`, `mem-room`, `peel2` | `9s var(--glide) infinite`; `peel2 .4s var(--glide) both` (one-shot disclosure) |
| Ask | `ask-blink` | `1s steps(2) infinite` (caret) |
| Brand | `b-focus`, `b-inset` | `3.6s var(--glide) infinite` (bevel-focus pulse demo) |
| ClassPage | `peel-open`, `mixin-beat`, `tr-pulse` | `peel-open .5s var(--glide) both`; `mixin-beat 3.6s ease-in-out infinite` (box-shadow pulse periwinkle↔bevel); `tr-pulse 3.4s ease-in-out infinite` (opacity 1↔.5) |
| Controls | `sweepdemo`, `ctoast-in` | `sweepdemo 2.4s linear infinite`; `ctoast-in .5s var(--bounce) both` |
| Descent | `drop-loop`, `rip-loop` | `7s var(--glide) infinite` / `7s linear infinite` |
| Inbox | `pz-ask`, `pz-dropped`, `pz-zone`, `pz-rip`, `pz-drop` | all `11s var(--glide|linear) infinite` |
| Loading | `st-retry`, `st-draw`, `st-pop` | `6s`/`7s`/`7s`, `var(--glide)`/`var(--glide)`/`var(--bounce)`, all `infinite` |
| MotionLive | `m-drop`, `m-rip`, `m-new`, `m-room`, `m-wave`, `m-desc`, `m-fade`, `m-focus`, `m-inset`, `m-peel` | mostly `4s`/`3.6s`/`4.4s var(--glide|snap|linear) infinite` — this board is the dedicated motion-timing reference |

(9 boards — Collapse, Color, EmptyStates, FaultAnatomy, HintMode, Keyboard, Language, Resize, Source — define **no** board-specific keyframes; they use only the 27 shared ones.)

### 4.4 Hover neighbour-wave (precise, no JS)

Implemented three times with the same CSS idiom (`:has(+ selector:hover)` plus a manual `.n1/.n2/.n3` class fallback for browsers without `:has`), always a **decaying falloff over 3 neighbours**:

- **Comb**: hovered `-9px / scaleY(1.5)`; n1 `-6px/1.3`; n2 `-3.5px/1.16`; n3 `-1.5px/1.06`. Transition `transform .26s var(--bounce), opacity .16s`.
- **Mosaic**: hovered `-3px, scale(1.55)`; immediate neighbour only, `-1.5px` (falloff stops at n1). Transition `.2s var(--bounce)`.
- **Capability marks**: hovered `-3px, scale(1.15)`; immediate neighbour `-1.5px`. Transition `.2s var(--bounce)`.

### 4.5 Reduced motion

Global override: `@media (prefers-reduced-motion:reduce){.nx *{animation-duration:1ms!important;animation-iteration-count:1!important;transition-duration:1ms!important}}`.

---

## 5. Icons

**See `R5-icons.json`** for verbatim SVG markup (111 entries). Summary:

### 5.1 Kind marks (20) — family = colour, shape = kind, `viewBox 0 0 24 24`, wrapper class `k <fam> lg`

- **Namespaces** (`--f-ns`, 4): module, package, import, unknown (dashed diamond placeholder)
- **Types** (`--f-type`, 5): struct, class, enum, union, type
- **Contracts** (`--f-con`, 2): trait, interface
- **Callables** (`--f-call`, 4): function, method, constructor, macro
- **Values** (`--f-val`, 5): constant, field, property, variable, variant

Base `.k` is 20×20px (14px icon), `.k.sm` 16×16 (11px icon), `.k.lg` 28×28 (18px icon). v3: background removed entirely (`background:none!important`), icon strokes go `stroke-linecap:square; stroke-linejoin:miter` ("cut, mitred, one lit facet"); filled sub-paths (`class="f"` / `class="s"`) carry `opacity:.34` vs full-opacity outline strokes.

### 5.2 Modifier marks (17) — short name / full tooltip, `wrapper class="mod [colour]"`, 15×15px

`const`("evaluated at compile time") · `async`("returns a future") · `unsafe`("you uphold the invariants") · `generic` · `static`("one for the whole program") · `crate`("visible inside this crate only") · `deprecated`("since 0.4") · `abstract`("you must provide it") · `derived`("no custom logic") · `blanket`("arrives through a blanket impl") · `auto`("the compiler proves it") · `override`("overrides the inherited one") · `inherited`("unchanged") · `makes`("no self: makes one or stands alone") · `reads`("borrows self: reads only") · `changes`("borrows self mutably: changes it") · `consumes`("takes self: the value is gone after"). Colour accents available: `.mod.amber/.coral/.peri/.mint`.

### 5.3 Capability marks (14, the derive-trait row) — `wrapper class="cap [on|via]"`, 30×30px hit / 18×18 icon

`Clone` `Copy` `Eq` `Ord` `Hash` `Debug` `Display` `From/Into/ToString`(short: convert) `Send/Sync`(short: thread) `Default` `Serialize`(short: serde) `IntoIterator`(short: iter) `Deref` `Error`.

### 5.4 UI icons (50), `viewBox 0 0 24 24`, class `ico [sNN]`

search, orbit, package, layers, trail, rose, mosaic, comb, peel, book, file, folder, seal, shield-check, lock, key, eye, pin, bookmark, tag, bell, inbox, msg, note, user, users, settings, filter, clock, history, globe, target, spark, zap, play, copy, link, side-l, peek, split, grid, list, more, alert, info, diamond, crown, heart, cursor, server. Size variants via `.ico.s12/.s14/.s18/.s20/.s24/.s32` (stroke-width scales inversely, 2.0 → 1.4).

### 5.5 Core direction glyph

**Chevron** (`viewBox 0 0 24 24`, `path d="m9 6 6 6-6 6"`) — the single glyph used for (a) breadcrumb separators, (b) the `.twisty` disclosure caret (rotates 90° via CSS transform when `.open`/`details[open]`), (c) the "forward" motif described in `Brand.dc.html` as one of the six things "everything comes from the mark": *"The green chevron that closes the wordmark is the separator, the prompt and the way deeper. Forward is always right."*

### 5.6 Language/ecosystem marks (7) — `class="logo-lang"`, 14×14px, real brand marks, `viewBox 0 0 24 24`

Rust (crab, `color:#f0a27a`), TypeScript, Python, Go, Java, C#, C++ — confirms the 7-ecosystem support surface (Nix is not among them). Verbatim official brand SVG paths in `R5-icons.json` (group `"language"`).

### 5.7 Nudox logo (combination mark)

`viewBox 0 0 240 240`, rendered at 64/56/52/40/28px in different contexts, no CSS class (sized via `width`/`height` attrs). Full-colour version uses 8 named `<linearGradient>` defs (`lbd` outer diamond, `lid` inner diamond, `lnd` "N" silver, `lud` "U" silver-blue, `ldd` "D" teal, `lod` "O" mint-ring, `lcd` "X"/chevron mint-green, `lsd` grid-line silver) layered: 3 nested diamond planes → grid tick-mark overlays → the 5 letterforms (N-U-D-O-X) drawn as flat coloured paths → accent tick-mark groups. Verbatim in `R5-icons.json` (group `"logo"`).

**Wordmark** is *not* a separate SVG — it's set text: `.wordmark{font:660 112px/100px var(--display);letter-spacing:-.055em;font-stretch:88%}` (i.e. Bricolage Grotesque, 660 weight, 112px).

### 5.8 "Everything comes from the mark" — six motifs (Brand.dc.html)

1. The diamond → the gem (§3.6): every kind is a 12-facet cut stone.
2. The hatch stripes beside the letters → the "pending/inferred" texture (`--hatch`) and the seam (§3.9).
3. The green chevron closing the wordmark → direction/disclosure (§5.5).
4. The periwinkle bevel of the diamond → keyboard focus and state generally (§3.1 Focus row).
5. The falling bars under the chevron → the comb (§3.7).
6. The cut corner of the diamond → every raised surface (§3.1).

---

## 6. Component catalogue

*(Boards: Controls, Plates, DataMarks, TypeSheet, Language, Brand, Color — canvas 1440×1320/1760/1230/1950/1760/1420/1340 respectively, all `.doc` layout, `padding:56px 64px`.)*

| Component | Anatomy | States / variants |
|---|---|---|
| **Button** `.btn` | pill (or `.cutb` 8px chamfer) plate, 30/24/38/46px heights, optional leading `.ico` | rest, hover (facet sweep), press, focus (doubled peri bevel), busy (hatch fill, text hidden), disabled (opacity .42); intents: default/`.primary`/`.edge`/`.ghost`/`.danger` |
| **Icon button** `.ibtn` | 28×28px (sm 22×22), 8px/5px radius | rest, hover (`--line2` fill), active (scale .92), `.on` (mint tint) |
| **Key cap** `.kbd` | 18px-tall min-width chip, mono/UI numerals | default, `.hot` (mint, on Cmd-held), `.hint` (periwinkle, hint-mode letter overlay, uppercase mono 700) |
| **Segmented control** `.seg` | pill or `.sq` (8px radius) track, 24px items | rest / `.on` (plate3 two-tone bevel) |
| **Switch** `.switch` | 32×18px track, 14px round knob | off / `.on` (mint fill, knob slides +14px) |
| **Checkbox** `.check` | 16×16px, 5px radius diamond-family shape | off / `.on` (mint fill) / `.mixed` (periwinkle) |
| **Radio** `.radio` | 16×16px circle | off / `.on` (5px inset mint ring) |
| **Slider** | built from `.comb` (see §3.7), filled ticks = value, tallest tick = thumb | — |
| **Chip** `.chip` | pill, 22px (sm 18 / lg 28), optional dismiss `.x` | plain / `.on` / colour `.mint/.peri/.amber/.coral` / `.bare` / `.sample` (dashed outline + hatch swatch) |
| **Tag** `.tag` | 16px chip, uppercase 9.5px/700 | plain / `.mint/.peri/.amber/.coral` |
| **Count badge** `.count` | 16px pill, tabular numerals | plain / `.mint` |
| **Semver pill** `.semver` | 18px, 3 coloured mono segments | maj(coral) / min(amber) / pat(mint) |
| **Input / text field** `.input` | cut plate, caret, 32px (sm 26 / xl 60) | rest, focus (periwinkle ring), bad (coral ring), ok (signal ring) |
| **Select trigger** | same as `.input` | rest / open |
| **Card** `.card` | flat `--plate`/`--card` fill, 12px radius, 1px hairline | rest, hover/`.lift` (translateY -2px), `.sel` (periwinkle ring), `.sunk` (inset shadow) |
| **Cut plate** `.cut` | chamfered flat surface, see §3.1 | rest/focus/hot/amber/coral/ghost/two/deep/weave/lift |
| **Toast** `.toast` | 14px-radius glass pill | enter animation `toast 380ms var(--spring)`; "says what happened once, offers undo" |
| **Dialog** `.dialog` | 480px, 20px radius, glass | default / destructive (coral bevel) |
| **Tooltip** (`[data-tip]::after`) | 5px-chamfered plate3, arrow-less | opacity/translate on hover or `.is-hover`; `.tip-l`/`.tip-r` flip anchor |
| **Popup / hovercard** `.hovercard`/`.menu` | 220px+ glass, chamfer via child rules | menu item `.mi` (28px row), `.mi.on`, `.mi.bad` (coral) |
| **Peek card** | plate with code preview + action row | "reads a symbol without leaving this one" |
| **Banner** `.banner` | 1-line strip, left-tinted | `.amber/.coral/.peri/.mint` |
| **Meter** `.meter` | 5px bar | plain / `.peri/.amber/.coral` / `.sample` (hatch) / `.run` (animated hatch) |
| **Bars** `.bars` | 40px sparkbars | `.hot` per-bar, `.sample` (hatch) |
| **Ring** `.ring` | conic-gradient donut, 40px (sm 26/lg 64) | value via `--v` (0–100), colour via `--c` |
| **Stat** `.stat` | number (display font, 20px/700) + caption | — |
| **KV list** `.kv` | 2-col grid | — |
| **Table** `.tbl` | uppercase 10px headers, 12.5px rows | row hover, `.on` (periwinkle tint) |
| **Avatar** `.av` | 22px circle (sm 18/lg 32/xl 44), gradient fill | `.team` (7px radius square), presence dot **removed in v3** (`.dot,.av>.on{display:none!important}`) |
| **Skeleton** `.skel` | hatch-textured placeholder | `.solid` variant breathes instead of hatch-scrolls |
| **Empty state** `.empty` | centred icon/copy/CTA stack | see §7 EmptyStates |
| **Fault card** `.fault` | coral-tinted panel | `.amber`/`.peri` recolours; full anatomy in §7 FaultAnatomy |
| **Gem** `.gem` | 12-facet cut stone kind mark, doubles as a progress indicator | hollow/partial/working/complete/stalled/cracked/glint (§3.6) |
| **Comb** `.comb` | tick row (§3.7) | horizontal/`.v` vertical, `.down` popover flip |
| **Mosaic** `.mosaic` | stone grid (§3.8) | `.dimmed` |
| **Seam** `.seam` | staged progress strip (§3.9) | done/now/stall/bad/todo |
| **Strand/nodule** | relation graph edge + label (§3.10) | written/via/flow |
| **Capability row** `.caps`/`.cap` | derive-trait icon row (§3.11) | on/via, hover lift |
| **Kind/modifier/capability marks** | see §5 | — |
| **Titlebar (v2)** | trail/thread of `.bead`s + `.here` capsule | see §7 titlebar anatomy |
| **Shelf** | package tree nav, 264px | collapsed → `.kspine` 42px |
| **Altimeter** `.alt` | 4-depth stepper (Orbit/Package/Page/Source) | v3 text-only "on" state |
| **Float/floatwrap** | glass popover shell, own drop-shadow | §3.5 |
| **Package card** `.pkg` | title/desc(2-line clamp)/footer | — |
| **Timeline** `.tl` | vertical dotted line + event dots | `.mint/.amber/.coral/.peri`, `.now` (mint, glow) |
| **Node/edge** (dependency graph) | pill node (30px) + SVG edge | `.me` (mint ring, "you are here"), `.dim`, edge `.hot`/`.guess` (dashed) |
| **Diff line** `.add`/`.del` | tinted row + left bar | mint / coral |
| **Colour swatch** `.swatch` | 76px-min chip w/ caption | used across Color board |

---

## 7. Page anatomy

All canvases are **1440px wide**; height varies per board (listed). Titlebar is always `.titlebar.v2` (50px) unless noted. Status footer is always 26px unless noted. Ordering below is DOM/visual top-to-bottom, left-to-right.

| Board | Canvas | Layout |
|---|---|---|
| **Main** (symbol page) | 1440×1500 | Titlebar-v2 (lights, `.alt` depth stepper w/ "Page" active, bead-thread present›glyph›KindGlyph›**RelationLabel**(here)›Typed, trail-map/inbox icon buttons, team chip+avatar) → Body: `.shelf`(264px: shelf-up crumb, `.book` cut-plate header w/ comb release-graph, search input, package tree `.li` rows, `.dock` 7-icon strip) + `.split` + `.reader`(`.folio` gap 20px, containing the `.rose` relation graph height 318px + `.led`/`.ledhead` member rows + `.xnote` cross-reference callouts) → Status footer (mono URL, "hold ⌘ for keys") |
| **Territory** (package page) | 1440×1080 | Titlebar-v2 (`.alt` "Package" active, single bead + `here`=serde/1.0.193) → same shelf+split+reader shell as Main, `.folio` gap 18px |
| **Orbit** | 1440×900 | Titlebar-v2 (`.alt` "Orbit" active, `.here` replaced by an "Ask anything, or find a package" ⌘K prompt) → shelf (with `.famrow` family-group dividers) + split + `.reader > .sky`(absolute-positioned orbit view: `.sector` labels, `.body-o` planet nodes at literal px coords, `.body-o.far`/`.core` depth variants, `.tier` 236px side panel, `.adding` in-flight node, `.float` bottom-centred action dock) |
| **AddFlow** | 1440×900 | Titlebar-v2 (no shelf/rail this board) → `.reader`-only, `.folio` max-width 1180px gap 22px: heading row, a focused `.cut.lg.focus` search/add plate, `.olist` (results list), a divider row |
| **Source** | 1440×900 | Titlebar-v2 (bead-thread ending at `as_str`) → `.reader`-only, `.srcwrap` (source-with-peeled-doc view) |
| **TraitPage** | 1440×1400 | Titlebar-v2 (`.alt` "Page", here=Deserialize) → shelf+split+reader (`.folio` gap 20px) — same shell as Main/ClassPage, tallest of the "Page" family due to trait method list length |
| **ClassPage** | 1440×1300 | identical shell to TraitPage, here=DataFrame (pandas) |
| **Ask** | 1440×900 | Titlebar-v2 (`.alt` "Page") + shelf+split+reader (`.folio` gap 18px) **plus** an overlay: `.scrim` + centred `.floatwrap.askwrap`(`.cut.lg.focus.askplate`: `.ask-input` search field w/ result count, `.ask-body`(`.ask-list.stagger` + `.ask-prev` preview pane), `.ask-foot`(4 `.cut.sm` scope-token chips + move/open/close key hints)) |
| **SearchResults** | 1440×1040 | Titlebar-v2 (`.alt` "Orbit", here-bar shows the literal query "retry with backoff" + ⌘K) → shelf+split+reader, `.folio` max-width 1140px gap 20px |
| **Recommend** | 1440×560 (shortest canvas) | Titlebar-v2 (`.alt` "Page") → shelf+split+reader, `.folio` gap **6px** max-width 1180px (a dense recommendation list) |
| **Onboarding** | 1440×900 | Titlebar-v2 (no `.alt` state — first-run copy "First run, point Nudox at a project", sealing-count badge) → `.reader > .pz-sky` (dependency-arrival orbit: `.pz-sector` labels, `.pz-body`(+`.is-ghost` not-yet-arrived) stone nodes with `.lb` labels, `.pz-center`(`.pz-dropzone` dashed target + `.pz-ask`/`.pz-dropped` icon states), `.pz-caption`, `.pz-cta`(primary CTA + skip link)) → Footer `.status.pz-idxstatus`: 5-stage row (Fetch/Unpack/Parse/Resolve/Seal, current stage `.now`) + `.seam` progress strip + "N of M packages sealed" |
| **SettingsAppearance** | 1440×900 | Titlebar-v2 (breadcrumb "Settings › Appearance") → `.shelf`(no book/search, just a 6-item nav list: Appearance/Index & Registries/Keymap/Notifications/Privacy/Account) + split + `.reader`(`.folio` max-width 900px, 6× `.pz-sec` setting sections) |
| **SettingsIndex** | 1440×900 | identical shell, nav item 2 active, `.folio` max-width 920px |
| **Inbox** | 1440×900 | Titlebar-v2 (breadcrumb "Inbox / 4 unread") → `.reader`-only, `.folio` max-width 760px: `.pz-day` date dividers + `.cut.sm.pz-inrow` message rows (states: `.hot`, `.focus.focus`, plain, `.coral`) → Footer: unread count, J/K move, E archive, ⌘↵ open, ⇧A mark-all-read |
| **Offline** | 1440×900 | Titlebar-v2 (Orbit ⌘K state) → **`.cut.sm.amber.offbar`** full-width banner ("crates.io isn't answering", retry countdown `.retryseam`, attempt counter, Retry-now button) *above* the body → shelf(`.lib-folio`)+split+reader gap 16px → Footer notes "local index 2.41M symbols" |
| **Loading** | 1440×1180 | Titlebar-v2 (`.alt` "Page", here=RelationLabel) → shelf+split+reader (`.folio` gap 20px) — a skeleton/resolve-state variant of Main |
| **EmptyStates** | 1440×860 | `.doc` layout (FACET 7.1) → `.egrid` 12-col grid of `.cut.sm.eroom` cards (`grid-column:span 5/7/4/4/4`), each with `.estage`(120px-min icon composition) + `.stsent` sentence + CTA button. 5 scenarios: empty package (add-project CTA), no-symbol-found (search-wider CTA), no-readme (open-reference CTA), no-implementors (write-first-impl CTA), nothing-shared (share-package CTA) |
| **FaultAnatomy** | 1440×900 | `.doc` layout (FACET 7.2) → `.fa-wrap`(flex): `.cut.sm.coral.fa-plate`(5 numbered `.pinrow`s, circular `.pinmark` badges 20px) + `.fa-legend`(5 numbered explanation rows) → below, `.fa-row`: 3 `.cut.sm.fa-compact` folded/collapsed variants (coral/amber/peri), each with an error code (`ORCL-0x7F3-PYREFLY`, `IDX-SCHEMA-6-9`, `POLICY-LICENCE-GPL3`). 5-part anatomy: (1) plain-words headline, (2) strand-of-nodules cause chain, (3) what Nudox already tried, (4) ≤2 actions, (5) mono/copyable error code |
| **Keyboard** | 1440×1180 | `.doc` layout (FACET 08) → `.legend3`(3 key-colour meanings: hot=mint "acts on cursor target", peri="moves cursor", plain="jumps to a place") → `.panel`("Depth"): Orbit→Package→Page→Source `.pkitem` chain + descends/surfaces pair → `.leaf-row`: `.krose` compass (322px, 660px max) + `.mg.peri.tie` margin note explaining up/down/left/right semantics → `.panel`("Move and act"): `.bindgrid` 2-col key-binding table (Move focus J/K or ↑↓, Peek Space, Peel-to-source S, Hint-mode F, Ask ⌘K, Walk-thread ⌘[/⌘], Move-zones Tab) → `.panel`("Cmd held"): 2× 640px-wide mini titlebar frames (`.kbtb-frame`) contrasting at-rest vs Cmd-held |
| **HintMode** | 1440×900 | Titlebar-v2 with every actionable region wearing an `.hwrap`+`.kbd.hint.hintcap` letter overlay (2-letter hints like "SD"/"SF"/"GJ", the active one styled `<b>A</b>S`) → shelf+split+reader (`.folio` gap 18px) → floating `.hintstatus` readout bottom-right ("A narrows to 5 of 12") |
| **Resize** | 1440×760 | `.doc` layout (FACET 09) → `.rrow`: 4 shrinking `.rframe`+`.rshot`(live 1440×900 app screenshot scaled 0.32, 0.32×0.8, etc.) frames at 461/352/243/154px wrapper widths, each with `.rlabel`+`.rnote` caption → `.panel`("The four rules"): `.rules` 2-col grid (breakpoint → consequence) |
| **Collapse** | 1440×900 | Titlebar-v2 (single bead, here=glyph) → Body: `.kspine`(42px icon-only shelf) + `.reader`(flex 1.2, split reader pane, `.folio` padding `22px 22px 22px 26px`) + `.split.drag` + second `.reader`(flex 1, mirrored padding) → floating `.floatwrap` readout ("512 px") pinned mid-canvas → Footer: ⌘. "zen, one page no shelf" / ⌘\ "reopen shelf" |
| **TrailMap** | 1440×900 | Titlebar-v2 (bead-thread, here=as_str) → `.reader > .folio`(no max-width, `padding:24px 28px`, `height:100%`): top row + a grow row containing a 12-col `.map` grid (64px row height) of package `.cell`s, plus a horizontal `.scrub` timeline (44px, 24px rotated-diamond `.knob`, 2×12px tick marks) |
| **Descent** | 1440×1160 | `.doc` layout (FACET 04) → `.panel`: `.strip` containing **4 nested live-app screenshots** (`.shot.nx`, each a full `.win` mockup) stacked diagonally at `transform:scale(0.232)`, positioned `left:0/330/660/990px, top:40/130/220/310px` (constant Δ+330px/+90px per depth) — "the whole app is one zoom" | → below, 3 `.panel.grow` summary cards |

**Common app-shell chrome present on nearly every board above** (titlebar-v2): traffic-light `.lights`(3× 12px), sidebar-toggle `.ibtn`, `.alt` 4-stage depth stepper (Orbit/Package/Page/Source), bead-`.thread` (breadcrumb-as-relation-trail: older beads shrink 11→9→7px and fade, the current one is the `.here` capsule, forward beads are hollow/`.fwd`), trail-map + inbox icon buttons, team `.chip`+avatar, personal `.av`. Common footer: mono resource URL (left), "hold ⌘ for keys" hint (right).
