//! What a person can do with a project on the shelf, as typed row commands.
//!
//! One vocabulary for every surface that lists projects: the Library's cards
//! (`failure.rs`) and the sidebar's rows (`shell/side/*`, W-Side) ask the same
//! two questions of it, and never spell an intent or a label themselves:
//!
//! - [`ProjectCommand::for_phase`]: which commands make sense for a project in
//!   this phase, in the order a row's menu or a card's buttons list them;
//! - [`ProjectCommand::intent`] / [`ProjectCommand::label`] /
//!   [`ProjectCommand::weight`]: what pressing one dispatches, what it says, and
//!   how loudly it is drawn.
//!
//! A caller dispatches the intent through its own `Links`; nothing here draws.

use crate::core::LocalProjectId;
use crate::model::ProjectPhase;
use crate::navigation::Intent;

/// One thing to do with a project.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ProjectCommand {
    /// Make it the active project (nothing is indexed again).
    Activate,
    /// Ask the owner to index it again after it stopped.
    Retry,
    /// Ask the owner to index it again after it was paused.
    Resume,
    /// Show its folder in the file manager.
    Reveal,
    /// Add its folder again (the one it was is gone).
    Locate,
    /// Take it off the shelf. The owner keeps what it indexed.
    Remove,
}

/// How loudly a command is drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Weight {
    /// The one thing that gets the project moving again.
    Primary,
    /// Everything else.
    Plain,
    /// Destructive.
    Danger,
}

impl ProjectCommand {
    /// What it says on the button or menu row.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Activate => "Make active",
            Self::Retry => "Try again",
            Self::Resume => "Resume",
            Self::Reveal => "Reveal",
            Self::Locate => "Add it again",
            Self::Remove => "Remove from shelf",
        }
    }

    /// How loudly it is drawn.
    #[must_use]
    pub(crate) const fn weight(self) -> Weight {
        match self {
            Self::Retry | Self::Resume | Self::Locate => Weight::Primary,
            Self::Activate | Self::Reveal => Weight::Plain,
            Self::Remove => Weight::Danger,
        }
    }

    /// What pressing it dispatches for `project`.
    #[must_use]
    pub(crate) fn intent(self, project: &LocalProjectId) -> Intent {
        match self {
            Self::Activate => Intent::ActivateProject(project.clone()),
            Self::Retry | Self::Resume => Intent::RetryIndex(project.clone()),
            Self::Reveal => Intent::RevealProject(project.clone()),
            Self::Locate => Intent::OpenAddProject,
            Self::Remove => Intent::RemoveProject(project.clone()),
        }
    }

    /// The commands that make sense for a project in `phase`, in the order a
    /// menu lists them. `active` is whether it already is the active project.
    #[must_use]
    pub(crate) fn for_phase(phase: ProjectPhase, active: bool) -> Vec<Self> {
        match phase {
            ProjectPhase::Ready => [(!active).then_some(Self::Activate), Some(Self::Reveal), Some(Self::Remove)].into_iter().flatten().collect(),
            ProjectPhase::Indexing => vec![Self::Reveal, Self::Remove],
            ProjectPhase::Cancelling => vec![Self::Reveal],
            ProjectPhase::Cancelled => vec![Self::Resume, Self::Reveal, Self::Remove],
            ProjectPhase::Failed => vec![Self::Retry, Self::Reveal, Self::Remove],
            ProjectPhase::Unconfirmed => vec![Self::Reveal],
            ProjectPhase::Missing => vec![Self::Locate, Self::Remove],
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_phase_offers_what_makes_sense_and_each_command_dispatches_its_own_intent() {
        let project = LocalProjectId::new("/tmp/nudox-commands-project").expect("identity");
        let offered = |phase, active| ProjectCommand::for_phase(phase, active).into_iter().map(ProjectCommand::label).collect::<Vec<_>>();
        assert_eq!(offered(ProjectPhase::Failed, true), ["Try again", "Reveal", "Remove from shelf"]);
        assert_eq!(offered(ProjectPhase::Unconfirmed, true), ["Reveal"], "an ambiguous mutation offers no second submission");
        assert_eq!(offered(ProjectPhase::Cancelled, false), ["Resume", "Reveal", "Remove from shelf"]);
        assert_eq!(offered(ProjectPhase::Missing, false), ["Add it again", "Remove from shelf"], "a folder that is gone cannot be revealed");
        assert_eq!(offered(ProjectPhase::Ready, false), ["Make active", "Reveal", "Remove from shelf"]);
        assert_eq!(offered(ProjectPhase::Ready, true), ["Reveal", "Remove from shelf"], "the active project need not be made active");
        assert_eq!(offered(ProjectPhase::Cancelling, false), ["Reveal"], "nothing to remove or resume while the owner is stopping it");
        assert_eq!(ProjectCommand::Retry.intent(&project), Intent::RetryIndex(project.clone()));
        assert_eq!(ProjectCommand::Resume.intent(&project), Intent::RetryIndex(project.clone()));
        assert_eq!(ProjectCommand::Reveal.intent(&project), Intent::RevealProject(project.clone()));
        assert_eq!(ProjectCommand::Remove.intent(&project), Intent::RemoveProject(project.clone()));
        assert_eq!(ProjectCommand::Activate.intent(&project), Intent::ActivateProject(project.clone()));
        assert_eq!(ProjectCommand::Locate.intent(&project), Intent::OpenAddProject);
        assert_eq!(ProjectCommand::Remove.weight(), Weight::Danger);
    }
}
