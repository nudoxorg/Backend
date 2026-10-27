//! The first-launch sign-in window: paste an `ndx_` key, have it checked,
//! then the app opens.
//!
//! It is its own small window, shown before the main one, so nothing in the
//! shell has to know whether anyone is signed in. The key is checked with
//! `GET /v1/me` off the UI thread and saved where the CLI and the MCP server
//! read it; only then does [`SignIn`] hand over to the main window.

use crate::shell::kit::text;
use backend_account::{ApiKey, KEYS_URL, KeyStore, Service, ServiceError};
use facet::controls::{button, field};
use facet::icons::Icon;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Space};
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};

/// What runs once the key is saved: opens the main window.
pub(crate) type Proceed = Box<dyn FnOnce(&mut App)>;

/// The sign-in window's view.
pub(crate) struct SignIn {
    input: Entity<InputState>,
    /// The one sentence under the field: why the last attempt failed.
    said: Option<SharedString>,
    checking: bool,
    proceed: Option<Proceed>,
    _subscriptions: Vec<Subscription>,
}

impl SignIn {
    pub(crate) fn new(proceed: Proceed, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            _subscriptions: vec![events],
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
        cx.spawn(async move |view, cx| {
            let checked = cx
                .background_executor()
                .spawn(async move { check_and_save(&key) })
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
                    .child(text(ty::DISPLAY, &measure, palette.ink0).child("Sign in to Nudox"))
                    .child(
                        text(ty::BODY, &measure, palette.ink1)
                            .child("Paste an API key from your dashboard. The app, the CLI and your MCP clients will all use it."),
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
