//! The fixture world, loaded once for every test that reads it.

use crate::graph::model::{NodeId, Package, World};
use crate::semantics::names::Names;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The pinned fixture (see `fixtures/README.md`): the prototype's world
/// trimmed to the four target pages' neighbourhood at a fixed revision.
pub(super) fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/semantics/tests/fixtures")
}

/// The pinned world.
pub(super) fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let bytes = std::fs::read(fixture_dir().join("world.json")).expect("world.json");
        World::from_json(&bytes).expect("the fixture parses")
    })
}

/// Its name index.
pub(super) fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| Names::new(world()))
}

/// A symbol by its qualified name (`present::glyph::RelationLabel`).
pub(super) fn find(qualified: &str) -> NodeId {
    let w = world();
    let (place, name) = qualified.rsplit_once("::").unwrap_or(("", qualified));
    (0..u32::try_from(w.len()).unwrap_or(0))
        .find(|&i| w.node(i).name.as_ref() == name && w.qual(i).as_ref() == place)
        .unwrap_or_else(|| panic!("no {qualified} in the fixture"))
}

/// Reads a pinned source: `src/registry/<file>` for an external package,
/// `src/repo/<file>` for the workspace.
pub(super) fn read(package: &Package, file: &str) -> Option<Arc<str>> {
    let dir = if package.external { "registry" } else { "repo" };
    std::fs::read_to_string(fixture_dir().join("src").join(dir).join(file)).ok().map(Arc::from)
}
