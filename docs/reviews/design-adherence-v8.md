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


## Cross-branch integration audit

This section reviews the seven current worktrees as a merge problem. Every listed worktree was inspected as a dirty tree against its common `e8dc3411f02b7ca8c4e1702aba3ff7e320a8265d` base, except `responsive-shell-v8`, whose committed support head is `90b907870efc5d14e76db12675eca59198db873e`. The working trees are not independently cherry-pickable commits. The machine-readable form of this inventory, overlap matrix, boundary, and merge sequence is the `integration_audit` object in [design-adherence-v8.json](</Users/mileswirht/Documents/ChatGPT/backend-design-adherence-review-v8/docs/reviews/design-adherence-v8.json>).

The canonical product boundary is one GUI crate, one evidence crate, and one generic vendor boundary:

| Boundary | Sole authority | It owns | It must not own |
| --- | --- | --- | --- |
| `apps/desktop` | Product GUI crate | GPUI entities, route projections, theme access, runtime projection | A second component/shell/reader/graph GUI crate |
| `apps/desktop/src/theme` | Tokens, palette, type, chrome, per-window ActionFrames lifecycle | Abyss/Glacier, titlebar/rail/input metrics, 90/160/240/380/620ms motion, semantic frame begin/publish | Breakpoint choice, route state, widget-local focus state |
| `apps/desktop/src/core/layout.rs` | Responsive geometry | `ContentConstraints`, `ResponsiveLayout::resolve`, `WidthClass`, `PanelMode`, `HeaderMode`, split/package decisions | Animation clocks, graph-local widths, route-specific width enums |
| `apps/desktop/src/ui` | Stateless GPUI builders plus the measured semantic seam | `foundation.rs`, `surface.rs`, `text.rs`, rich `ActionMetadata`/`ActionTree`/`ActionFrames`, `components::measure` | Reducers, route selection, a parallel gallery semantic schema |
| `apps/desktop/src/navigation` | Typed intents and product focus | `FocusTree`, action scope/key, focus origin, modal trap/restore, escape | Layout rectangles and screenshot serialization |
| `apps/desktop/src/runtime` | Async contract and one `UiRootEntity` | `EngineRequest`/`EngineDto`/`EngineEvent`, coalescing, `AnimationTimeline`, focus/modal projection, resource request identities | Route markup and duplicate package/reader/graph stores |
| `apps/desktop/src/model` | Immutable producer-backed read model | `PackageSurfaceState`, `ReaderIndex`, graph resource, coverage algebra, snapshot field preservation | GPUI elements, focus handles, animation progress |
| `apps/desktop/src/views` | Route projections | Shell/catalog/package/reader/graph markup and insertion points | DTO decoding, a second layout/focus/semantic authority |
| `tools/gui-harness` | Evidence protocol | Per-frame pixels, viewport/scale provenance, crops, semantic probes, conformance, journeys | Product state, layout decisions, synthetic semantic truth, motion timing |
| `vendor/gpui_ce_components` | Generic native adapters | Labels, native focus observation, tab/disabled/toggled behavior | Nudox route state, palette, application semantic schema |

The component foundation is therefore a visual vocabulary, not another state machine. The responsive resolver is a geometry authority, not an animation authority. `ActionFrames` is the only bridge from concrete GPUI layout to semantic evidence. `FocusTree` owns product traversal while native GPUI focus handles provide actuation and native evidence. The graph is a bounded route projection made from retained GPUI elements; its current `graph_canvas` name should become `graph_surface` so the implementation and contract agree.

### Pixels checked against the fresh references

The fresh raster references were opened full size, inspected through crops, and measured directly. The key dimensions are structural evidence for the merge order:

| Reference | Measured pixel fact | Integration implication | Inspected crops |
| --- | --- | --- | --- |
| crates.io serde, 1280×720 | Header ends at `y=66`; dossier card begins at `x=177`, `y=84` and ends at `x=1103` exclusive; side gutter is 177px | A route must preserve a centered reading measure below stable chrome | `/tmp/nudox-integration-audit-v8/crates-io-serde-cua-1280x720-top.png`, `-left.png`, `-center.png`, `-right.png` |
| docs.rs serde, 1280×720 | Toolbar ends at `y=31`; full navigation ends at `x=200`; content begins at `x=200` | A full shelf is a real reading column, not a fixed generic 248px panel | `/tmp/nudox-integration-audit-v8/docs-rs-serde-cua-1280x720-top.png`, `-left.png`, `-center.png`, `-right.png` |
| docs.rs `lib.rs`, 1280×720 | Collapsed rail ends at `x=48`; content begins at `x=56` after an 8px separation | Collapse retains keyboard/re-entry affordances while releasing reading width | `/tmp/nudox-integration-audit-v8/docs-rs-serde-lib-rs-cua-1280x720-top.png`, `-left.png`, `-center.png`, `-right.png` |
| Colour board, 1600×1000 | Dark Facet ground carries the four voice/state treatments | State edges and surface grammar belong in shared primitives | `/tmp/nudox-integration-audit-v8/colour-1600x1000-top.png`, `-middle.png`, `-bottom.png` |
| Brand board, 1600×1000 | Identity is mark/stone/stripe/bevel/comb/cut-corner language | Routine layout remains GPUI geometry; brand marks are a narrowly scoped asset exception | `/tmp/nudox-integration-audit-v8/brand-1600x1000-top.png`, `-middle.png`, `-bottom.png` |
| Descent board, 1600×1000 | Orbit → Package → Page → Source keeps the shell while depth changes | Route transitions need named channels and stable shell geometry | `/tmp/nudox-integration-audit-v8/descent-1600x1000-top.png`, `-middle.png`, `-bottom.png` |
| Language board, 1600×1000 | Hue identifies family; shape identifies kind; edge/mark identifies semantic state | Graph/reader/package nodes must preserve family/kind/state semantics instead of generic cards | `/tmp/nudox-integration-audit-v8/language-1600x1000-top.png`, `-middle.png`, `-bottom.png` |

The HTML contract remains the token authority. `.titlebar` is 46px, `.rail` is 54px, the ordinary input is 32px, the omnibar is 30px, the standard chamfer is 14px, and the motion variables are `90/160/240/380/620ms` at `Color.dc.html:60,123-166,230-284`. The responsive shell’s 52px shelf/context rails and 280/300px full panels are compatible with that rail-first reading model. Motion’s alternate `64/248px` shelf and `72/320px` context widths are not compatible and must not survive as a second geometry vocabulary.

### Exact overlap decisions

The following files have multiple authorities and require extraction. Whole-file cherry-picks are unsafe.

| File and symbols | Branches | Implementation that survives | Small extraction unit |
| --- | --- | --- | --- |
| `apps/desktop/src/ui/components.rs`: `ActionRole`, `ActionMetadata`, `ActionTree`, `ActionFrames`, `measure` | components, motion, a11y | A11y’s rich roles/states/relations/measurement plus motion’s `ActionScope`, stable focus key, focus entries, and scoped helpers | Semantic fields → post-layout/native focus publication → scope/key helpers → visual builder adapters |
| `apps/desktop/src/theme/mod.rs`: frame lifecycle, `record_action_bounds` | components, a11y | A11y’s per-window parent/modal/frame lifecycle and getters; retain one bounds recorder only as a compatibility bridge | Frame token → parent/modal context → measured bounds → harness snapshot |
| `apps/desktop/src/core/layout.rs`: width/panel resolver | responsive, reader, graph, motion | Responsive’s constraint resolver and actual-width/text-scale geometry | Resolver → root observation → route signature migration → diagnostics-only local labels |
| `apps/desktop/src/runtime/animation.rs` plus route transition calls | motion, responsive, graph, reader | Motion’s retargetable timeline and named channels; its `Beat` values match the CSS contract | Clock/channels → panel/disclosure bindings → graph/route bindings → reduced-motion snap |
| `apps/desktop/src/navigation/focus.rs` and action focus helpers | motion, a11y | Motion’s `FocusTree` fed from A11y’s rendered action tree; native focus remains a GPUI evidence/actuation path | Stable action identity → rendered order → modal trap/restore → keyboard/pointer focus-visible origin |
| `apps/desktop/src/runtime/ui_graph.rs`: `UiRootEntity` | responsive, package, reader, graph, motion | One root entity partitioned by layout, animation/focus, package/reader resources, and graph request identity | `last_layout` → motion/focus fields → package/reader fields → graph field/request → one install constructor |
| `apps/desktop/src/views/mod.rs`: `render_root`, route signatures | responsive, reader, graph, motion, a11y | Responsive root composition and one `window.viewport_size()`/layout resolution, extended with feature parameters and frame publish | Window geometry → shell chrome → route dispatch → ActionFrames begin/publish → semantic callback |
| `apps/desktop/src/views/shell.rs`, `catalog.rs`, `primitives.rs` | responsive, reader, graph, motion, a11y | Responsive geometry and shell visuals, motion scoped actions/traps, and A11y measured roles | Shell geometry → semantic wrappers → focus scope/trap → route content |
| `apps/desktop/src/views/package.rs` | responsive, package, graph, a11y | Package’s rich live dossier/lane coverage; graph hooks and shared layout/measurement wrappers are added afterward | Package resources → lane sections → graph insertion point → layout/a11y pass |
| `apps/desktop/src/views/reader.rs` | responsive, reader, graph, a11y | Reader’s document/source folios and `ReaderIndex`; graph hook plus layout normalization and measurement | Reader model → folios → outline/context collapse → symbol graph → semantic wrappers |
| `apps/desktop/src/model/snapshot.rs` | package, reader, graph | One `SnapshotData` preserving package surface, reader index, and graph resource through every clone/update method | Package field → reader field → graph field → constructor preservation audit |
| `apps/desktop/src/runtime/{actor,client,mapping,coordinator,mailbox}.rs` | package, reader, graph | One `EngineRequest`/`EngineDto`/`EngineEvent`: package `SurfaceCommand` lanes, reader root payload, graph request keyed by `GraphNodeId` | Surface mapping → reader root field → graph request/DTO/coalesce arms → stale-root tests |
| `apps/desktop/src/harness.rs` and the GUI harness binary | components, responsive, package, reader, graph, a11y | One live capture entrypoint; gallery is a fixture, and semantics always come from rendered ActionFrames | Capture setup → rendered semantic serializer → route fixtures → one CLI option merge |
| `tools/gui-harness/src/{artifact,gpui_driver,conformance,input,lib,state}.rs` | all seven | Motion’s per-frame viewport/crops/resize path plus A11y’s semantic schema/TextScale, with one union state catalog | Frame viewport/crop → semantic callback → dimension conformance → union stress/state matrix |
| `apps/desktop/src/graph.rs` and `views/graph.rs` | graph | Bounded graph projection and ordinary GPUI node/edge elements; rename `graph_canvas` to `graph_surface` | Bounded projection → retained div segments → pan/zoom → package/reader hooks |

Specific incompatibilities are measurable. The responsive shell currently calls `EffectTransition::new(Duration::from_millis(200))` at `views/mod.rs:131`, while the reference standard is 240ms. The graph route creates a raw 180ms animation at `views/graph.rs:523-526`; it must select a named reveal/unfold beat rather than invent a duration. The reader uses `WidthClass::Compact/Standard/Wide/Full` at `views/reader.rs:186-301`, while the canonical resolver uses `Tiny/Narrow/Medium/Wide/Ultrawide`. The graph model independently defines `ViewportClass::Compact/Regular/Wide` at `graph.rs:72-104`; that class may remain a diagnostic projection only after it is derived from the canonical layout, never as an input to shell geometry.

The component gallery also has a semantic authority collision. `foundation.rs:374-378` calls `Theme::record_action_bounds`, while the A11y branch’s `components::measure` and `ActionFrames` path proves post-layout bounds and native focus. The gallery’s `component_gallery_semantic_artifact` is useful as a fixture description but cannot certify the rendered tree. It must be adapted to consume the same `capture_rendered_semantics` result used by live routes.

### Ordered merge and extraction plan

The safe merge order is deliberately finer-grained than branch order:

1. **U0 — Freeze the common contract.** Keep `apps/desktop` as the only GUI crate and `tools/gui-harness` as evidence-only. Convert dirty trees into reviewable commits; do not cherry-pick a full branch diff.
2. **U1 — Land semantic primitives.** Take the A11y `ActionRole`/`ActionState`/`ActionMetadata`/`ActionTree`/`ActionFrames`, per-window frame lifecycle, modal parent context, post-layout bounds, and native-focus recording. Adapt the component foundation’s tracking call to this seam.
3. **U2 — Land action scopes and product focus.** Take Motion’s `ActionScope`, stable focus keys, `focus_entries`, `FocusTree` origins, traversal, modal trap, escape, and restoration. Feed it from U1’s rendered actions.
4. **U3 — Land the retargetable timeline.** Take Motion’s capture clock, channel IDs, versions, retargeting, and reduced-motion behavior. Make panel/disclosure/overlay/graph values read named channels. Remove raw route timing as each binding is migrated.
5. **U4 — Land the responsive shell.** Keep Responsive’s `ContentConstraints` and `ResponsiveLayout::resolve`, thread one result through `render_root` and routes, and merge `last_layout/observe_layout` into the U3 root. Reject Motion’s hardcoded `target_width` API and older route width enums.
6. **U5 — Land stateless Facet foundation/gallery.** Take the component visual builders and gallery fixture after the shared semantic, layout, focus, and motion seams exist. Make gallery captures use real ActionFrames and the shared per-frame viewport.
7. **U6 — Land package read model/runtime lanes.** Take `PackageSurfaceState`, record retention, lane coverage mapping, and SurfaceCommand handling. Preserve honest `not-yet/unknown/empty/error` states.
8. **U7 — Land reader model/root payload.** Take `ReaderIndex`, reader coverage states, `CatalogState.reader`, and the Root DTO field. Merge every snapshot clone/update method so package and graph resources survive reader updates.
9. **U8 — Land rich package/reader views.** Take the package dossier and reader document/source folios. Add Responsive layout, A11y measurement, Motion scopes, and explicit graph insertion points without replacing either rich projection with an older placeholder.
10. **U9 — Land the bounded graph.** Merge graph request/DTO/coalesce/mapping and graph snapshot resource into the unified runtime. Take the bounded `GraphProjection` and GPUI div edge segments, rename `graph_canvas`, and add package/reader hooks. Keep graph request identity separate from `ensure_surface`.
11. **U10 — Land one evidence harness.** Merge Motion’s per-frame viewport/crop/resize/reversal driver and A11y’s semantic schema/TextScale/conformance. Union `WindowFocus`, text scale, routes, overlays, themes, reduced motion, long copy, and state catalogs. The semantic probe viewport must come from the effective frame viewport.
12. **U11 — Land generic vendor adapters and audit pixels.** Add vendor button/input behavior only after app semantics are stable. Then capture and inspect 1280×720, narrow/collapsed, 200% text, 1×/2×, Abyss/Glacier, reduced/full motion, overlays, and keyboard journeys.

The post-merge acceptance invariant is simple: a route can be traced from producer DTO to immutable snapshot field to one view projection, one responsive layout, one ActionFrames record, one FocusTree entry, one animation channel, one per-frame screenshot, and one semantic probe. If any branch introduces a second owner in that chain, its extraction is incomplete.

### Integration requirement ledger

| ID | Requirement and authoritative selector | Static evidence | Status / explicit gap |
| --- | --- | --- | --- |
| INT-001 | One GUI crate and one evidence crate: `apps/desktop`, `tools/gui-harness`, one GUI harness binary | All seven inventories place product code under `apps/desktop`; no second GUI crate was found | **Pass, static** |
| INT-002 | One responsive authority: `ResponsiveLayout::resolve`, `ContentConstraints::for_text_scale`, `UiRootEntity::observe_layout` | Responsive resolver covers panel fit, text scale, height, package columns, and split panes; reference pixels prove full vs collapsed rails | **Blocked until U4**: motion/reader/graph width vocabularies still overlap |
| INT-003 | One measured semantic authority: `ActionTree`, `ActionFrames`, `components::measure`, `capture_rendered_semantics` | A11y path reads concrete bounds, focus order, and native focus after GPUI layout | **Blocked until U1/U5**: foundation/gallery still has a parallel bounds/synthetic path |
| INT-004 | One focus authority: `FocusTree::sync_action_order`, `ActionTree::focus_entries`, root modal focus sync | Motion has product traversal and restore; A11y has rendered action/native focus evidence | **Blocked until U2**: the two models need one adapter, not two orders |
| INT-005 | One motion authority: CSS variables and Motion `Beat`/`AnimationChannel` | CSS is exact at 90/160/240/380/620ms; Motion timeline has channels and retargeting | **Gap**: raw 200ms shell and 180ms graph animations remain; route bindings are incomplete |
| INT-006 | Snapshot projections: `PackageSurfaceState`, `CatalogState.reader`, graph resource, route views | Package, reader, and graph branches each provide real producer-backed models | **Blocked until U6-U9**: three `SnapshotData`/runtime edits must be field-preserving |
| INT-007 | Unified runtime lanes: `EngineRequest`, `EngineDto`, `EngineEvent`, `CoalesceKey` | Package uses `SurfaceCommand`; reader adds `ReaderIndex` to root; graph adds Graph lane/DTO | **Blocked until U6-U9**: same actor/client/mapping/coordinator files overlap |
| INT-008 | Frame-local resize/scale evidence: `CaptureRecord.viewport`, `FrameArtifact.viewport`, `SemanticProbe.viewport`, crops | Motion artifact path records per-frame viewport; A11y adds semantic schema and crop/hash helpers | **Partial**: semantic callback/conformance must use effective per-frame viewport |
| INT-009 | Stress matrix: route/page, collapse, long copy, text scale, theme, reduced motion, resize/scale, focus/overlay | Responsive/reader state catalogs, Motion `WindowFocus`, A11y `TextScale` all exist | **Blocked until U10**: input/state enums are duplicated |
| INT-010 | GPUI retained construction: ordinary `div()`/edge segments, no canvas/SVG routine fallback | Graph uses ordinary GPUI children and no canvas API was found | **Partial**: rename `graph_canvas`; keep SVG limited to explicit brand/kind assets |
| INT-011 | Reference geometry and language: `.titlebar`, `.rail`, `.btn`, `.input`, `.cut`, `.bevel`, hue family/kind shape/edge state | Fresh crates/docs crops and design-board crops are listed above; CSS selectors were inspected | **Partial**: complete Facet chamfer/bevel/weave and family/kind atlas are not yet composed in every route |

No native product screenshot is claimed by this integration audit. The only pixels accepted as evidence here are the inspected reference images and their crops. No Cargo, Nix, compiler, build, test, or native GUI run was invoked, and no product source file was edited.
