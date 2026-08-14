//! The macOS menu bar — the only UI lindsey has left once the window is gone.
//!
//! # Why this file exists at all
//!
//! `grep -rn 'set_menus' workspace/gui` returned **nothing** before this change.
//! lindsey shipped with no menu bar, and the consequences were larger than a
//! missing convenience:
//!
//! * **`cmd-Q` did nothing.** gpui installs no default quit binding anywhere —
//!   `MacPlatform::quit` is only ever reached from an explicit `App::quit`
//!   (`gpui_macos/src/platform.rs:503`), and the keymap registry had no `Quit`
//!   row. The only way to stop lindsey was to kill the process, which skips
//!   `on_app_quit` and therefore skips the drain of in-flight agent requests
//!   the MCP host implements. A background-resident app you cannot quit
//!   cleanly is worse than one that dies with its window.
//! * **A dismissed window was a dead end.** Keystrokes reach GPUI through a
//!   window's dispatch tree, so with zero windows *no* keymap entry can fire.
//!   AppKit still routes menu key equivalents and menu clicks, because the menu
//!   bar belongs to the application rather than to any window. That makes the
//!   menu the only possible keyboard route back, and the only place a reader
//!   can be told the app is still running.
//!
//! # Why the endpoint is printed here
//!
//! §L6 hosts one MCP endpoint per process and the status bar shows it — but the
//! status bar is *in the window*, so the moment the window is dismissed the
//! reader loses the one thing that says what the still-running process is
//! doing. The app menu carries the same fact, derived from the same
//! [`McpStatus`], so "is it up, and at what address?" has an answer in both
//! states rather than only in the state where it is least needed.
//!
//! Rendering the endpoint from [`McpStatus`] rather than from an
//! `Option<String>` is the same L35 lesson: `Absent`, `Failed` and `Stopped`
//! must not collapse into one silent nothing.
//!
//! # Key equivalents come from the keymap, not from here
//!
//! `MacPlatform::create_menu_item` looks each item's action up in the live
//! keymap and uses the first binding it finds
//! (`gpui_macos/src/platform.rs:311-327`, `find_or_first`). So the shortcut
//! printed beside "Quit lindsey" is *the same registry row* `?` teaches, and a
//! menu item cannot advertise a shortcut the keymap does not have.
//!
//! One consequence is load-bearing and easy to get wrong: an AppKit key
//! equivalent is consumed by the menu **before** GPUI ever sees the keystroke.
//! Putting `cmd-W` on a "Close Window" item would therefore silently break
//! `CloseTab`, which is bound to `cmd-W` in the `Pane` context. Close Window is
//! `cmd-shift-W` for exactly that reason.

use gpui::{Menu, MenuItem, NoAction, SharedString};

use crate::app::account::AccountStatus;
use crate::app::actions::{CopyMcpEndpoint, DismissWindow, HideApp, OpenAccount, Quit, ShowWindow};
use crate::app::mcp::McpStatus;

/// The name shown in the leftmost (application) menu, and in Quit / Hide.
///
/// macOS renders the first menu's title in bold as the application's identity.
/// It is the package name rather than "Nudox" because that is what the process
/// is called in Activity Monitor and in the Dock, and a resident background app
/// that calls itself two different things is one the reader cannot find to
/// quit.
const APP_NAME: &str = "lindsey";

/// Build the whole menu bar for a given MCP status and account status.
///
/// A pure function of both statuses so it can be asserted on without a
/// platform: `TestPlatform::set_menus` is an empty stub
/// (`gpui/src/platform/test/platform.rs:417`), so nothing installed can be read
/// back. The value handed to the platform is the only testable surface, and
/// this is it.
pub fn main_menu(status: &McpStatus, account: &AccountStatus) -> Vec<Menu> {
    let mut app_items = vec![
        // Informational, and deliberately first: with the window dismissed this
        // line is the entire answer to "what is this process doing?".
        //
        // `NoAction` has no handler, so gpui's `validateMenuItem:` hook reports
        // it unavailable and AppKit greys it out — which is the rendering an
        // informational row wants, arrived at by it genuinely not being
        // clickable rather than by a flag that claims so.
        MenuItem::action(status.menu_label(), NoAction).disabled(true),
    ];

    // Offered only when there is something to copy. A permanently-present
    // "Copy MCP Endpoint" would be enabled in every state, because
    // `is_action_available` answers per action type and cannot see that this
    // particular server failed to bind — so the honest control is one that is
    // absent when the address is.
    if status.url().is_some() {
        app_items.push(MenuItem::action("Copy MCP Endpoint", CopyMcpEndpoint));
    }

    // The account panel: sign in, see the account, sign out. Present in every
    // posture this process actually has a gate for (`AccountStatus::menu_label`
    // hides it only for `Absent`) — with the window dismissed this is the
    // *only* way back into a signed-in session's sign-out button, since the
    // status bar chip lives inside the window.
    if let Some(label) = account.menu_label() {
        app_items.push(MenuItem::separator());
        app_items.push(MenuItem::action(label, OpenAccount));
    }

    app_items.extend([
        MenuItem::separator(),
        MenuItem::action(format!("Hide {APP_NAME}"), HideApp),
        MenuItem::separator(),
        MenuItem::action(format!("Quit {APP_NAME}"), Quit),
    ]);

    vec![
        Menu::new(APP_NAME).items(app_items),
        // Named "Window" exactly: `MacPlatform::create_menu_bar` matches that
        // string to call `setWindowsMenu:`, which is what lets AppKit manage
        // the standard window entries alongside ours
        // (`gpui_macos/src/platform.rs:258-262`).
        Menu::new("Window").items([
            MenuItem::action(format!("Show {APP_NAME}"), ShowWindow),
            MenuItem::action("Close Window", DismissWindow),
        ]),
    ]
}

/// Every action a menu item dispatches, for the test that pins them.
///
/// Exists so the assertion "each of these has a global handler" is written
/// against a list the menu itself produces, rather than against a second list
/// that has to be kept in step with it.
pub fn menu_action_names(menus: &[Menu]) -> Vec<SharedString> {
    fn walk(items: &[MenuItem], out: &mut Vec<SharedString>) {
        for item in items {
            match item {
                MenuItem::Action { action, .. } => out.push(action.name().into()),
                MenuItem::Submenu(menu) => walk(&menu.items, out),
                MenuItem::Separator | MenuItem::SystemMenu(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    for menu in menus {
        walk(&menu.items, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    // Needed for `Quit.name()` on a *concrete* action type. Method calls on
    // `&dyn Action` resolve without it, which is why the non-test code above
    // does not import it.
    use gpui::Action as _;

    fn listening() -> McpStatus {
        McpStatus::Listening {
            url: SharedString::from("http://127.0.0.1:51234/mcp"),
            client_config: SharedString::from("{}"),
        }
    }

    /// This process hosts no account gate — every test above the account
    /// section pins the menu against this, so adding the item did not change
    /// what they assert about `Quit`/`Show`/`Hide`/`Copy MCP Endpoint`.
    fn no_account() -> AccountStatus {
        AccountStatus::Absent
    }

    /// A real posture, pre-rendered exactly the way `app::account` derives it
    /// for the status bar and the overlay — not a hand-built stand-in, so a
    /// passing test here is evidence the *real* derivation produces a
    /// distinct menu line, not evidence of a copy of it.
    fn account_posture(posture: nudox_engine::mcp::Posture) -> AccountStatus {
        AccountStatus::Live(crate::app::account::AccountPresentation::derive(
            &posture,
            None,
            None,
            &nudox_engine::mcp::account::host::UsageReport {
                pending: 0,
                dropped: nudox_engine::mcp::account::ledger::DroppedTally::default(),
                persistent: false,
            },
        ))
    }

    fn titles(menus: &[Menu]) -> Vec<String> {
        menus
            .iter()
            .flat_map(|menu| {
                menu.items.iter().filter_map(|item| match item {
                    MenuItem::Action { name, .. } => Some(name.to_string()),
                    _ => None,
                })
            })
            .collect()
    }

    /// The reason this file exists. Without a Quit item there is no `cmd-Q`,
    /// and a background-resident process can only be killed — which skips the
    /// `on_app_quit` drain the MCP host relies on.
    #[test]
    fn the_menu_bar_can_always_quit_and_always_summon() {
        for status in [
            McpStatus::Absent,
            listening(),
            McpStatus::Stopped,
            McpStatus::failed(&nudox_engine::mcp::McpError::Bind {
                addr: "127.0.0.1:0".parse().expect("a literal loopback address"),
                source: std::io::Error::other("denied"),
            }),
        ] {
            let names = menu_action_names(&main_menu(&status, &no_account()));
            for required in [
                Quit.name(),
                ShowWindow.name(),
                DismissWindow.name(),
                HideApp.name(),
            ] {
                assert!(
                    names.iter().any(|n| n.as_ref() == required),
                    "every MCP state must still offer {required}; {status:?} offered {names:?}",
                );
            }
        }
    }

    /// L35 restated for the windowless case: the four MCP states must read
    /// differently in the one place a reader can still see them.
    #[test]
    fn each_mcp_state_reads_differently_in_the_menu() {
        let states = [
            McpStatus::Absent,
            listening(),
            McpStatus::Stopped,
            McpStatus::failed(&nudox_engine::mcp::McpError::Bind {
                addr: "127.0.0.1:0".parse().expect("a literal loopback address"),
                source: std::io::Error::other("denied"),
            }),
        ];
        let lines: Vec<String> = states
            .iter()
            .map(|s| titles(&main_menu(s, &no_account()))[0].clone())
            .collect();

        for (i, a) in lines.iter().enumerate() {
            for (j, b) in lines.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a, b,
                    "{:?} and {:?} render the same menu line {a:?} — that is the \
                     collapse L35 was",
                    states[i], states[j],
                );
            }
        }
    }

    /// The address must be copy-able out of the menu bar *verbatim*, because
    /// with the window dismissed this is the only place it appears.
    #[test]
    fn a_listening_server_prints_and_offers_its_url() {
        let menus = main_menu(&listening(), &no_account());
        let first = titles(&menus)[0].clone();
        assert!(
            first.contains("http://127.0.0.1:51234/mcp"),
            "the menu must show the dialable url verbatim: {first:?}",
        );
        assert!(
            menu_action_names(&menus)
                .iter()
                .any(|n| n.as_ref() == CopyMcpEndpoint.name()),
            "a listening server must offer a copy affordance",
        );
    }

    /// A server that never bound has no address, so a copy control would put
    /// nothing — or worse, a stale port — on the clipboard.
    #[test]
    fn a_server_with_no_address_offers_no_copy() {
        for status in [McpStatus::Absent, McpStatus::Stopped] {
            assert!(
                !menu_action_names(&main_menu(&status, &no_account()))
                    .iter()
                    .any(|n| n.as_ref() == CopyMcpEndpoint.name()),
                "{status:?} has no url; offering to copy one is a lie",
            );
        }
    }

    /// `cmd-W` closes a *tab*. If a menu item ever claims it, AppKit consumes
    /// the keystroke before GPUI can dispatch `CloseTab` and the binding dies
    /// with no diagnostic anywhere.
    ///
    /// Asserted against the keymap rather than against a hard-coded string, so
    /// the guard follows the registry if the tab binding ever moves.
    #[test]
    fn no_menu_item_steals_a_context_scoped_keystroke() {
        use crate::app::keymaps::KEYMAP_REGISTRY;

        let menu_actions = menu_action_names(&main_menu(&listening(), &no_account()));

        // Every keystroke AppKit will now intercept, because some menu item
        // carries it as a key equivalent.
        let intercepted: Vec<&'static str> = KEYMAP_REGISTRY
            .iter()
            .filter(|entry| {
                let action = (entry.binding)();
                menu_actions
                    .iter()
                    .any(|name| action.action().name() == name.as_ref())
            })
            .map(|entry| entry.keystroke)
            .collect();

        for entry in KEYMAP_REGISTRY {
            // A context-scoped binding is only ever reachable through a
            // window's dispatch tree. If AppKit eats the keystroke first, the
            // binding dies silently — no error, no log, and `?` keeps teaching
            // it. `cmd-W`/`CloseTab` is the concrete case this guards.
            if entry.context == "global" || entry.context.starts_with('!') {
                continue;
            }
            assert!(
                !intercepted.contains(&entry.keystroke),
                "a menu item carries {:?}, which is also the {:?}-scoped \
                 binding for {:?}. AppKit consumes menu key equivalents before \
                 GPUI dispatches, so that binding can never fire again.",
                entry.keystroke,
                entry.context,
                entry.description,
            );
        }
    }

    /// With no account gate in this process, there is nothing for the item to
    /// open — offering one would be a button that does nothing, the exact
    /// defect this feature exists to remove from the status bar.
    #[test]
    fn no_gate_offers_no_account_item() {
        let menus = main_menu(&listening(), &no_account());
        assert!(
            !menu_action_names(&menus)
                .iter()
                .any(|n| n.as_ref() == OpenAccount.name()),
            "a process with no account service must not offer an account menu item",
        );
    }

    /// A real gate, in any posture, offers a way in from the menu bar — this
    /// is the only route back to sign-out with the window dismissed.
    #[test]
    fn a_real_gate_always_offers_the_account_item() {
        for posture in [
            nudox_engine::mcp::Posture::SignedOut,
            nudox_engine::mcp::Posture::Active {
                account: sample_account(),
                source: nudox_engine::mcp::account::store::KeySource::Keychain,
                quota: nudox_engine::mcp::account::state::QuotaKnowledge::Unknown,
            },
        ] {
            let menus = main_menu(&listening(), &account_posture(posture.clone()));
            assert!(
                menu_action_names(&menus)
                    .iter()
                    .any(|n| n.as_ref() == OpenAccount.name()),
                "{posture:?} must still offer a way to reach the account panel",
            );
        }
    }

    /// L35 restated a third time: signed-out and signed-in must not collapse
    /// to the same menu line, or a user cannot tell from the menu bar alone
    /// whether they need to act.
    #[test]
    fn signed_out_and_signed_in_read_differently_in_the_menu() {
        let signed_out = account_posture(nudox_engine::mcp::Posture::SignedOut);
        let signed_in = account_posture(nudox_engine::mcp::Posture::Active {
            account: sample_account(),
            source: nudox_engine::mcp::account::store::KeySource::Keychain,
            quota: nudox_engine::mcp::account::state::QuotaKnowledge::Unknown,
        });
        assert_ne!(
            signed_out.menu_label(),
            signed_in.menu_label(),
            "signed-out and signed-in must not render the same account menu line",
        );
    }

    fn sample_account() -> nudox_engine::mcp::account::state::Account {
        nudox_engine::mcp::account::state::Account {
            user: nudox_engine::mcp::account::state::UserId(24),
            fingerprint: nudox_engine::mcp::ApiKey::parse("ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517")
                .expect("sample key parses")
                .fingerprint(),
            verified_at: std::time::SystemTime::now(),
        }
    }
}
