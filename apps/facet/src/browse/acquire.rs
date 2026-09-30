//! Adding a release to the library (W-Acquire): the offer, the work, and
//! what came of it, said the same way wherever browsing meets a package the
//! person does not have (Find's answers, a package page's header).
//!
//! One control, four states. An **offer** is a teal `+` and where the source
//! is: on this machine, or only in the registry (which needs a download, so
//! the offer says so and passes the request to the shell). **Adding** is the
//! seam, one stage per step the work really takes (find the source, unpack
//! its archive when that is all there is, index it), marching on the ambient
//! pulse: no timer of its own. **Added** says so and opens the package.
//! **Failed** says why in the owner's words and offers the same `+` again.
//!
//! The component never contacts anything: the shell supplies the state and
//! the two actions.

use super::view::{child, words};
use crate::controls::button::button;
use crate::controls::glyph::Glyph;
use crate::data::progress::{Stage, StageState, seam};
use crate::icons::{Icon, IconSize, ui};
use crate::measure::{Control, Measure, Space};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Window, div};
use std::path::PathBuf;
use std::rc::Rc;

/// Where an offered release's source is.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Place {
    /// Unpacked on this machine.
    Unpacked,
    /// Its registry archive is on this machine; adding unpacks it.
    Archive,
    /// Only in the registry: adding it needs a download.
    Download,
    /// More than one local source matches; choose the intended file first.
    Ambiguous { indexes: Vec<PathBuf> },
    /// A local archive is present, but its contents have not been verified.
    UnverifiedArchive(PathBuf),
}

impl Place {
    /// What the offer says about where the source is.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Unpacked => "on this machine".to_owned(),
            Self::Archive => "on this machine, packed".to_owned(),
            Self::Download => "needs a download".to_owned(),
            Self::Ambiguous { indexes } => format!(
                "choose one matching source before adding: {}",
                indexes.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
            Self::UnverifiedArchive(path) => format!(
                "verify this archive before adding: {}",
                path.display()
            ),
        }
    }

    /// Whether this source has a safe add action. The shell remains the
    /// authority on whether a requested download can proceed under the user's
    /// policy.
    #[must_use]
    pub const fn addable(&self) -> bool {
        matches!(self, Self::Unpacked | Self::Archive | Self::Download)
    }
}

/// A release this person can add.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Offer {
    /// The release's address: what [`AddActions::add`] receives.
    pub release: SharedString,
    /// `anyhow 1.0.104`.
    pub label: SharedString,
    /// Where its source is.
    pub place: Place,
    /// Its page, when it is already in the library.
    pub library: Option<SharedString>,
}

/// One step of adding a release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Step {
    /// Waiting its turn (another release is being indexed).
    Waiting,
    /// Finding its source.
    Resolving,
    /// Checking and unpacking its archive.
    Unpacking,
    /// Indexing it.
    Indexing,
}

impl Step {
    const fn words(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting its turn",
            Self::Resolving => "Finding its source",
            Self::Unpacking => "Unpacking its archive",
            Self::Indexing => "Indexing it with its compiler",
        }
    }
}

/// Where adding an offered release stands.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Adding {
    /// Not asked for.
    Idle,
    /// Under way.
    Working(Step),
    /// In the library; `open` is its page's address.
    Added {
        /// What [`AddActions::open`] receives.
        open: SharedString,
    },
    /// Not added, and why.
    Failed(SharedString),
}

/// What the shell supplies: the state of an offer, adding it, opening it.
#[derive(Clone)]
pub struct AddActions {
    /// Where adding `release` stands (read while drawing).
    pub state: Rc<dyn Fn(&SharedString, &mut App) -> Adding>,
    /// Adds `release`.
    pub add: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    /// Cancels the shell's exact in-flight ticket for `release`, when one is
    /// available. `None` hides the control rather than offering a no-op.
    pub cancel: Option<Rc<dyn Fn(SharedString, &mut Window, &mut App)>>,
    /// Opens an added package's page.
    pub open: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
}

/// The seam's stages for `offer` at `step`: only the steps its source needs.
#[must_use]
pub fn seam_stages(place: &Place, step: Step) -> Vec<Stage> {
    let steps: &[Step] = match place {
        Place::Archive => &[Step::Resolving, Step::Unpacking, Step::Indexing],
        Place::Unpacked | Place::Download => &[Step::Resolving, Step::Indexing],
        Place::Ambiguous { .. } | Place::UnverifiedArchive(_) => &[],
    };
    // Waiting: nothing has started.
    let at = steps.iter().position(|known| *known == step).unwrap_or(usize::MAX);
    steps
        .iter()
        .enumerate()
        .map(|(index, known)| {
            let state = if at == usize::MAX { StageState::Todo } else { match index.cmp(&at) {
                std::cmp::Ordering::Less => StageState::Done,
                std::cmp::Ordering::Equal => StageState::Now,
                std::cmp::Ordering::Greater => StageState::Todo,
            } };
            // Indexing is most of the wait: it takes most of the strip.
            Stage::new(known.words(), state).weight(if *known == Step::Indexing { 4.0 } else { 1.0 })
        })
        .collect()
}

/// The control for `offer`, in whatever state the shell says it is.
pub fn add_control(id: impl Into<ElementId>, offer: &Offer, actions: &AddActions, measure: &Measure, cx: &mut App) -> AnyElement {
    let id: ElementId = id.into();
    let palette = cx.palette();
    let state = match ((actions.state)(&offer.release, cx), &offer.library) {
        (Adding::Idle, Some(page)) => Adding::Added { open: page.clone() },
        (state, _) => state,
    };
    let row = div().flex().flex_wrap().items_center().gap_x(measure.space(Space::Roomy)).gap_y(measure.space(Space::Tight));
    let caption = |part: &str, text: SharedString| words(child(&id, part), text, ty::CAPTION, palette.ink2, measure);
    let body = match state {
        Adding::Idle => {
            let add = Rc::clone(&actions.add);
            let release = offer.release.clone();
            let button = button(child(&id, "add"), "Add to library", measure)
                .primary()
                .size(Control::Small)
                .glyph(Glyph::Plus)
                .disabled(!offer.place.addable())
                .on_click(move |window, cx| add(release.clone(), window, cx));
            row.child(button).child(caption("where", format!("{} · {}", offer.label, offer.place.words()).into())).into_any_element()
        }
        Adding::Working(step) => div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Tight))
            .child({
                let mut status = row.child(caption("step", format!("{} · {}", step.words(), offer.label).into()));
                if let Some(cancel) = &actions.cancel {
                    let cancel = Rc::clone(cancel);
                    let release = offer.release.clone();
                    status = status.child(
                        button(child(&id, "cancel"), "Cancel", measure)
                            .ghost()
                            .size(Control::Small)
                            .on_click(move |window, cx| cancel(release.clone(), window, cx)),
                    );
                }
                status
            })
            .child(div().w_full().child(seam(child(&id, "seam"), seam_stages(&offer.place, step), measure)))
            .into_any_element(),
        Adding::Added { open: target } => {
            let open = Rc::clone(&actions.open);
            let button = button(child(&id, "open"), "Open", measure)
                .ghost()
                .size(Control::Small)
                .icon(Icon::Package)
                .on_click(move |window, cx| open(target.clone(), window, cx));
            row.child(ui(Icon::ShieldCheck, IconSize::S18, palette.mint.base))
                .child(words(child(&id, "added"), format!("{} is in your library", offer.label), ty::SMALL, palette.ink1, measure))
                .child(button)
                .into_any_element()
        }
        Adding::Failed(reason) => {
            let add = Rc::clone(&actions.add);
            let release = offer.release.clone();
            let button = button(child(&id, "retry"), "Try again", measure)
                .edge()
                .size(Control::Small)
                .glyph(Glyph::Plus)
                .disabled(!offer.place.addable())
                .on_click(move |window, cx| add(release.clone(), window, cx));
            div()
                .flex()
                .flex_col()
                .gap(measure.space(Space::Tight))
                .child(row.child(ui(Icon::Alert, IconSize::S18, palette.coral.base)).child(words(child(&id, "failed"), format!("{} was not added", offer.label), ty::SMALL, palette.ink1, measure)).child(button))
                .child(caption("reason", reason))
                .into_any_element()
        }
    };
    div().id(id).child(body).into_any_element()
}

#[cfg(test)]
mod tests;
