//! The sign-in surface: paste a key, watch it be accepted, see the account.
//!
//! # One view, four phases
//!
//! [`SignInPhase`] is a state machine, not a set of booleans. The four phases
//! are mutually exclusive and each has different legal operations — you cannot
//! submit while a submission is in flight, and you cannot edit the field while
//! the accepted animation is playing — so making them a bool triple would make
//! three of the eight combinations reachable and meaningless.
//!
//! # The animation, and why it is not decoration
//!
//! `src/motion/spring.rs` is a physically-based spring solver that most of this
//! app uses for docks and scrims. Sign-in is the one moment in the product
//! where the *user has handed over a secret and is waiting to be told whether
//! it was right*, and where motion can answer that faster than text can be
//! read.
//!
//! Three springs, each carrying one fact:
//!
//! * [`SignInView::lift`] — the panel rises and settles on
//!   [`Spring::SNAPPY`] as it opens. Standard entrance; it exists so the panel
//!   does not appear as a jump-cut over the workspace.
//! * [`SignInView::checking`] — a determinate-looking sweep under the field
//!   while the network call is out, on [`Spring::GENTLE`]. It is honest about
//!   being indeterminate: it oscillates rather than filling, because we do not
//!   know how long `POST v1/authorize` will take and a bar that crept to 90%
//!   and stopped would be a lie about progress.
//! * [`SignInView::accepted`] — the one that earns its place. On acceptance the
//!   whole panel *collapses toward the account row it becomes*: the form
//!   shrinks and fades while the identity line grows out of it, driven by a
//!   single spring value from 0 to 1. That is not ornament — it says "the thing
//!   you typed became the thing you now are", which is the only information the
//!   user wants at that instant, and it says it before the sentence under it
//!   can be read.
//!
//! Every one of the three checks `reduced_motion()` and calls `snap_to`
//! instead. A user who has asked the system for less motion has asked for it
//! here most of all, because this is a modal they cannot dismiss until it
//! finishes.
//!
//! # The input field
//!
//! `gpui_component::input::{Input, InputState}` — the library's own text
//! input, not hand-rolled. An earlier version of this file hand-rolled
//! `on_key_down` to match `OmniSearch`, on the theory that a sign-in form was
//! the wrong place to be the first caller of a widget nothing here had ever
//! wired in. That theory cost real functionality twice over before anyone
//! noticed: the hand-rolled field never wired a caret and never wired
//! `cmd-v`, because a bespoke reimplementation of a text field has to
//! reinvent IME, selection, undo/redo, and clipboard integration one at a
//! time, and this one had reinvented exactly the parts someone had gotten
//! around to. `InputState` gives all of it for free — including the caret,
//! via its own `blink_cursor`.
//!
//! **The field masks with `InputState::masked(true)`**, the library's own
//! password mode (`•` per character, `input::MASK_CHAR`). This gives up the
//! previous bespoke masking — `ndx_` in clear plus the last four characters
//! once the value was long enough — in favour of the one no hand-rolled
//! field has ever failed to wire correctly: uniform, on every character, all
//! the time. A sign-in form is the single most photographed surface in any
//! application, and a plaintext secret in it is the same defect class as one
//! in a log file (CWE-532), reached by a different route.

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use nudox_engine::mcp::{ApiKey, ApiKeyError, Posture, SignInFailure};

use crate::app::account::AccountPresentation;
use crate::app::actions::{ConfirmOverlay, DismissOverlay};

/// Where a key typed into this card ends up, named for the platform running it.
///
/// This line used to say "the macOS Keychain" everywhere, including on Linux,
/// where there is no Keychain — and, until `account::store` grew a Secret
/// Service backend, no store at all, so the sentence promised something the
/// build could not do. Both halves are now true on both platforms; the const is
/// `cfg`-selected rather than formatted so `render` still does no string work
/// (§11).
#[cfg(target_os = "macos")]
const WHERE_THE_KEY_GOES: &str =
    "Paste the API key from your dashboard. It is stored in the macOS Keychain, never in a file.";
/// See the macOS arm.
#[cfg(target_os = "linux")]
const WHERE_THE_KEY_GOES: &str =
    "Paste the API key from your dashboard. It is stored in your desktop keyring, never in a file.";
/// See the macOS arm.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const WHERE_THE_KEY_GOES: &str =
    "Paste the API key from your dashboard. This build has no credential store, so set \
     NUDOX_API_KEY in the environment instead.";
use crate::motion::{Motion, Spring};
use crate::theme::ext::ThemeExtAccessor as _;

/// What the sign-in surface is doing.
///
/// Exhaustive and mutually exclusive; see the module docs on why this is not
/// three bools.
#[derive(Clone, Debug, PartialEq)]
pub enum SignInPhase {
    /// Waiting for a key. `problem` is set once the user has submitted
    /// something that did not work.
    Editing {
        /// What went wrong last time, and what to do about it.
        problem: Option<SignInProblem>,
    },
    /// A `POST v1/authorize` is in flight. The field is read-only.
    Checking,
    /// The key was accepted; the transition animation is playing or has played.
    Accepted {
        /// The account, pre-rendered.
        account: Box<AccountPresentation>,
    },
    /// The process hosts no account service, so there is nothing to sign in to.
    ///
    /// A real state rather than an error: a `#[gpui::test]` app has no gate,
    /// and telling the user their key was rejected would be a lie.
    Unavailable,
}

/// A rejection, split the same way `McpError` splits `message` and `help`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInProblem {
    /// What happened.
    pub message: SharedString,
    /// What to do about it. Never a restatement of `message`.
    pub help: SharedString,
}

impl SignInProblem {
    /// From an offline validation failure — the common case, and the one that
    /// costs no round trip.
    pub fn from_parse(e: &ApiKeyError) -> Self {
        Self {
            message: SharedString::from(e.to_string()),
            help: SharedString::from(e.help()),
        }
    }

    /// From a failed round trip.
    pub fn from_failure(e: &SignInFailure) -> Self {
        Self {
            message: SharedString::from(e.to_string()),
            help: SharedString::from(e.help()),
        }
    }
}

/// What the shell needs to hear from this view.
#[derive(Clone, Debug)]
pub enum SignInEvent {
    /// Close the overlay.
    Dismiss,
    /// The user asked to sign in with this key.
    ///
    /// The **view does not perform the sign-in**. It cannot: the account
    /// service is a `Global`, and a view that reached for a global to do
    /// network work would be a view that is untestable without one. The shell
    /// owns the effect and hands the result back through
    /// [`SignInView::settle`].
    Submit(Box<ApiKey>),
    /// The user asked to sign out.
    SignOut,
}

/// The sign-in overlay.
pub struct SignInView {
    /// Focus target for phases that render no `Input` — `Accepted` and
    /// `Unavailable`. `Focusable::focus_handle` picks between this and
    /// `input`'s own handle by phase; see that impl for why both must exist.
    focus: FocusHandle,
    input: Entity<InputState>,
    phase: SignInPhase,
    /// Panel entrance.
    lift: Motion,
    /// Indeterminate activity while a check is out.
    checking: Motion,
    /// The form → identity transition. 0 is "form", 1 is "account".
    accepted: Motion,
    /// Set when motion is reduced, so every spring snaps instead of animating.
    reduced: bool,
    _subs: Vec<Subscription>,
}

impl SignInView {
    /// Build the overlay for the current account state.
    ///
    /// The initial phase is derived from the presentation rather than assumed:
    /// opening this while already signed in must show the account, not an empty
    /// form asking for a key the user has already given.
    pub fn new(
        presentation: Option<AccountPresentation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reduced = cx.theme_ext().reduced_motion();
        let phase = match presentation {
            None => SignInPhase::Unavailable,
            Some(p) if p.can_work || p.can_sign_out => SignInPhase::Accepted {
                account: Box::new(p),
            },
            Some(_) => SignInPhase::Editing { problem: None },
        };

        let mut lift = Motion::new(0.0, Spring::SNAPPY);
        let mut accepted = Motion::new(
            f32::from(u8::from(matches!(phase, SignInPhase::Accepted { .. }))),
            Spring::DEFAULT,
        );
        if reduced {
            lift.snap_to(1.0);
            accepted.snap_to(accepted.target());
        } else {
            lift.animate_to(1.0);
        }

        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("ndx_…")
        });
        let subs = vec![cx.subscribe_in(&input, window, Self::on_input_event)];

        let focus = cx.focus_handle();
        // Whichever handle this phase actually renders — see the struct doc
        // and `Focusable::focus_handle` below. Not `view.focus_handle(cx)`,
        // which does not exist until `Self` is constructed; this repeats
        // that method's rule by hand for the one caller who cannot call it.
        if matches!(phase, SignInPhase::Editing { .. }) {
            window.focus(&input.read(cx).focus_handle(cx), cx);
        } else {
            window.focus(&focus, cx);
        }

        Self {
            focus,
            input,
            phase,
            lift,
            checking: Motion::new(0.0, Spring::GENTLE),
            accepted,
            reduced,
            _subs: subs,
        }
    }

    /// Route the field's own events: `Enter` submits, typing clears a stale
    /// rejection message.
    ///
    /// Not wiring this up is the same failure mode as never wiring
    /// `cmd-v` — `InputState` raises these, it does not act on them, and a
    /// view that never subscribes has a field that edits itself and a
    /// "Sign in" button that submits nothing new.
    fn on_input_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                if let SignInPhase::Editing { problem: Some(_) } = &self.phase {
                    self.phase = SignInPhase::Editing { problem: None };
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => self.submit(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// The current phase, for the shell and for tests.
    pub fn phase(&self) -> &SignInPhase {
        &self.phase
    }

    /// The `InputState` backing the field, for tests that need to assert on
    /// what was actually typed rather than through this view's own phase.
    pub(crate) fn input_state(&self) -> &Entity<InputState> {
        &self.input
    }

    /// Whether the form→identity transition has finished playing.
    ///
    /// The shell polls this when this view is standing in as the launch
    /// gate: acceptance must not swap the gate away for the *real* shell
    /// mid-collapse, or the one animation the module docs call load-bearing
    /// — "the thing you typed became the thing you now are" — would never be
    /// seen. `Motion::is_settled` is already true the instant `settle`
    /// snaps a reduced-motion spring, so this is correct in both modes with
    /// no branch of its own.
    pub fn accepted_and_settled(&self) -> bool {
        matches!(self.phase, SignInPhase::Accepted { .. }) && self.accepted.is_settled()
    }

    /// Whether the field currently accepts input.
    fn editable(&self) -> bool {
        matches!(self.phase, SignInPhase::Editing { .. })
    }

    /// Insert text into the field, as if pasted — for tests that need to get
    /// a key into the form without simulating a real keystroke or clipboard
    /// event. `InputState` owns real `cmd-v` itself now (`crate::input::Paste`,
    /// bound in its own `"Input"` key context); this exists only for
    /// programmatic use.
    pub fn paste(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let mut next = self.input.read(cx).value().to_string();
        next.push_str(text.trim());
        self.input
            .update(cx, |input, cx| input.set_value(next, window, cx));
    }

    /// Validate and emit [`SignInEvent::Submit`], or show the problem.
    ///
    /// Validation happens *here*, offline, before anything reaches the network.
    /// A truncated paste is answered instantly and specifically, instead of
    /// costing a round trip and coming back as the service's generic
    /// `invalid token`.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let typed = self.input.read(cx).value();
        match ApiKey::parse(&typed) {
            Ok(key) => {
                self.phase = SignInPhase::Checking;
                if self.reduced {
                    self.checking.snap_to(1.0);
                } else {
                    self.checking.animate_to(1.0);
                }
                cx.emit(SignInEvent::Submit(Box::new(key)));
            }
            Err(e) => {
                self.phase = SignInPhase::Editing {
                    problem: Some(SignInProblem::from_parse(&e)),
                };
            }
        }
        cx.notify();
    }

    /// Deliver the outcome of a submission.
    ///
    /// On success the typed value is dropped immediately — the view has no
    /// further use for it, and a secret that outlives its purpose in a
    /// long-lived UI object is a secret in a heap dump.
    pub fn settle(
        &mut self,
        outcome: Result<(Posture, AccountPresentation), SignInFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok((_posture, presentation)) => {
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.phase = SignInPhase::Accepted {
                    account: Box::new(presentation),
                };
                if self.reduced {
                    self.accepted.snap_to(1.0);
                    self.checking.snap_to(0.0);
                } else {
                    // The one animation that carries information: the form
                    // becomes the account.
                    self.accepted.animate_to(1.0);
                    self.checking.animate_to(0.0);
                }
                // `Accepted` renders no `Input` — the field's own handle,
                // focused while this phase showed it, would otherwise be a
                // focus id absent from this frame's tree, the same failure
                // `Shell::settle_gate_if_ready`'s own doc comment describes.
                window.focus(&self.focus, cx);
            }
            Err(failure) => {
                self.phase = SignInPhase::Editing {
                    problem: Some(SignInProblem::from_failure(&failure)),
                };
                if self.reduced {
                    self.checking.snap_to(0.0);
                } else {
                    self.checking.animate_to(0.0);
                }
                // Back in `Editing`; the field is visible again and is where
                // a retry belongs.
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
            }
        }
        cx.notify();
    }

    /// Return to the form, after a sign-out.
    pub fn reset_to_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.phase = SignInPhase::Editing { problem: None };
        if self.reduced {
            self.accepted.snap_to(0.0);
        } else {
            self.accepted.animate_to(0.0);
        }
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        cx.notify();
    }
}

impl Focusable for SignInView {
    /// `Editing`/`Checking` render the real `Input`; nothing else does. A
    /// caller that focused `self.focus` while this phase is showing the
    /// field would get a visible, styled, apparently-live text box that eats
    /// every keystroke silently — focus would be sitting one level up, on an
    /// ancestor `Input` never claims, so `Input`'s own `on_key_down` never
    /// runs. This is the bug the launch-time gate actually shipped with:
    /// `Shell::new` and `Shell::engage_gate` both refocus through this trait
    /// after construction, so the answer has to be correct for whichever
    /// phase is current, not fixed at construction.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if matches!(
            self.phase,
            SignInPhase::Editing { .. } | SignInPhase::Checking
        ) {
            self.input.read(cx).focus_handle(cx)
        } else {
            self.focus.clone()
        }
    }
}

impl EventEmitter<SignInEvent> for SignInView {}

impl Render for SignInView {
    /// Sole producer of the root element.
    ///
    /// Split exactly as doctrine §8's GPUI note requires: this function applies
    /// `id`, `key_context`, `track_focus` and every `.on_action` once, and
    /// `body` returns children only. No phase branch is in a position to build
    /// a second root, so no phase branch can delete the ancestor context stack
    /// (docs/LIMITATIONS.md L22).
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = std::time::Instant::now();
        let mut animating = false;
        animating |= self.lift.tick(now);
        animating |= self.checking.tick(now);
        animating |= self.accepted.tick(now);
        if animating && !self.reduced {
            window.request_animation_frame();
        }

        div()
            .id("sign_in")
            // Focused only for phases that render no `Input` of their own —
            // see `Focusable::focus_handle`. Left in place (rather than
            // conditionally applied) because `track_focus` only registers
            // this node against `self.focus`'s id for whichever frame that
            // id *is* the active focus; it is a no-op the rest of the time.
            .track_focus(&self.focus)
            .key_context("SignIn Overlay")
            .on_action(cx.listener(|_view, _: &DismissOverlay, _window, cx| {
                cx.emit(SignInEvent::Dismiss);
            }))
            .on_action(cx.listener(|view, _: &ConfirmOverlay, _window, cx| {
                view.submit(cx);
            }))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(self.body(cx))
    }
}

impl SignInView {
    /// Children only. Never a root — see [`Render::render`]'s note.
    fn body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        let lift = self.lift.value();
        let settled = self.accepted.value();
        // The transition: the form shrinks away as the identity grows in. One
        // spring value drives both, so they cannot desynchronise.
        let form_presence = (1.0 - settled).clamp(0.0, 1.0);
        let account_presence = settled.clamp(0.0, 1.0);

        let mut panel = div()
            .id("sign_in.panel")
            .w(px(460.0))
            .flex()
            .flex_col()
            .gap(sp.space_3)
            .p(sp.space_5)
            .rounded(sp.r_xl)
            .bg(colours.bg_raised)
            .border_1()
            .border_color(colours.border_default)
            .shadow_lg()
            // A click inside the panel must not reach the scrim behind it.
            .occlude()
            // The entrance: the panel settles rather than cutting in.
            .top(px(-16.0 * (1.0 - lift)))
            .opacity(lift.clamp(0.0, 1.0));

        match &self.phase {
            SignInPhase::Unavailable => {
                panel = panel
                    .child(
                        div()
                            .text_size(ts.title.size)
                            .line_height(ts.title.line_height)
                            .text_color(colours.fg_default)
                            .child("Accounts are not available here"),
                    )
                    .child(
                        div()
                            .text_size(ts.prose.size)
                            .line_height(ts.prose.line_height)
                            .text_color(colours.fg_muted)
                            .child(
                                "This build of nudox is running without an account service, so \
                                 there is nothing to sign in to.",
                            ),
                    );
            }

            SignInPhase::Accepted { account } => {
                panel = panel
                    .opacity((lift * account_presence.max(0.05)).clamp(0.0, 1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(sp.space_2)
                            .child(
                                div()
                                    .text_size(ts.title.size)
                                    .line_height(ts.title.line_height)
                                    .text_color(colours.fg_default)
                                    .child(account.headline.clone()),
                            )
                            .child(div().flex_1())
                            .children(account.user.clone().map(|user| {
                                div()
                                    .text_size(ts.caption.size)
                                    .line_height(ts.caption.line_height)
                                    .text_color(colours.fg_muted)
                                    .child(user)
                            })),
                    )
                    .child(
                        div()
                            .text_size(ts.prose.size)
                            .line_height(ts.prose.line_height)
                            .text_color(if account.urgent {
                                colours.warn
                            } else {
                                colours.fg_muted
                            })
                            .child(account.detail.clone()),
                    );

                if let Some(hint) = account.key_hint.clone() {
                    panel = panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(sp.space_2)
                            .px(sp.space_3)
                            .py(sp.space_2)
                            .rounded(sp.r_sm)
                            .bg(colours.bg_sunken)
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(colours.fg_muted)
                            .child(hint)
                            .child(div().flex_1())
                            .children(
                                account
                                    .source
                                    .clone()
                                    .map(|source| div().child(format!("via {source}"))),
                            ),
                    );
                }

                if let Some(usage) = account.usage.clone() {
                    let fraction = account.usage_fraction.unwrap_or(0.0);
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(sp.space_1)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(sp.space_2)
                                    .text_size(ts.caption.size)
                                    .line_height(ts.caption.line_height)
                                    .text_color(colours.fg_muted)
                                    .child(usage)
                                    .child(div().flex_1())
                                    .children(account.pending.clone().map(|p| div().child(p)))
                                    .children(account.dropped.clone().map(|d| {
                                        // Shown, not logged. Doctrine §8: a
                                        // repair — here, a usage batch we could
                                        // not confirm and refused to re-send —
                                        // is only permitted when it is visible.
                                        div().text_color(colours.warn).child(d)
                                    })),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.0))
                                    .rounded(sp.r_sm)
                                    .bg(colours.bg_sunken)
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(fraction.clamp(0.0, 1.0)))
                                            .rounded(sp.r_sm)
                                            .bg(if account.urgent {
                                                colours.warn
                                            } else {
                                                colours.accent
                                            }),
                                    ),
                            ),
                    );
                }

                if account.can_sign_out {
                    panel = panel.child(
                        div()
                            .id("sign_in.sign_out")
                            .px(sp.space_3)
                            .py(sp.space_2)
                            .rounded(sp.r_sm)
                            .border_1()
                            .border_color(colours.border_default)
                            .text_size(ts.ui.size)
                            .line_height(ts.ui.line_height)
                            .text_color(colours.fg_default)
                            .cursor_pointer()
                            .hover(|s| s.bg(colours.bg_hover))
                            .on_click(cx.listener(|_view, _event, _window, cx| {
                                cx.emit(SignInEvent::SignOut);
                            }))
                            .child("Sign out"),
                    );
                }
            }

            SignInPhase::Editing { problem } => {
                panel = panel.opacity((lift * form_presence.max(0.05)).clamp(0.0, 1.0));
                panel = panel
                    .child(
                        div()
                            .text_size(ts.title.size)
                            .line_height(ts.title.line_height)
                            .text_color(colours.fg_default)
                            .child("Sign in to nudox"),
                    )
                    .child(
                        div()
                            .text_size(ts.prose.size)
                            .line_height(ts.prose.line_height)
                            .text_color(colours.fg_muted)
                            .child(WHERE_THE_KEY_GOES),
                    )
                    .child(self.field(false))
                    .child(
                        div()
                            .id("sign_in.submit")
                            .px(sp.space_3)
                            .py(sp.space_2)
                            .rounded(sp.r_sm)
                            .bg(colours.accent)
                            .text_size(ts.ui.size)
                            .line_height(ts.ui.line_height)
                            .text_color(colours.accent_fg_on)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                            .on_click(cx.listener(|view, _event, _window, cx| {
                                view.submit(cx);
                            }))
                            .child("Sign in"),
                    );

                if let Some(problem) = problem {
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(sp.space_1)
                            .child(
                                div()
                                    .text_size(ts.prose.size)
                                    .line_height(ts.prose.line_height)
                                    .text_color(colours.danger)
                                    .child(problem.message.clone()),
                            )
                            .child(
                                div()
                                    .text_size(ts.caption.size)
                                    .line_height(ts.caption.line_height)
                                    .text_color(colours.fg_muted)
                                    .child(problem.help.clone()),
                            ),
                    );
                }
            }

            SignInPhase::Checking => {
                panel = panel
                    .child(
                        div()
                            .text_size(ts.title.size)
                            .line_height(ts.title.line_height)
                            .text_color(colours.fg_default)
                            .child("Checking with nudox…"),
                    )
                    .child(self.field(true))
                    .child(
                        // Indeterminate on purpose: the sweep travels, it does
                        // not fill. A bar that crept toward 100% would be
                        // claiming progress we cannot observe.
                        div()
                            .w_full()
                            .h(px(3.0))
                            .rounded(sp.r_sm)
                            .bg(colours.bg_sunken)
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(0.35))
                                    .left(gpui::relative(
                                        (self.checking.value() * 0.65).clamp(0.0, 0.65),
                                    ))
                                    .rounded(sp.r_sm)
                                    .bg(colours.accent),
                            ),
                    );
            }
        }

        panel
    }

    /// The key field. Caret, selection, IME and masking are `InputState`'s
    /// own — nothing here paints any of them.
    fn field(&self, dimmed: bool) -> impl IntoElement {
        div()
            .w_full()
            .child(Input::new(&self.input).disabled(dimmed))
    }
}
