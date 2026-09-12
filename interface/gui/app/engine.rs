//! Defines the resident engine bridge for `interface-gui`.
//! This module owns the two pumps that make the window the resident surface: every command runs
//! off the main thread, and the epoch file is watched so CLI and MCP mutations appear without a
//! restart. Its narrow surface is two channels and one fold.

use std::thread;

use async_channel::{Receiver, Sender, bounded};
use interface_library::{
    AddProgress, Command, CommandId, ExplorePackageName, ExploreQuery, Library, LibraryWatcher,
    Reply,
};

use crate::store::document::PageKey;

/// How often the resident engine re-reads the epoch file, so a package added by the CLI or the MCP
/// server appears in this window without it being told.
const EPOCH_POLL_INTERVAL: core::time::Duration = core::time::Duration::from_millis(1200);

/// Which store one running command owes its reply to.
#[derive(Clone, Debug, PartialEq)]
pub enum Errand {
    /// Replaces every shelf row.
    Shelf,
    /// Replaces the capability report.
    Health,
    /// Runs one compile to its terminal.
    Add {
        /// The package being compiled.
        coordinate: interface_identity::PackageCoordinate,
    },
    /// Removes one package.
    Remove {
        /// The package being removed.
        coordinate: interface_identity::PackageCoordinate,
    },
    /// Opens one page in the reader.
    Page {
        /// The page being opened.
        key: PageKey,
    },
    /// Answers one hover card.
    Card {
        /// The target the card describes.
        key: PageKey,
    },
    /// Replaces the context panel's tree.
    Outline {
        /// The package whose tree is read.
        coordinate: interface_identity::PackageCoordinate,
    },
    /// Replaces the results sheet.
    Search,
    /// Replaces the exploration index page.
    IndexSearch {
        /// The query being searched.
        query: ExploreQuery,
    },
    /// Replaces one package's version rows.
    Versions {
        /// The package whose versions are read.
        name: ExplorePackageName,
    },
    /// Replaces one package's profile.
    Profile {
        /// The package whose profile is read.
        name: ExplorePackageName,
    },
}

impl Errand {
    /// The registry row whose reply this errand expects, so a mismatch is detectable rather than
    /// silently misfiled.
    #[must_use]
    pub const fn command_id(&self) -> CommandId {
        match self {
            Self::Shelf => CommandId::Packages,
            Self::Add { .. } => CommandId::Add,
            Self::Remove { .. } => CommandId::Remove,
            Self::Page { .. } => CommandId::Show,
            Self::Card { .. } => CommandId::Show,
            Self::Outline { .. } => CommandId::Outline,
            Self::Search => CommandId::Search,
            Self::IndexSearch { .. } => CommandId::IndexSearch,
            Self::Versions { .. } => CommandId::PackageVersions,
            Self::Profile { .. } => CommandId::PackageProfile,
            Self::Health => CommandId::Health,
        }
    }
}

/// One unit of work handed to the engine.
#[derive(Debug)]
pub struct EngineRequest {
    /// Where the reply is owed.
    pub errand: Errand,
    /// The closed command to run.
    pub command: Command,
}

/// What the engine tells the window.
#[derive(Debug)]
pub enum EngineEvent {
    /// One progress report from a running compile.
    Progress(AddProgress),
    /// One command's terminal answer.
    Replied {
        /// The errand that was owed.
        errand: Errand,
        /// The engine's reply, unmodified.
        reply: Reply,
    },
    /// The epoch file moved beneath another process's hand.
    ShelfChanged,
}

/// Spawns the resident engine: one thread that runs commands in arrival order and one thread that
/// watches the epoch file. Both stop when the window drops its request sender and the event
/// receiver.
///
/// The engine thread is the process's compile-lock holder: because the GUI is the resident
/// surface, its thread outlives any single CLI or MCP invocation, and those processes see its
/// publications through the same epoch file it watches for theirs.
#[must_use]
pub fn start(library: Library) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    let (request_sender, request_receiver) = bounded::<EngineRequest>(64);
    let (event_sender, event_receiver) = bounded::<EngineEvent>(512);
    if let Some(watcher) = library.watch().ok() {
        let watcher_sender = event_sender.clone();
        thread::spawn(move || watch_loop(watcher, watcher_sender));
    }
    thread::spawn(move || command_loop(library, request_receiver, event_sender));
    (request_sender, event_receiver)
}

fn command_loop(
    library: Library,
    requests: Receiver<EngineRequest>,
    events: Sender<EngineEvent>,
) {
    while let Ok(request) = requests.recv_blocking() {
        let EngineRequest { errand, command } = request;
        let mut progress = {
            let events = events.clone();
            move |event: AddProgress| {
                let _ = events.send_blocking(EngineEvent::Progress(event));
            }
        };
        let reply = library.execute(command, &mut progress);
        if events
            .send_blocking(EngineEvent::Replied { errand, reply })
            .is_err()
        {
            return;
        }
    }
}

fn watch_loop(watcher: LibraryWatcher, events: Sender<EngineEvent>) {
    let mut watcher = watcher;
    loop {
        thread::sleep(EPOCH_POLL_INTERVAL);
        match watcher.poll() {
            Ok(Some(_)) | Err(_) => {
                if events.send_blocking(EngineEvent::ShelfChanged).is_err() {
                    return;
                }
            }
            Ok(None) => {}
        }
    }
}
