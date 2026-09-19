//! Opening scenes: the window, already doing the thing worth looking at.
//!
//! Design review and screenshots need the window in states a reader reaches by
//! typing — the palette dropped, a search sheet with its coverage strip, the
//! add flow answering a bad coordinate, a hover card anchored over a member.
//! macOS accessibility cannot see a GPUI window, so those states cannot be
//! driven from outside the process; they have to be asked for from inside it.
//!
//! What this module deliberately is *not* is a fixture set. There is no fake
//! shelf here, no invented page, no hand-written signature. A scene performs
//! the same store calls a keystroke performs, against whatever the live
//! service actually published — so a screenshot taken this way is evidence
//! about the real product rather than a picture of a mock. If the engine has
//! nothing indexed, the scene shows the honest empty state, which is itself
//! worth looking at.
//!
//! Scenes are applied once, on the first frame where the shelf has settled,
//! because a scene that fired before the first revision arrived would only
//! ever photograph the loading state.

use crate::store::document::{Subject, Target};
use crate::views::workspace::Workspace;
use backend_library::RowId;
use gpui::{Context, Window, point, px};

/// The environment variable that names the scene to open in.
pub(crate) const SCENE_ENV: &str = "BACKEND_DESKTOP_PREVIEW";

/// The environment variable that picks which declaration a reading scene opens.
///
/// Its value is matched against the end of each published coordinate, so
/// `glyph.rs:136::RelationLabel` opens that page without the project root.
pub(crate) const COORDINATE_ENV: &str = "BACKEND_DESKTOP_PREVIEW_COORDINATE";

/// One state the window can be asked to open in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scene {
    /// The first declaration on the shelf, open in the reader.
    Reading,
    /// The omnibar in command-palette mode over the whole registry.
    Palette,
    /// The omnibar searching, with its lane coverage strip.
    Search,
    /// The library panel's add flow, answering a coordinate it cannot accept.
    Add,
    /// A page open with a hover card anchored over its first member.
    Hover,
    /// The first project's page.
    Project,
    /// A coordinate nothing answers to, rendered as a fault.
    Fault,
    /// A real index request for a package this deployment cannot reach.
    Index,
    /// The settings sheet on its agents page.
    Settings,
    /// Three pages opened as a branch, to show the tab tree.
    Tree,
    /// The palette holding a command with an argument.
    Command,
    /// A page with its source sheet open over it.
    Source,
    /// The home page with the explore region's first catalog page loaded.
    Browse,
    /// One registry package's page, on a coordinate the demo shelf holds.
    Package,
}

impl Scene {
    /// Returns the scene named by the environment, when one is named.
    pub(crate) fn from_env() -> Option<Self> {
        Self::parse(&std::env::var(SCENE_ENV).ok()?)
    }

    /// Parses one scene name.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "reading" => Some(Self::Reading),
            "palette" => Some(Self::Palette),
            "search" => Some(Self::Search),
            "add" => Some(Self::Add),
            "hover" => Some(Self::Hover),
            "project" => Some(Self::Project),
            "fault" => Some(Self::Fault),
            "index" => Some(Self::Index),
            "settings" => Some(Self::Settings),
            "tree" => Some(Self::Tree),
            "command" => Some(Self::Command),
            "source" => Some(Self::Source),
            "browse" => Some(Self::Browse),
            "package" => Some(Self::Package),
            _ => None,
        }
    }

    /// Returns the stable name this scene is asked for by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Palette => "palette",
            Self::Search => "search",
            Self::Add => "add",
            Self::Hover => "hover",
            Self::Project => "project",
            Self::Fault => "fault",
            Self::Index => "index",
            Self::Settings => "settings",
            Self::Tree => "tree",
            Self::Command => "command",
            Self::Source => "source",
            Self::Browse => "browse",
            Self::Package => "package",
        }
    }

    /// Returns whether this scene needs a declaration on the shelf to be worth opening.
    const fn needs_rows(self) -> bool {
        matches!(self, Self::Reading | Self::Hover | Self::Project | Self::Tree | Self::Source)
    }
}

/// The text a search scene types, so the sheet has a coverage strip to state.
const SEARCH_TERM: &str = "e";

/// A coordinate no revision answers to, for the fault scene.
const MISSING: &str = "/nowhere::src/lib.rs:1::absent";

/// A coordinate the add flow must refuse, for the validation scene.
const BAD_COORDINATE: &str = "serde@9.9.9";

/// A canonical coordinate the add flow accepts and submits.
const PINNED_COORDINATE: &str = "pkg:cargo/memchr@2.7.4";

/// Where a hover card is anchored when no pointer placed it.
const HOVER_ANCHOR: (f32, f32) = (520.0, 470.0);

/// Applies one scene to a freshly built window.
///
/// Returns whether the scene was applied. A scene that needs rows waits for
/// them rather than photographing an empty shelf it could have had.
pub(crate) fn stage(
    workspace: &mut Workspace,
    scene: Scene,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    if scene.needs_rows() && first_declaration(workspace, cx).is_none() {
        return false;
    }
    match scene {
        Scene::Palette => workspace.set_field(">".to_owned(), cx),
        Scene::Search => workspace.set_field(SEARCH_TERM.to_owned(), cx),
        Scene::Add => stage_add(workspace, window, cx),
        Scene::Reading => stage_reading(workspace, cx),
        Scene::Hover => stage_hover(workspace, cx),
        Scene::Project => stage_project(workspace, cx),
        Scene::Fault => stage_fault(workspace, cx),
        Scene::Index => stage_index(workspace, window, cx),
        Scene::Settings => workspace.preview_settings(cx),
        Scene::Tree => stage_tree(workspace, cx),
        Scene::Command => workspace.set_field("> show Duke".to_owned(), cx),
        Scene::Source => {
            stage_reading(workspace, cx);
            workspace.preview_source(cx);
        }
        Scene::Browse => workspace.preview_browse(cx),
        Scene::Package => workspace.open_package(PINNED_COORDINATE.to_owned(), Target::Here, cx),
    }
    cx.notify();
    true
}

fn stage_add(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.begin_add(window, cx);
    workspace.fill_coordinate(BAD_COORDINATE, cx);
    workspace.submit_preview_coordinate(cx);
}

fn stage_reading(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    if let Some((symbol, coordinate)) = first_declaration(workspace, cx) {
        workspace.open_subject(Subject::Declaration { symbol, coordinate }, Target::Here, cx);
    }
}

fn stage_hover(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    stage_reading(workspace, cx);
    let Some((symbol, coordinate)) = second_declaration(workspace, cx) else {
        return;
    };
    let anchor = point(px(HOVER_ANCHOR.0), px(HOVER_ANCHOR.1));
    workspace.preview_hover(symbol, &coordinate, anchor, cx);
}

fn stage_tree(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let coordinate = workspace
        .preview_shelf(cx)
        .entries()
        .first()
        .map(|entry| entry.identity().coordinate().as_str().to_owned());
    if let Some(coordinate) = coordinate {
        workspace.open_project(coordinate, cx);
    }
    for (at, target) in [(0, Target::Child), (1, Target::Child), (2, Target::Background)] {
        if let Some((symbol, coordinate)) = declaration_at(workspace, cx, at) {
            workspace.open_subject(Subject::Declaration { symbol, coordinate }, target, cx);
        }
    }
}

fn stage_project(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let coordinate = workspace
        .preview_shelf(cx)
        .entries()
        .first()
        .map(|entry| entry.identity().coordinate().as_str().to_owned());
    if let Some(coordinate) = coordinate {
        workspace.open_project(coordinate, cx);
    }
}

fn stage_fault(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    workspace.open_project(MISSING.to_owned(), cx);
}

/// Submits one real index request, so the shelf row shows the real outcome.
///
/// Nothing about the result is arranged: the engine either accepts the intent
/// and the row progresses, or it refuses and the row carries the refusal. Both
/// are worth looking at, and which one happens is the engine's answer, not
/// this module's.
fn stage_index(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.begin_add(window, cx);
    workspace.fill_coordinate(PINNED_COORDINATE, cx);
    workspace.submit_preview_coordinate(cx);
}

/// Returns the first declaration the live root published, if it published one.
fn first_declaration(
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> Option<(backend_library::SymbolKey, String)> {
    let wanted = std::env::var(COORDINATE_ENV).ok().filter(|text| !text.is_empty());
    match wanted {
        Some(suffix) => workspace
            .preview_root(cx)
            .rows()
            .iter()
            .find_map(|row| match row.id {
                RowId::Symbol(symbol) if row.label.ends_with(&suffix) => {
                    Some((symbol, row.label.clone()))
                }
                _ => None,
            }),
        None => declaration_at(workspace, cx, 0),
    }
}

fn second_declaration(
    workspace: &Workspace,
    cx: &Context<Workspace>,
) -> Option<(backend_library::SymbolKey, String)> {
    declaration_at(workspace, cx, 1).or_else(|| declaration_at(workspace, cx, 0))
}

fn declaration_at(
    workspace: &Workspace,
    cx: &Context<Workspace>,
    at: usize,
) -> Option<(backend_library::SymbolKey, String)> {
    workspace
        .preview_root(cx)
        .rows()
        .iter()
        .filter_map(|row| match row.id {
            RowId::Symbol(symbol) => Some((symbol, row.label.clone())),
            RowId::Package(_) | RowId::Object(_) => None,
        })
        .nth(at)
}
