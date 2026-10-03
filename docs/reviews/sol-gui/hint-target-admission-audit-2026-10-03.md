# Hint target admission audit — 2026-10-03

Source-only audit of the 54 production `Target` constructors on the isolated `7dce30a1b11a278389ca725559941afa2a22c17b` base. A `Target` now requires one private `TargetAction` pairing its action with producer admission. The hint code captures origin zone and target-list frame, checks current structural visit and that same target admission before moving native focus, then invokes the guarded callback once after focus. Source-only parsing and diff hygiene passed; native tests and a rebuilt app have not run.

| Constructor | ID expression | Admission used before hint focus and action |
| --- | --- | --- |
| `bodies/orbit.rs:119` | `id.clone()` | local or owner snapshot (project phase) |
| `bodies/orbit.rs:128` | `tree_id.clone()` | local structural (tree project) |
| `bodies/orbit.rs:220` | `id.clone()` | current indexed resource |
| `bodies/orbit.rs:356` | `id.clone()` | owner snapshot |
| `bodies/orbit.rs:408` | `id.clone()` | current indexed resource |
| `bodies/package/cargo_readme.rs:271` | `id.clone()` | exact current Cargo README dependency stamp |
| `bodies/package/cargo_readme.rs:348` | `id.clone()` | exact current Cargo README dependency stamp |
| `bodies/package/cargo_readme.rs:524` | `id.clone()` | exact current Cargo README dependency stamp |
| `bodies/package/folio.rs:843` | `id.clone()` | exact current package read |
| `bodies/package/folio.rs:882` | `id.clone()` | exact current package read |
| `bodies/package/folio.rs:1514` | `leave_id` | exact current package read |
| `bodies/package.rs:517` | `id.clone()` | current package resource/read lease |
| `bodies/package.rs:732` | `id.clone()` | current package resource/read lease |
| `bodies/package.rs:1144` | `id.clone()` | current package resource/read lease |
| `bodies/package.rs:1174` | `id.clone()` | current package resource/read lease |
| `bodies/package.rs:1246` | `id.clone()` | current package resource/read lease |
| `bodies/package.rs:1285` | `id.clone()` | current package resource/read lease |
| `bodies/source/cargo/inventory.rs:81` | `id.clone()` | exact Cargo inventory source revision |
| `bodies/source/cargo/inventory.rs:118` | `id.clone()` | exact Cargo inventory source revision |
| `bodies/source/cargo/inventory.rs:153` | `id.clone()` | exact Cargo inventory source revision |
| `bodies/source/cargo.rs:242` | `id.clone()` | exact Cargo tree dependency stamp |
| `bodies/source/cargo.rs:381` | `id.clone()` | current Cargo source resource |
| `bodies/source.rs:393` | `id.clone()` | local visit + exact retained display Arc/route |
| `bodies/source.rs:454` | `id.clone()` | local visit + exact retained display Arc/route |
| `bodies/source.rs:688` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:908` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:960` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:999` | `id.clone()` | paired exact source + semantic stamps |
| `bodies/source.rs:1042` | `copy_id.clone()` | exact current source stamp |
| `bodies/source.rs:1106` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:1137` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:1166` | `field_id.clone()` | exact current source stamp |
| `bodies/source.rs:1198` | `id.clone()` | exact current source stamp |
| `bodies/source.rs:1325` | `id.clone()` | paired exact source + semantic stamps |
| `bodies/state.rs:176` | `id.clone()` | local structural retry |
| `bodies/symbol/host.rs:104` | `key.clone()` | current symbol page admit receipt |
| `bodies/symbol/host.rs:129` | `key.clone()` | current symbol page admit receipt |
| `bodies/symbol/host.rs:216` | `id.clone()` | current symbol page admit receipt |
| `bodies/symbol/host.rs:243` | `id.clone()` | current symbol page admit receipt |
| `onboard/failure.rs:178` | `door.clone().into()` | local structural recovery |
| `onboard/failure.rs:225` | `id.clone().into()` | owner snapshot or local structural by command |
| `onboard/library.rs:156` | `id.clone().into()` | local structural Library visit |
| `onboard/library.rs:210` | `"add-folder".into()` | local structural Library visit |
| `onboard/library.rs:306` | `"add-folder".into()` | local structural Library visit |
| `side/mod.rs:944` | `item.key.clone()` | existing shelf scene/owner action guard |
| `titlebar.rs:282` | `id.clone()` | local structural Titlebar visit |
| `titlebar.rs:319` | `id.into()` | local structural Titlebar visit |
| `titlebar.rs:439` | `"jump-back".into()` | exact JumpVisit subject/root/attachment |
| `titlebar.rs:502` | `"jump-forward".into()` | exact JumpVisit subject/root/attachment |
| `titlebar.rs:596` | `last_id.clone()` | exact JumpVisit subject/root/attachment |
| `titlebar.rs:635` | `"here".into()` | local structural Titlebar visit |
| `titlebar.rs:702` | `id.clone()` | exact JumpVisit subject/root/attachment |
| `titlebar.rs:772` | `"ask".into()` | local structural Titlebar visit |
| `titlebar.rs:872` | `id.clone()` | exact JumpVisit subject/root/attachment |

The two pure-test constructors in `focus.rs` and `hints.rs` use an explicitly unscoped fixture predicate. They are not production targets. No production constructor uses an unconditional admission predicate. `SymbolHost` no longer registers a target when the producer supplied no `open` action.

Source geometry/keyboard audit: the Run19 restored Code image `1187-code-row-focus-tab` shows a thin outline at window x=0 while visible source rows start around x=320 in the resized image. Expanded-shelf `1210-find-doc-match-open-code` shows the same left-anchored rectangle around the sidebar tick while source rows begin farther right. The next Space in the earlier sequence opened an `advance_signal` Peek, proving some logical target with a semantic `peek` received the command, but AX reports only the window as focused and does not identify that target. `Source` registers `source-line-N` around the row, with its native handle on the first line-number piece; `Reader` paints `FocusGlow` separately from the scroll body and `Shell::peek` uses `Targets::focused_bounds()`.

The source geometry path explains a concrete mismatch: `facet::motion::shared::Shared::prepaint` paints its child under `window.with_layer_transform`, while `Targets::Tracked::prepaint` previously cached the untransformed child bounds for the glow, hints, and peek anchor. GPUI's `insert_hitbox` transforms the same bounds to window space. `Tracked` now records `window.layer_transform().apply_bounds(bounds)` once for window-space consumers and passes the original bounds to `insert_hitbox`, which applies its own transform. Manual target bounds already supplied in window space are unchanged. The mounted source-row/native-button oracle checks paint bounds, current Reader logical and native focus, and target-frame replacement across expanded/collapsed/expanded shelf. This source correction is not a claim that the historical rectangle or Space's Peek had this exact owner; only a new native replay can establish the full visual outcome.

Focused regression intentions: mounted Reader filter hint from a titlebar-focused session, native Return on that filter with Shelf unchanged; picker hint retaining menu focus for Down/Return; same-root indexed owner replacement denied before focus and clipboard mutation; local Settings/Inbox hint admitted after producer replacement; mounted Code source row bounds and native owner across shelf transitions. These tests are source-only and have not been executed.
