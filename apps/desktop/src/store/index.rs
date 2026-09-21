//! Derived structure over the live root: which project holds a declaration,
//! what it contains, what sits beside it, and what each file holds.
//! Rebuilt once per admitted revision on the event path, never in a render.
//!
//! Every panel that used to walk all rows on every frame — the outline, the
//! project page, the "which project is open" test on each shelf row — reads
//! from here instead. A ten-thousand-row project costs one rebuild when its
//! revision moves and a hash lookup afterwards, which is the difference
//! between a panel that scrolls and one that stutters.
//!
//! The store is a pure function of the root. It holds no reader state and
//! answers no question the root cannot; it only answers them in constant time.

use super::events::IndexEvent;
use super::workspace::WorkspaceStore;
use backend_library::{DeclarationKind, Row, RowId, SymbolKey, ViewRoot};
use backend_present::{Coordinate, Identity, IdentityKey};
use gpui::{Context, Entity, EventEmitter, Subscription};
use std::collections::HashMap;

/// Where one symbol lives: the project slot and the entry index inside it.
type Locate = HashMap<SymbolKey, (usize, usize)>;

/// Which symbol one exact coordinate names.
type Coordinates = HashMap<String, SymbolKey>;

/// One declaration the root published, with the facts a list needs.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    symbol: SymbolKey,
    identity: Identity,
    kind: Option<DeclarationKind>,
    parent: Option<SymbolKey>,
    signature: Option<String>,
}

impl Entry {
    /// Returns the declaration's stable key.
    pub(crate) const fn symbol(&self) -> SymbolKey {
        self.symbol
    }

    /// Returns the parsed identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the typed kind, when the producer published one.
    pub(crate) const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the containing declaration, when there is one.
    pub(crate) const fn parent(&self) -> Option<SymbolKey> {
        self.parent
    }

    /// Returns the canonical signature text, when there is one.
    pub(crate) fn signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    /// Returns the declaration's own name.
    ///
    /// A file module is named by its stem: the service coordinates it by its
    /// path, but no reader calls a module `components.rs`. Every other row
    /// is named exactly as the producer spelled it.
    pub(crate) fn name(&self) -> &str {
        if self.is_file_module() {
            return self
                .identity
                .path()
                .map_or_else(|| self.identity.name(), backend_present::PackagePath::stem);
        }
        self.identity.name()
    }

    /// Returns whether this row is the module a source file is.
    ///
    /// The service publishes one such row per file: kind `Module`, coordinate
    /// `project::path`, no symbol trail. It is where the file's top-level
    /// declarations hang, and it is drawn as a module, not as a file.
    pub(crate) fn is_file_module(&self) -> bool {
        self.kind == Some(DeclarationKind::Module) && self.identity.trail().is_empty()
    }

    /// Returns the exact coordinate the engine accepts for this row.
    pub(crate) fn coordinate(&self) -> &str {
        self.identity.coordinate().as_str()
    }

    /// Returns the package-relative source path, when the row names one.
    pub(crate) fn path(&self) -> Option<&str> {
        self.identity.path().map(backend_present::PackagePath::as_str)
    }
}

/// The declarations of one source file, in producer order.
#[derive(Clone, Debug)]
pub(crate) struct FileGroup {
    path: String,
    entries: Vec<usize>,
}

impl FileGroup {
    /// Returns the package-relative path.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Returns how many declarations the file holds.
    pub(crate) const fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The declarations of one project, indexed by symbol, container, and file.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProjectIndex {
    root: String,
    entries: Vec<Entry>,
    by_symbol: HashMap<SymbolKey, usize>,
    children: HashMap<SymbolKey, Vec<usize>>,
    top: Vec<usize>,
    files: Vec<FileGroup>,
    names: HashMap<String, usize>,
}

impl ProjectIndex {
    fn new(root: String) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    /// Returns the project coordinate every row in it starts with.
    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    /// Returns one declaration by key.
    pub(crate) fn entry(&self, symbol: SymbolKey) -> Option<&Entry> {
        self.by_symbol.get(&symbol).and_then(|at| self.entries.get(*at))
    }

    /// Returns the declarations no other declaration in this project contains.
    pub(crate) fn top_level(&self) -> impl Iterator<Item = &Entry> {
        self.top.iter().filter_map(|at| self.entries.get(*at))
    }

    /// Returns the declarations one declaration directly contains.
    pub(crate) fn children_of(&self, symbol: SymbolKey) -> impl Iterator<Item = &Entry> {
        self.children
            .get(&symbol)
            .into_iter()
            .flatten()
            .filter_map(|at| self.entries.get(*at))
    }

    /// Returns the declarations that sit beside one declaration.
    ///
    /// Beside means: under the same container, or — for a top-level
    /// declaration — top-level in the same file. The declaration itself is
    /// excluded.
    pub(crate) fn around(&self, symbol: SymbolKey) -> Vec<&Entry> {
        let Some(entry) = self.entry(symbol) else {
            return Vec::new();
        };
        match entry.parent().filter(|parent| self.by_symbol.contains_key(parent)) {
            Some(parent) => self
                .children_of(parent)
                .filter(|other| other.symbol() != symbol)
                .collect(),
            None => self
                .top_level()
                .filter(|other| other.symbol() != symbol && other.path() == entry.path())
                .collect(),
        }
    }

    /// Returns the containment chain from the outermost container inward.
    pub(crate) fn ancestors(&self, symbol: SymbolKey) -> Vec<&Entry> {
        let mut chain = Vec::new();
        let mut cursor = self.entry(symbol).and_then(Entry::parent);
        while let Some(parent) = cursor {
            let Some(entry) = self.entry(parent) else {
                break;
            };
            if chain.len() > 64 {
                break;
            }
            chain.push(entry);
            cursor = entry.parent();
        }
        chain.reverse();
        chain
    }

    /// Returns every source file with the declarations it holds, sorted by path.
    pub(crate) fn files(&self) -> &[FileGroup] {
        &self.files
    }

    /// Resolves a spelled name to the first declaration carrying it.
    ///
    /// Constant time: a source view resolves every identifier on every line
    /// through this, and a scan per word would cost a frame on a long file.
    pub(crate) fn by_name(&self, name: &str) -> Option<&Entry> {
        self.names.get(name).and_then(|at| self.entries.get(*at))
    }

    /// Returns the file modules of this project, in path order.
    pub(crate) fn modules(&self) -> impl Iterator<Item = &Entry> {
        self.top_level().filter(|entry| entry.is_file_module())
    }

    fn push(&mut self, entry: Entry) {
        let at = self.entries.len();
        self.by_symbol.insert(entry.symbol(), at);
        if !entry.is_file_module() {
            self.names.entry(entry.name().to_owned()).or_insert(at);
        }
        self.entries.push(entry);
    }

    fn finish(&mut self) {
        let mut files: HashMap<String, Vec<usize>> = HashMap::new();
        for (at, entry) in self.entries.iter().enumerate() {
            match entry.parent().filter(|parent| self.by_symbol.contains_key(parent)) {
                Some(parent) => self.children.entry(parent).or_default().push(at),
                None => self.top.push(at),
            }
            if let Some(path) = entry.path() {
                files.entry(path.to_owned()).or_default().push(at);
            }
        }
        self.files = files
            .into_iter()
            .map(|(path, entries)| FileGroup { path, entries })
            .collect();
        self.files.sort_by(|left, right| left.path.cmp(&right.path));
    }
}

/// Every project's declarations, rebuilt when the root moves.
pub(crate) struct IndexStore {
    version: [u8; 32],
    projects: Vec<ProjectIndex>,
    locate: Locate,
    coordinates: Coordinates,
    /// Held, not read: dropping it would stop the rebuilds.
    _feed: Subscription,
}

impl EventEmitter<IndexEvent> for IndexStore {}

impl IndexStore {
    /// Builds the index from the workspace's current root and follows it.
    pub(crate) fn new(workspace: &Entity<WorkspaceStore>, cx: &mut Context<Self>) -> Self {
        let feed = cx.observe(workspace, |this, store, cx| {
            let changed = this.absorb(store.read(cx).root());
            if changed {
                cx.emit(IndexEvent::Rebuilt);
                cx.notify();
            }
        });
        let mut store = Self {
            version: [0; 32],
            projects: Vec::new(),
            locate: HashMap::new(),
            coordinates: HashMap::new(),
            _feed: feed,
        };
        store.absorb(workspace.read(cx).root());
        store
    }

    /// Returns the index for one project coordinate.
    pub(crate) fn project(&self, root: &str) -> Option<&ProjectIndex> {
        self.projects.iter().find(|project| project.root() == root)
    }

    /// Returns how many projects the current admitted revision contains.
    pub(crate) fn project_count(&self) -> usize {
        self.projects.len()
    }

    /// Returns how many declarations the current admitted revision contains.
    pub(crate) fn declaration_count(&self) -> usize {
        self.projects.iter().map(|project| project.entries.len()).sum()
    }

    /// Returns the project one declaration belongs to.
    pub(crate) fn project_of(&self, symbol: SymbolKey) -> Option<&ProjectIndex> {
        self.locate
            .get(&symbol)
            .and_then(|(project, _)| self.projects.get(*project))
    }

    /// Returns one declaration by key, from whichever project holds it.
    pub(crate) fn entry(&self, symbol: SymbolKey) -> Option<&Entry> {
        self.locate.get(&symbol).and_then(|(project, at)| {
            self.projects
                .get(*project)
                .and_then(|project| project.entries.get(*at))
        })
    }

    /// Returns the exact coordinate of one declaration, when it is on the shelf.
    pub(crate) fn coordinate_of(&self, symbol: SymbolKey) -> Option<&str> {
        self.entry(symbol).map(Entry::coordinate)
    }

    /// Returns the declaration one exact coordinate names, in constant time.
    pub(crate) fn symbol_for(&self, coordinate: &str) -> Option<SymbolKey> {
        self.coordinates.get(coordinate).copied()
    }

    /// Resolves a declaration name to a coordinate and key, for signature links.
    ///
    /// The match is by spelled name and nothing else, which is exactly what
    /// [`backend_present::Resolved::ByName`] claims: a name that happens to
    /// match a declaration on this shelf, never a proven semantic edge. A
    /// name is looked for in `near` first, so `Foo` inside one project
    /// resolves to that project's `Foo` before any other's.
    pub(crate) fn resolve_name(
        &self,
        name: &str,
        near: Option<&str>,
    ) -> Option<(Coordinate, IdentityKey)> {
        let preferred = near.and_then(|root| self.project(root));
        preferred
            .and_then(|project| project.by_name(name))
            .or_else(|| self.projects.iter().find_map(|project| project.by_name(name)))
            .map(|entry| {
                (
                    Coordinate::new(entry.coordinate()),
                    IdentityKey::Symbol(entry.symbol()),
                )
            })
    }

    /// Returns whether the index describes this exact root already.
    fn is_current(&self, root: &ViewRoot) -> bool {
        self.version == *root.version().as_bytes() && !self.projects.is_empty()
    }

    /// Rebuilds from a root; returns whether anything changed.
    fn absorb(&mut self, root: &ViewRoot) -> bool {
        if self.is_current(root) {
            return false;
        }
        self.version = *root.version().as_bytes();
        let (projects, locate) = build(root.rows());
        self.coordinates = projects
            .iter()
            .flat_map(|project| project.entries.iter())
            .map(|entry| (entry.coordinate().to_owned(), entry.symbol()))
            .collect();
        self.projects = projects;
        self.locate = locate;
        true
    }
}

fn build(rows: &[Row]) -> (Vec<ProjectIndex>, Locate) {
    let mut projects: Vec<ProjectIndex> = Vec::new();
    let mut slots: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let RowId::Symbol(symbol) = row.id else {
            continue;
        };
        let identity = Identity::parse_with_key(&row.label, IdentityKey::Symbol(symbol));
        let Some(root) = identity.project().map(|project| project.root().to_owned()) else {
            continue;
        };
        let slot = *slots.entry(root.clone()).or_insert_with(|| {
            projects.push(ProjectIndex::new(root));
            projects.len().saturating_sub(1)
        });
        let Some(project) = projects.get_mut(slot) else {
            continue;
        };
        project.push(Entry {
            symbol,
            identity,
            kind: row.kind,
            parent: row.parent,
            signature: row.signature.clone(),
        });
    }
    for project in &mut projects {
        project.finish();
    }
    projects.sort_by(|left, right| left.root.cmp(&right.root));
    let locate = locate(&projects);
    (projects, locate)
}

/// Keys every symbol by its project slot and entry index, once the projects
/// are in their final order.
fn locate(projects: &[ProjectIndex]) -> Locate {
    let mut locate = HashMap::new();
    for (slot, project) in projects.iter().enumerate() {
        for (at, entry) in project.entries.iter().enumerate() {
            locate.insert(entry.symbol(), (slot, at));
        }
    }
    locate
}

/// Orders kinds the way a reader expects a table of contents: containers,
/// then types, then callables, then values.
pub(crate) const fn kind_rank(kind: Option<DeclarationKind>) -> u8 {
    match kind {
        Some(DeclarationKind::Module) => 0,
        Some(DeclarationKind::Struct) => 1,
        Some(DeclarationKind::Class) => 2,
        Some(DeclarationKind::Enum) => 3,
        Some(DeclarationKind::Union) => 4,
        Some(DeclarationKind::Interface) => 5,
        Some(DeclarationKind::Trait) => 6,
        Some(DeclarationKind::Type) => 7,
        Some(DeclarationKind::Constructor) => 8,
        Some(DeclarationKind::Function) => 9,
        Some(DeclarationKind::Method) => 10,
        Some(DeclarationKind::Macro) => 11,
        Some(DeclarationKind::Property) => 12,
        Some(DeclarationKind::Field) => 13,
        Some(DeclarationKind::Variant) => 14,
        Some(DeclarationKind::Constant) => 15,
        Some(DeclarationKind::Variable) => 16,
        Some(DeclarationKind::Import) => 17,
        Some(DeclarationKind::Unknown) | None => 18,
    }
}
