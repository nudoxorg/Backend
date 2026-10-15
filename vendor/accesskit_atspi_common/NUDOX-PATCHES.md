# Linux disabled control state translation

This is the published `accesskit_atspi_common` 0.19.1 crate, patched at the
AT-SPI translation layer. No dependencies were upgraded or additionally
vendored. The root Cargo patch selects this copy; the root lockfile removes only
this package's registry source/checksum. The original crate lockfile is retained
as upstream source; the workspace lockfile remains authoritative.

## Origin and retained source

- Published archive SHA-256:
  `023da0e5097f46df7092d5280b02efb9bbf8d93298daeced42652463e357d636`.
- [Upstream revision c88605b96d04431f9c3c792464a0f2f253480e94](https://github.com/AccessKit/accesskit/tree/c88605b96d04431f9c3c792464a0f2f253480e94/platforms/atspi-common),
  recorded in the unchanged `.cargo_vcs_info.json`.
- All 19 published archive files are retained. Only `src/node.rs` differs:
  the disabled condition, a modification notice, and the test module declaration.
  `src/node/tests.rs` is new. `.cargo-ok` is a local registry extraction marker,
  absent from the published archive and intentionally omitted.
- The crate source refers to Apache, MIT, and Chromium licenses that the
  published archive omits. `LICENSE-APACHE`, `LICENSE-MIT`, and `LICENSE.chromium`
  are copied verbatim from that exact upstream revision.
- `UPSTREAM-SOURCE.json` records archive-file hashes before modifications and
  exact license URLs/hashes. Cargo's normalized manifest and its original
  workspace-oriented manifest are both retained unchanged.

## Attribution and minimal change

Canonical source reviewed: `66a07e52acd4b180ead00889a62d183d44794890`.

1. `apps/facet/src/controls/button.rs:667` passes
   `self.disabled || self.busy` to `aria_disabled`. Lines 669-673 attach click
   admission only for an active button.
2. `vendor/gpui-ce/src/elements/div.rs:3503-3504` translates the disabled ARIA
   property into AccessKit's `node.set_disabled()`.
3. The pinned `accesskit_consumer` 0.38.0 `src/node.rs:448-462` reads the
   disabled property and computes read-only-or-disabled. Its role list at
   `src/node.rs:844-863` does not support read-only on `Role::Button`.
4. [Upstream AT-SPI state translation, lines 373-377](https://github.com/AccessKit/accesskit/blob/c88605b96d04431f9c3c792464a0f2f253480e94/platforms/atspi-common/src/node.rs#L373-L377)
   inserts `ReadOnly` only for a role supporting that state; its unconditional
   fallback therefore inserts `Enabled | Sensitive` for a disabled button.

The fallback now uses `else if !state.is_disabled()`. Enabled controls preserve
their existing states. Disabled controls without read-only support lose
`Enabled` and `Sensitive`. Read-only-supporting controls keep their existing
translation, including disabled text controls reporting `ReadOnly` and the
existing treatment of `Editable`. This patch does not broaden that behavior.

The Action interface is separate: upstream `src/node.rs:433-435,457-461,489-490`
uses explicitly supplied Click support. A button without Click correctly lacks
that interface, regardless of its enabled state. The patch preserves explicitly
supplied actions and focus behavior.

## Evidence boundary

Root's actual GUI29 run used frozen source
`3183000a7788d20c2d4bef362a68869a24500cac` and Linux ELF SHA-256
`af7d79ab988dc2cbe2991a2e8deda2a210d6d3184c03f0c6f6a0f48b5734c183`.
The empty Add button was visually disabled, exposed Accessible and Component,
and correctly lacked Action. Its observed AT-SPI state `1124073728` had bits
8, 24, 25, and 30, including the incorrect Enabled and Sensitive flags.

Root's independent evidence file:
`/Users/mileswirht/Downloads/nudox-active-20261013/gui-execution-inputs/gui29-review02-actual-evidence01/root-independent-actual-verification.json`.
Its SHA-256 was independently rechecked as
`874e8fb4f00b86ab270992e9d410b32d735e848c1f699bee92f5f723da9db5a9`.
These observations describe the existing failure; they do not validate this
patch or establish runtime repair.

## Regression coverage and validation limits

`src/node/tests.rs` exercises the real adapter and public PlatformNode API:

- Enabled and disabled buttons, each with and without explicit Click support:
  Enabled/Sensitive, ReadOnly absence, Accessible/Component/Action interfaces,
  action count, focus, and visibility.
- Single-line and multiline text inputs across enabled, read-only, disabled,
  and read-only-plus-disabled states, preserving current text/focus semantics.
- Disable/re-enable updates on a button without Click: Enabled/Sensitive
  notifications and current states, without introducing ReadOnly or Action.

Native tests are **UNRUN**. No Cargo check/build/test, Nix evaluation, native
compiler/index/owner job, or remote native command was run for this change.
The tests require native execution after Root reviews the diff and resource
guard and separately authorizes it. A prospective narrow command is
`cargo test -p accesskit_atspi_common --lib node::tests`; it has not been run.

Source-only checks completed: direct rustfmt on the new test file; Git whitespace
checks; a CPython byte comparison of every copied archive file (only the stated
node.rs changes); archive and license hashes; and an eight-case CPython branch
model. The model changes Enabled/Sensitive only for disabled roles without
read-only support. It is not a Rust adapter test or a GUI test. Cargo resolution,
Rust type checking, adapter execution, state notifications, and actual desktop
behavior remain unverified. No runtime repair credit is claimed.
