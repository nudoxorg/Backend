# W1.3 Sidebar surface replacement batch

This isolated batch is based on Backend `6c8656c419660d5e62a69912c534caf2eff72dc7`.
It replaces the nine `SidebarLayout`-only tests in
`vendor/gpui_ce_components/src/sidebar/mod.rs` with one GPUI test that renders the
public `Sidebar` API next to a measurable sibling. The new test is not yet executed;
this document records intended coverage, not a passing result.

The source-only vendor ledger records 16 test bodies in `sidebar/mod.rs`, all reviewed
individually. Its wider same-crate assignment covers 21 source files and 243 direct test
bodies, all substantively reviewed; that is review coverage, not runtime verification.
The sibling `sidebar/menu.rs` contributes two further reviewed bodies, for 18 reviewed
direct tests across the sidebar directory.

| Removed private test | Rendered behavior the replacement must assert |
| --- | --- |
| `icon_collapsed_should_use_icon_width_and_icon_rendering` | A left Icon sidebar with 240px width settles to 48px; following content begins at x=48 and the public item receives collapsed state. |
| `icon_expanded_should_use_expanded_width` | A left Icon sidebar with 240px width places following content at x=240 and renders its expanded item. |
| `icon_expanded_with_non_pixel_width_should_keep_original_layout` | A 50%-width Icon sidebar under a 480px parent resolves to 240px and remains visible in expanded state. |
| `none_should_ignore_collapsed_state` | Right-side `None` with `collapsed(true)` remains 240px wide, retains its expanded item, and sits after the following content. |
| `offcanvas_collapsed_with_pixel_width_should_animate_to_zero` | Pixel-width Offcanvas keeps its item mounted during the close transition, then removes it after the transition and releases layout width. |
| `offcanvas_expanded_with_pixel_width_should_use_expanded_width` | Expanded pixel-width Offcanvas occupies 240px on either side. |
| `offcanvas_collapsed_with_non_pixel_width_should_statically_release_layout` | Collapsing a fractional-width Offcanvas releases layout width immediately and does not mount the hidden item. |
| `offcanvas_expanded_with_non_pixel_width_should_keep_original_layout` | Expanded fractional-width Offcanvas resolves to 240px without requiring an animation wrapper. |
| `offcanvas_should_anchor_child_toward_the_content_edge` | At the same midpoint in left and right pixel-width Offcanvas close transitions, rendered item geometry follows the content-facing edge in each orientation. |

The replacement exercises rendered bounds via GPUI's `debug_selector`/`debug_bounds`
test surface and public `Sidebar::{new, side, collapsible, collapsed}` methods. A custom
`ProbeItem` implements the public `Collapsible` and `SidebarItem` contracts so the test
observes which collapsed state reaches rendered content. Fractional width uses
`gpui::relative(0.5)`, which selects the real non-pixel style path while resolving to
240px inside the test's 480px parent.

The batch is nine old bodies to one new body: a net reduction of eight. It makes no
claim toward a 100-test deletion target. The other seven source bodies remain:
`bool_collapsible_should_remain_backward_compatible`,
`animation_id_should_be_scoped_to_sidebar_id`, and the five `SidebarAnimationState`
transition tests. The static rendered matrix does not prove per-sidebar animation-key
isolation or the transition scheduler's repeated-hide, cancellation, and stale-request
laws. Those obligations stay covered by the retained tests until separately replaced
with stronger public-rendered journeys.

For review-batch accounting, [the 150-body cohort ledger](triage/sidebar-surface-batch-150.json)
contains the nine rewritten Sidebar source bodies plus 141 individually reviewed `keep`
bodies from this same `gpui_ce_components` crate. Those 141 rows reuse their exact
source/body hashes and substantive reasons from the frozen 63c2 audit; they remain
unchanged. The cohort therefore has zero test deletions, one pending replacement test,
and a source-body net change of minus eight. The 243-body audit total is crate-wide
review context, not this batch's deletion count.

No performance baseline, tolerance, or runtime guard changes are part of this batch.
