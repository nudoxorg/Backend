# Shelf measured layout: post-checkpoint prepaint and sticky closure

Worktree: `/private/tmp/nudox-shelf-measured-layout-20261004`, HEAD `c70bc0ec690dfad51c625177e5852c727c53985e`, original seven-path checkpoint `7f98ccc4e92e8a6e1bf34899e58a9bc5096cffff`. The index, branch, and HEAD were not changed. All eight source paths remain unstaged.

Incremental diff against the checkpoint: `/private/tmp/nudox-shelf-measured-layout-20261004-prepaint-incremental.diff`, SHA256 `8c5d68b4eb5a6d07ed61b0c83a0bd5b8d3f81b197375a912f491e1715f5b3b98`.
Full worktree diff: `/private/tmp/nudox-shelf-measured-layout-20261004-full-revised.diff`, SHA256 `664f68d0424ded1b7afd8404273a234d5d2dfd56eb4cbdcd32e28010df475cb2`.

## Causal source trace

`Div::prepaint` prepaints the list child before invoking `on_children_prepainted` (`vendor/gpui-ce/src/elements/div.rs:1902–1992`). `List::prepaint` freezes `ListPrepaintState.layout` (`vendor/gpui-ce/src/elements/list.rs:1544–1605`), and `List::paint` uses that frozen scroll top and item layouts (`list.rs:1606+`). The former Shelf parent callback corrected the stored ListState only after that child prepaint; therefore the old estimated offset could paint for one frame. The new `ListState::scroll_to_proportional` uses existing `PendingScroll::Proportional`, applied while `layout_items` measures its first target row (`list.rs:1030–1140`), before item prepaint and paint. The parent callback now only retires the ticket and schedules presentation publication. `clear_pending_scroll_adjustment` lets a later native wheel/keyboard/sticky intent keep its new logical offset when it supersedes the ticket.

The sticky `Shelf::sticky` now supplies its full semantic chain, independent of the previous frame's ListState viewport. `clipped_sticky_chain` gives its inner wrapper at least the clip's current height and a fixed content height. A fitting chain starts at the top; an overfull chain is bottom-anchored, clipping outer rows and retaining deepest rows. Each sticky child is flex-none. GPUI `Interactivity::should_insert_hitbox` makes no hitbox for the plain overlay/clip wrappers (`div.rs:2376+`); actual sticky row hitboxes get `window_content_mask` through `Window::insert_hitbox` (`window.rs:6940+`). The trailing fixed-height spacer reserves a native list row.

## Added or strengthened oracles (UNRUN)

- `gpui-ce`: `proportional_target_is_measured_before_its_first_paint` checks target measured at 80px despite 20px hint, 25% offset 20px, and first painted target top -20px.
- Desktop mounted RowLayout: `back_restores_the_same_row_past_an_unmeasured_tall_note` and `broad_fold_and_zoom_preserve_the_measured_items_fraction` now assert the measured fractional offset before the parent settlement callback.
- Desktop mounted synthetic depth-five tree: `freshly_shrunk_viewport_clips_an_old_five_row_sticky_chain` now checks deepest ancestor visible on first shrink and outer ancestor visible at clip top on first expansion.
- Real Shell frame: `real_shelf_resize_keeps_a_native_row_below_the_sticky_clip_on_its_first_frame` mounts the actual Shelf, opens its Types fold, scrolls a nested row and checks current viewport/clip on a single short frame.
- Real Shell owner: `real_drawer_rearms_inert_restoration_and_wheel_retires_queued_intent` checks exact pending ticket through closed/inert drawer, active reopen, native list wheel before a queued restore's prepaint, and stable user offset afterward.

No Cargo build, test, native GUI, CUA, staging, commit, or push occurred. `rustfmt --edition 2024 --emit stdout` parsed the changed Rust files and `git diff --check` passed. These oracles are source only and may expose fixture/layout assumptions when Root runs them. The real Shell sticky fixture has a shallow chain; the depth-five current-height proof is mounted GPUI rather than the full Shell. Native acceptance at 100/150/200% and narrow overlay sizes remains pending. The semantic anchor uses checked `ReadingText` (1024-byte bound); overlong row keys fall back to pixel restoration and may retain offscreen height-estimate uncertainty.

Suggested focused selectors for Root's controlled lane: `cargo test -p gpui-ce --features test-support proportional_target_is_measured_before_its_first_paint`; `cargo test -p backend-desktop --lib measured_layout_tests`; `cargo test -p backend-desktop --lib real_shelf_resize_keeps_a_native_row_below_the_sticky_clip_on_its_first_frame`; `cargo test -p backend-desktop --lib real_drawer_rearms_inert_restoration_and_wheel_retires_queued_intent`.
