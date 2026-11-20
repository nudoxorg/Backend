//! The first-launch sign-in window: paste an `ndx_` key, have it checked,
//! then the app opens.
//!
//! It is its own small window, shown before the main one, so nothing in the
//! shell has to know whether anyone is signed in. The key is checked with
//! `GET /v1/me` off the UI thread and saved where the CLI and the MCP server
//! read it; only then does [`SignIn`] hand over to the main window.

use crate::model::SettingsState;
use crate::shell::facet_sync::{Surroundings, facet_for};
use crate::shell::kit::text;
use crate::shell::root::is_dark;
use crate::shell::system;
use backend_account::{ApiKey, KEYS_URL, KeyStore, Service, ServiceError};
use facet::controls::{button, field};
use facet::icons::Icon;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Reveal, Space};
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
type AccountCheckFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
#[cfg(test)]
type AccountCheck = Arc<dyn Fn(ApiKey) -> AccountCheckFuture + Send + Sync>;

/// What runs once the key is saved: opens the main window.
pub(crate) type Proceed = Box<dyn FnOnce(&mut App)>;

/// The sign-in window's view.
pub(crate) struct SignIn {
    input: Entity<InputState>,
    /// The one sentence under the field: why the last attempt failed.
    said: Option<SharedString>,
    checking: bool,
    proceed: Option<Proceed>,
    settings: SettingsState,
    around: Surroundings,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    test_account_check: Option<AccountCheck>,
}

impl SignIn {
    pub(crate) fn new(proceed: Proceed, settings: SettingsState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // This window opens before the shell, which otherwise installs the
        // facet: without it the text engine keeps the component theme's
        // Light defaults, near-black ink and caret on the dark plate.
        let around = Surroundings {
            dark: is_dark(window.appearance()),
            text: 1.0,
            system_reduced_motion: system::watch_reduced_motion().initial(),
            display: system::display_key(window, cx),
        };
        apply_facet(&settings, &around, cx);
        let appearance = cx.observe_window_appearance(window, |view: &mut Self, window, cx| {
            view.around.dark = is_dark(window.appearance());
            apply_facet(&view.settings, &view.around, cx);
        });
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("ndx_…")
        });
        let events = cx.subscribe_in(
            &input,
            window,
            |view: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => view.submit(window, cx),
                InputEvent::Change => {
                    if view.said.take().is_some() {
                        cx.notify();
                    }
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            input,
            said: None,
            checking: false,
            proceed: Some(proceed),
            settings,
            around,
            _subscriptions: vec![events, appearance],
            #[cfg(test)]
            test_account_check: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        let key = match ApiKey::parse(&self.input.read(cx).value()) {
            Ok(key) => key,
            Err(error) => {
                self.said = Some(capitalized(&error.to_string()).into());
                cx.notify();
                return;
            }
        };
        self.checking = true;
        self.said = None;
        cx.notify();
        let window_handle = window.window_handle();
        #[cfg(test)]
        let account_check = self.test_account_check.clone();
        cx.spawn(async move |view, cx| {
            let checked = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(check) = account_check {
                        return check(key).await;
                    }
                    check_and_save(&key)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.checking = false;
                match checked {
                    Ok(()) => {
                        if let Some(proceed) = view.proceed.take() {
                            cx.defer(move |cx| {
                                proceed(cx);
                                let _ =
                                    window_handle.update(cx, |_, window, _| window.remove_window());
                            });
                        }
                    }
                    Err(sentence) => view.said = Some(sentence.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Checks the key with the service and saves it; the error is the sentence
/// shown under the field.
fn check_and_save(key: &ApiKey) -> Result<(), String> {
    match Service::new().account(key) {
        Ok(_) => {}
        Err(ServiceError::InvalidKey) => {
            return Err(
                "Nudox doesn't recognize that key. Create a new one and paste it here.".to_owned(),
            );
        }
        Err(ServiceError::Unreachable(_)) => {
            return Err("Can't reach nudox.org. Check your connection and try again.".to_owned());
        }
        Err(error) => return Err(capitalized(&error.to_string())),
    }
    KeyStore::open()
        .and_then(|store| store.save(key))
        .map_err(|error| capitalized(&error.to_string()))
}

/// Installs the facet the saved settings ask for in these surroundings, the
/// same one the shell installs once it opens.
fn apply_facet(settings: &SettingsState, around: &Surroundings, cx: &mut App) {
    facet::set_facet(facet_for(settings, around, Reveal::default()), cx);
}

const HEADING: &str = "Sign in to Nudox";
const INSTRUCTIONS: &str =
    "Paste an API key from your dashboard. The app, the CLI and your MCP clients will all use it.";

fn capitalized(sentence: &str) -> String {
    let mut letters = sentence.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

impl Render for SignIn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = facet.measure(px(400.0));
        let mut key_field = field("sign-in-key", &self.input, &measure).icon(Icon::Lock);
        if let Some(said) = &self.said {
            key_field = key_field.fault(said.clone());
        }
        let submit = cx.entity();
        div()
            .id("sign-in")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(palette.g0)
            .child(
                div()
                    .w(px(400.0))
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Roomy))
                    // The window's words are native reading nodes: text without an
                    // id is invisible to a screen reader (GPUI makes a node only
                    // for an element that has one).
                    .child(
                        text(ty::DISPLAY, &measure, palette.ink0)
                            .keyed("sign-in-heading")
                            .role(gpui::Role::Heading)
                            .aria_level(1)
                            .aria_label(HEADING)
                            .child(HEADING),
                    )
                    .child(
                        text(ty::BODY, &measure, palette.ink1)
                            .keyed("sign-in-instructions")
                            .role(gpui::Role::Label)
                            .aria_label(INSTRUCTIONS)
                            .child(INSTRUCTIONS),
                    )
                    .child(key_field)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                button("sign-in-get-key", "Get a key", &measure)
                                    .ghost()
                                    .on_click(|_, cx| cx.open_url(KEYS_URL)),
                            )
                            .child(
                                button("sign-in-submit", "Sign in", &measure)
                                    .primary()
                                    .busy(self.checking)
                                    .on_click(move |window, cx| {
                                        submit.update(cx, |view, cx| view.submit(window, cx));
                                    }),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AppearancePreference;
    use facet::Appearance;
    use gpui::Focusable as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn wait_for_checking(
        visual: &mut gpui::VisualTestContext,
        view: &gpui::Entity<SignIn>,
        expected: bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            visual.run_until_parked();
            if view.read_with(visual, |view, _| view.checking) == expected {
                return;
            }
            assert!(Instant::now() < deadline, "sign-in checker did not reach the expected pending state");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The sign-in window opens before the shell: its key field must already
    /// write with the facet's ink and caret, not the component theme's Light
    /// defaults (near-black on the dark plate).
    #[gpui::test]
    fn the_key_field_writes_in_the_facet_before_the_shell_opens(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        for (preference, appearance) in [
            (AppearancePreference::Abyss, Appearance::Abyss),
            (AppearancePreference::Glacier, Appearance::Glacier),
        ] {
            let settings = SettingsState { appearance: preference, ..SettingsState::default() };
            let _ = cx.add_window_view(|window, cx| SignIn::new(Box::new(|_| {}), settings, window, cx));
            cx.update(|cx| {
                let facet = cx.facet();
                let theme = gpui_component::Theme::global(cx);
                assert_eq!(facet.appearance, appearance);
                assert_eq!(theme.is_dark(), appearance == Appearance::Abyss);
                assert_eq!(theme.foreground, facet.palette().ink0.hsla(), "typed ink");
                assert_eq!(theme.caret, facet.palette().mint.base.hsla(), "caret");
                assert_eq!(theme.muted_foreground, facet.palette().ink3.hsla(), "placeholder");
            });
        }
    }

    #[gpui::test]
    fn first_sign_in_screen_accepts_unicode_and_paste_without_exposing_the_key_or_authenticating(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(|cx| cx.set_global(gpui::TextTrace));
        let settings = SettingsState {
            appearance: AppearancePreference::Abyss,
            ..SettingsState::default()
        };
        // Production mounts this view under the component Root, which owns
        // native Tab/Shift-Tab dispatch before the main shell exists.
        let mut sign_in = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| SignIn::new(Box::new(|_| {}), settings, window, cx));
            sign_in = Some(view.clone());
            gpui_component::Root::new(view, window, cx).bordered(false)
        });
        let view = sign_in.expect("the production-shaped sign-in view is mounted");
        visual.update(|window, cx| {
            window.set_a11y_forced(true);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let initial = visual.update(|window, _| {
            window.painted_texts().iter().map(|text| text.text.to_string()).collect::<Vec<_>>()
        });
        assert!(initial.iter().any(|text| text == "Sign in to Nudox"), "the first page paints its heading");
        assert!(initial.iter().any(|text| text.contains("Paste an API key from your dashboard")), "the first page explains key entry");
        assert!(initial.iter().any(|text| text.contains("ndx_")), "the key field shows its expected prefix");

        let input = view.read_with(visual, |view, _| view.input.clone());
        let field_focus = input.read_with(visual, |input, cx| input.focus_handle(cx));
        assert!(visual.update(|window, _| field_focus.is_focused(window)), "the key field owns the initial caret");
        visual.simulate_keystrokes("tab");
        assert!(!visual.update(|window, _| field_focus.is_focused(window)), "Tab leaves the key field for a sign-in action");
        let enter = gpui::Keystroke::parse("enter").expect("native Enter");
        visual.simulate_keystrokes("enter");
        visual.simulate_event(gpui::KeyUpEvent { keystroke: enter });
        assert_eq!(visual.opened_url().as_deref(), Some(KEYS_URL),
            "Get a key opens the verified dashboard entry through the test platform only");
        visual.simulate_keystrokes("shift-tab");
        assert!(visual.update(|window, _| field_focus.is_focused(window)), "Shift-Tab returns to the key field");

        visual.simulate_input("fixture-key-123");
        visual.simulate_keystrokes("left");
        visual.simulate_input("λ🧪");
        let typed = input.read_with(visual, |input, cx| input.value().to_string());
        assert!(typed == "fixture-key-12λ🧪3", "Unicode input inserts at the caret");

        // This clipboard belongs to GPUI's test platform, not the macOS pasteboard.
        let pasted_key = format!("ndx_{}", "0".repeat(64));
        visual.write_to_clipboard(gpui::ClipboardItem::new_string(pasted_key.clone().into()));
        visual.simulate_keystrokes("secondary-a");
        visual.simulate_keystrokes("secondary-v");
        assert!(
            input.read_with(visual, |input, cx| input.value().as_ref() == pasted_key),
            "the test clipboard paste replaces the selected text exactly",
        );
        assert!(visual.update(|window, _| field_focus.is_focused(window)), "paste keeps the caret in the field");

        let (paint, accessibility) = visual.update(|window, _| {
            (
                window.painted_texts().iter().map(|text| text.text.to_string()).collect::<Vec<_>>(),
                window.debug_a11y_tree_json().expect("first-page native tree"),
            )
        });
        assert!(!paint.iter().any(|text| text.contains(&pasted_key)), "masked key text is absent from the rendered text");
        assert!(!accessibility.contains(&pasted_key), "masked key value is absent from the accessibility tree");

        // A fake, locally failing checker exercises pending, duplicate-submit,
        // and retry UI states without Service, KeyStore, credentials, or network.
        // GPUI's deterministic executor polls background tasks on its test
        // thread. An async release gate must yield rather than block that
        // thread while the test drives the pending UI.
        let (release_tx, release_rx) = async_channel::bounded::<()>(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let check_calls = Arc::clone(&calls);
        let checker: AccountCheck = Arc::new(move |_key| {
            let release_rx = release_rx.clone();
            let calls = Arc::clone(&check_calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                release_rx.recv().await.expect("test releases the fake check");
                Err("Can't reach nudox.org. Check your connection and try again.".to_owned())
            })
        });
        view.update(visual, |view, _| view.test_account_check = Some(checker));

        visual.simulate_keystrokes("enter");
        wait_for_checking(visual, &view, true);
        assert!(view.read_with(visual, |view, _| view.checking), "the submit action shows a pending state");
        visual.simulate_keystrokes("enter");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a second Enter cannot duplicate the pending check");
        release_tx.try_send(()).expect("release first fake check");
        wait_for_checking(visual, &view, false);
        assert!(!view.read_with(visual, |view, _| view.checking), "the failed request leaves pending state");
        assert_eq!(
            view.read_with(visual, |view, _| view.said.as_deref().map(str::to_owned)),
            Some("Can't reach nudox.org. Check your connection and try again.".to_owned()),
            "the failure is explained inline",
        );
        assert!(view.read_with(visual, |view, _| view.proceed.is_some()), "a failed check cannot open the main window");
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(
            visual.update(|window, _| window.painted_texts().iter().any(|text|
                text.text.contains("Can't reach nudox.org"))),
            "the failed check is visible beneath the input",
        );

        visual.simulate_keystrokes("enter");
        wait_for_checking(visual, &view, true);
        assert!(view.read_with(visual, |view, _| view.checking), "retry returns to the pending state");
        release_tx.try_send(()).expect("release retry fake check");
        wait_for_checking(visual, &view, false);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "retry starts one new check");
        assert!(view.read_with(visual, |view, _| view.proceed.is_some()), "failed retry still does not authenticate");
    }

    /// A person using a screen reader meets this window first. Its heading and
    /// instructions are reading nodes, its field and both buttons carry names,
    /// and a refusal is announced as a status, once.
    #[gpui::test]
    fn the_first_page_is_named_for_a_screen_reader(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let settings = SettingsState { appearance: AppearancePreference::Abyss, ..SettingsState::default() };
        let mut sign_in = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| SignIn::new(Box::new(|_| {}), settings, window, cx));
            sign_in = Some(view.clone());
            gpui_component::Root::new(view, window, cx).bordered(false)
        });
        let view = sign_in.expect("mounted sign-in view");
        let nodes = |visual: &mut gpui::VisualTestContext| {
            visual.update(|window, cx| {
                window.set_a11y_forced(true);
                window.refresh();
                window.draw(cx).clear(cx);
                window
                    .a11y_tree()
                    .expect("forced first-page tree")
                    .nodes
                    .iter()
                    .map(|(_, node)| (node.role(), node.label().map(str::to_owned), node.level()))
                    .collect::<Vec<_>>()
            })
        };
        let count = |nodes: &[(gpui::Role, Option<String>, Option<usize>)], role, words: &str| {
            nodes.iter().filter(|(node_role, label, _)| *node_role == role && label.as_deref() == Some(words)).count()
        };
        let first = nodes(visual);
        assert_eq!(count(&first, gpui::Role::Heading, HEADING), 1, "{first:?}");
        assert_eq!(
            first.iter().find(|(role, label, _)| *role == gpui::Role::Heading && label.as_deref() == Some(HEADING)).and_then(|(_, _, level)| *level),
            Some(1),
            "the window's title is its first-level heading",
        );
        assert_eq!(count(&first, gpui::Role::Label, INSTRUCTIONS), 1, "{first:?}");
        assert_eq!(count(&first, gpui::Role::Button, "Get a key"), 1, "{first:?}");
        assert_eq!(count(&first, gpui::Role::Button, "Sign in"), 1, "{first:?}");
        assert!(
            first.iter().any(|(role, _, _)| matches!(role, gpui::Role::TextInput | gpui::Role::PasswordInput)),
            "the key field is a text input node: {first:?}"
        );

        // A refusal appears as one status node and goes with the next edit.
        view.update(visual, |view, cx| {
            view.said = Some("Nudox API keys start with ndx_".into());
            cx.notify();
        });
        let refused = nodes(visual);
        assert_eq!(count(&refused, gpui::Role::Status, "Nudox API keys start with ndx_"), 1, "{refused:?}");
    }
}
