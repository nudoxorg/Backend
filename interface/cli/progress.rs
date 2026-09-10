//! Defines progress behavior for `interface-cli`, whose purpose is to project the one shared local library onto a command line.
//! This module owns the progress invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! What an add looks like while it runs: one self-rewriting line on a terminal, one line per phase anywhere else.
//!
//! Progress is written to standard error, so `nudox add … --format json | jq` sees exactly one JSON
//! object and a person watching the same command still sees the compile move. A phase is never a
//! claim of success: only the terminal [`interface_library::AddOutcome`] on standard output says
//! whether the package is readable.

use std::io::{self, Write as _};

use interface_core::PackageCompilePhase;
use interface_library::{
    AddProgress, CompilePhaseProgress,
    render::{common::phase_dots, text::Palette},
};

/// Whether the add is being watched by a person at a terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Liveness {
    /// Standard error is a terminal: one line, rewritten in place.
    Interactive,
    /// Standard error is a file or a pipe: one durable line per phase.
    #[default]
    Logged,
}

/// Draws the phases of one add.
pub(crate) struct AddProgressWriter {
    coordinate: String,
    liveness: Liveness,
    palette: Palette,
    open: bool,
}

impl AddProgressWriter {
    /// Prepares to draw the add of one package.
    pub(crate) const fn new(coordinate: String, liveness: Liveness, palette: Palette) -> Self {
        Self {
            coordinate,
            liveness,
            palette,
            open: false,
        }
    }

    /// Draws one progress event.
    pub(crate) fn observe(&mut self, event: AddProgress) {
        let (dots, label) = match event {
            AddProgress::Admitted { .. } => (phase_dots(PackageCompilePhase::Locate), "admit"),
            AddProgress::Phase(progress) => (phase_dots(progress.phase), progress.label()),
            AddProgress::Indexing => (
                phase_dots(PackageCompilePhase::Discover),
                CompilePhaseProgress::of(PackageCompilePhase::Discover).label(),
            ),
        };
        match self.liveness {
            Liveness::Interactive => self.rewrite(&dots, label),
            Liveness::Logged => self.append(event, label),
        }
    }

    /// Erases the live line so the terminal outcome can take its place.
    pub(crate) fn clear(&mut self) {
        if self.liveness == Liveness::Interactive && self.open {
            let mut error = io::stderr();
            let _ = error.write_all(b"\r\x1b[2K");
            let _ = error.flush();
            self.open = false;
        }
    }

    fn rewrite(&mut self, dots: &str, label: &str) {
        let mut stream = io::stderr();
        let line = format!(
            "\r\x1b[2K{dots} {}  {}",
            self.palette.dim(label),
            self.coordinate
        );
        let _ = stream.write_all(line.as_bytes());
        let _ = stream.flush();
        self.open = true;
    }

    fn append(&self, event: AddProgress, label: &str) {
        let ordinal = match event {
            AddProgress::Admitted { .. } => 0,
            AddProgress::Phase(progress) => u16::from(progress.ordinal).saturating_add(1),
            AddProgress::Indexing => {
                u16::from(CompilePhaseProgress::of(PackageCompilePhase::Discover).ordinal)
                    .saturating_add(1)
            }
        };
        let total = u16::from(CompilePhaseProgress::of(PackageCompilePhase::Locate).total);
        let mut stream = io::stderr();
        let line = format!("{}  {ordinal}/{total} {label}\n", self.coordinate);
        let _ = stream.write_all(line.as_bytes());
    }
}
