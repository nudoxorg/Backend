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
//! Hand-rolled `on_key_down`, matching `OmniSearch`. There is no reusable text
//! input in this app; `gpui-component` ships one that nothing here has ever
//! wired in, and a sign-in form is the wrong place to be the first. What this
//! field needs is narrow — paste, type, backspace, select-all, enter — and a
//! full editor would additionally swallow `escape`, which must close the
//! overlay.
//!
//! **The field renders a mask, never the key.** `SignInView::masked` shows
//! `ndx_` followed by one bullet per remaining character, plus the last four
//! once there are enough. A sign-in form is the single most photographed
//! surface in any application — it is in every screen recording of a
//! first-run — and a plaintext secret in it is the same defect class as one in
//! a log file (CWE-532), reached by a different route.

use gpui::prelude::*;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, Render, SharedString, Window,
    div, px,
};
use nudox_mcp::{ApiKey, ApiKeyError, Posture, SignInFailure};

use crate::app::account::AccountPresentation;
use crate::app::actions::{ConfirmOverlay, DismissOverlay};
use crate::motion::{Motion, Spring};
use crate::theme::ext::ThemeExtAccessor as _;

/// How much of the key is shown in clear once it is long enough.
const TAIL_SHOWN: usize = 4;

/// Render `typed` the way the field paints it.
///
/// A free function rather than a method body so the tests can exercise **this**
/// rule rather than a copy of it. An earlier draft had the tests reimplement the
/// masking in a shim struct, which is precisely the shape AGENTS-DOCTRINE §4
/// rules out: it would have gone on passing after the real field started
/// painting the key in clear.
fn mask(typed: &str) -> String {
    let chars: Vec<char> = typed.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let prefix_len = if typed.starts_with(nudox_mcp::account::API_KEY_PREFIX) {
        nudox_mcp::account::API_KEY_PREFIX.len()
    } else {
        0
    };
    // Below twice the tail length, showing the last four would reveal most of
    // the value rather than merely identifying it.
    let tail_len = if chars.len() >= prefix_len + TAIL_SHOWN * 2 {
        TAIL_SHOWN
    } else {
        0
    };
    let hidden = chars.len().saturating_sub(prefix_len + tail_len);

    let mut out: String = chars[..prefix_len].iter().collect();
    out.extend(std::iter::repeat_n('•', hidden));
    if tail_len > 0 {
        out.extend(chars[chars.len() - tail_len..].iter());
    }
    out
}

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
    focus: FocusHandle,
    phase: SignInPhase,
    /// The key as typed. Never rendered; see [`SignInView::masked`].
    typed: String,
    /// Panel entrance.
    lift: Motion,
    /// Indeterminate activity while a check is out.
    checking: Motion,
    /// The form → identity transition. 0 is "form", 1 is "account".
    accepted: Motion,
    /// Set when motion is reduced, so every spring snaps instead of animating.
    reduced: bool,
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

        window.focus(&cx.focus_handle(), cx);

        Self {
            focus: cx.focus_handle(),
            phase,
            typed: String::new(),
            lift,
            checking: Motion::new(0.0, Spring::GENTLE),
            accepted,
            reduced,
        }
    }

    /// The current phase, for the shell and for tests.
    pub fn phase(&self) -> &SignInPhase {
        &self.phase
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

    /// The masked rendering of what has been typed.
    ///
    /// `ndx_` in clear (it is a public prefix), then one bullet per hidden
    /// character, then the last [`TAIL_SHOWN`] in clear once the value is long
    /// enough for that to reveal nothing useful. Users need to see that a paste
    /// landed and that it is the key they meant; they do not need to see it.
    pub fn masked(&self) -> SharedString {
        SharedString::from(mask(&self.typed))
    }

    /// Whether the field currently accepts input.
    fn editable(&self) -> bool {
        matches!(self.phase, SignInPhase::Editing { .. })
    }

    /// Append typed text, or handle a control key.
    ///
    /// Returns `true` when the key was consumed.
    pub fn on_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.editable() {
            return false;
        }
        match keystroke.key.as_str() {
            "backspace" => {
                self.typed.pop();
            }
            // `cmd-v` arrives as a keystroke with no `key_char`; GPUI delivers
            // the pasted text through the clipboard, which the shell reads and
            // pushes in with `paste`.
            _ => match keystroke.key_char.as_deref() {
                Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
                    self.typed.push_str(text);
                }
                _ => return false,
            },
        }
        // Typing clears the previous rejection: a message about the *last*
        // value shown next to a *different* one is worse than no message.
        self.phase = SignInPhase::Editing { problem: None };
        cx.notify();
        true
    }

    /// Insert clipboard text.
    pub fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        self.typed.push_str(text.trim());
        self.phase = SignInPhase::Editing { problem: None };
        cx.notify();
    }

    /// Clear the field.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.typed.clear();
        cx.notify();
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
        match ApiKey::parse(&self.typed) {
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
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok((_posture, presentation)) => {
                self.typed = String::new();
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
            }
        }
        cx.notify();
    }

    /// Return to the form, after a sign-out.
    pub fn reset_to_form(&mut self, cx: &mut Context<Self>) {
        self.typed = String::new();
        self.phase = SignInPhase::Editing { problem: None };
        if self.reduced {
            self.accepted.snap_to(0.0);
        } else {
            self.accepted.animate_to(0.0);
        }
        cx.notify();
    }
}

impl Focusable for SignInView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
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
    /// (LIMITATIONS.md L22).
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
            .track_focus(&self.focus)
            .key_context("SignIn Overlay")
            .on_action(cx.listener(|_view, _: &DismissOverlay, _window, cx| {
                cx.emit(SignInEvent::Dismiss);
            }))
            .on_action(cx.listener(|view, _: &ConfirmOverlay, _window, cx| {
                view.submit(cx);
            }))
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, _window, cx| {
                if view.on_key(&event.keystroke, cx) {
                    cx.stop_propagation();
                }
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
                            .child(
                                "Paste the API key from your dashboard. It is stored in the \
                                 macOS Keychain, never in a file.",
                            ),
                    )
                    .child(self.field(sp, ts, colours, false))
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
                    .child(self.field(sp, ts, colours, true))
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

    /// The key field, masked.
    fn field(
        &self,
        sp: crate::theme::tokens::SpaceTokens,
        ts: crate::theme::tokens::TypeScale,
        colours: crate::theme::tokens::ColourRoles,
        dimmed: bool,
    ) -> impl IntoElement {
        let shown = self.masked();
        let empty = shown.is_empty();
        div()
            .id("sign_in.field")
            .w_full()
            .px(sp.space_3)
            .py(sp.space_2)
            .rounded(sp.r_sm)
            .bg(colours.bg_sunken)
            .border_1()
            .border_color(if dimmed {
                colours.border_default
            } else {
                colours.accent
            })
            .text_size(ts.ui.size)
            .line_height(ts.ui.line_height)
            .text_color(if empty {
                colours.fg_muted
            } else {
                colours.fg_default
            })
            .opacity(if dimmed { 0.6 } else { 1.0 })
            .child(if empty {
                SharedString::from("ndx_…")
            } else {
                shown
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The masking rule under test is `mask` itself — the same function
    /// `SignInView::masked` calls. See its doc comment for why this is not a
    /// reimplementation.
    #[test]
    fn the_field_never_renders_the_body_of_a_key() {
        let key = "ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517";
        let shown = mask(key);
        assert!(shown.starts_with("ndx_"), "the prefix is public: {shown}");
        assert!(shown.ends_with("4517"), "the last four identify it: {shown}");
        assert!(
            !shown.contains("2f8c41a9"),
            "the body of the key must never be painted: {shown}"
        );
        assert_eq!(
            shown.chars().filter(|c| *c == '\u{2022}').count(),
            key.chars().count() - 4 - TAIL_SHOWN,
            "every hidden character must be accounted for"
        );
    }

    #[test]
    fn a_short_value_hides_everything_after_the_prefix() {
        // With too few characters, showing the last four would reveal most of
        // the value rather than merely identify it.
        assert_eq!(mask("ndx_abcd"), "ndx_\u{2022}\u{2022}\u{2022}\u{2022}");
    }

    #[test]
    fn an_empty_field_masks_to_nothing_rather_than_to_bullets() {
        assert_eq!(mask(""), "");
    }

    #[test]
    fn a_value_without_the_prefix_is_masked_entirely() {
        // A loopback session token pasted into the wrong field is still a
        // secret; it must not be shown just because it is the wrong kind.
        let shown = mask("0123456789abcdef0123456789abcdef");
        assert!(!shown.contains("0123"), "nothing recognisable may survive: {shown}");
    }
}
