//! Typed identity parsed from the engine's coordinate spelling.
//!
//! The engine addresses rows with four closed spellings:
//!
//! | shape | coordinate |
//! |---|---|
//! | package | `/abs/project` |
//! | module | `/abs/project::src/lib.rs` |
//! | declaration | `/abs/project::src/lib.rs:2::ferris` |
//! | semantic | `/abs/project::semantic::<64 hex>::ferris` |
//! | external | `/abs/project::external::<id>` |
//!
//! [`Identity`] keeps the original spelling verbatim so [`Identity::coordinate`]
//! round-trips to the exact bytes the engine accepts, and exposes the parsed
//! parts for display. Parsing is total: an unrecognized spelling becomes
//! [`IdentityShape::Opaque`] with the whole text as its trail, never an error
//! and never a guess.

use crate::language::Language;
use backend_library::{PackageKey, RowId, SymbolKey, encode_id};
use core::fmt;
use core::num::NonZeroU32;

/// Marker separating the project, path, and symbol parts of a coordinate.
const SEPARATOR: &str = "::";

/// Separator rendered between the parts of a readable identity trail.
pub(crate) const TRAIL: &str = " › ";

/// The exact coordinate text the engine accepts for one row.
///
/// This is the value an agent passes back. It is never abbreviated, never
/// wrapped, and never truncated by any renderer in this crate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Coordinate(String);

impl Coordinate {
    /// Retains one coordinate exactly as the engine spelled it.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Returns the exact coordinate text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Coordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One project: its readable name and the absolute root the engine indexed.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectRef {
    name: String,
    root: String,
}

impl ProjectRef {
    /// Derives the readable project name from its absolute root.
    #[must_use]
    pub fn new(root: impl Into<String>) -> Self {
        let root = root.into();
        let name = last_component(&root).to_owned();
        Self { name, root }
    }

    /// Returns the short readable project name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the absolute project root exactly as the engine spelled it.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }
}

impl fmt::Display for ProjectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// One package-relative source path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackagePath(String);

impl PackagePath {
    /// Retains a package-relative path as the engine spelled it.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// Returns the package-relative path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the file name without its directories.
    #[must_use]
    pub fn file_name(&self) -> &str {
        last_component(&self.0)
    }

    /// Returns the lowercase extension, when the path has one.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        name.rsplit_once('.')
            .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
            .map(|(_, extension)| extension.to_ascii_lowercase())
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A one-based source line.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineNumber(NonZeroU32);

impl LineNumber {
    /// Admits a one-based line.
    #[must_use]
    pub const fn new(line: u32) -> Option<Self> {
        match NonZeroU32::new(line) {
            Some(line) => Some(Self(line)),
            None => None,
        }
    }

    /// Returns the one-based line.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for LineNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One `::`-separated segment of a declaration's symbol path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolSegment(String);

impl SymbolSegment {
    /// Retains one symbol path segment.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Returns the segment text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SymbolSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The ordered symbol path of one declaration, outermost segment first.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolTrail(Box<[SymbolSegment]>);

impl SymbolTrail {
    /// Splits one trailing symbol part into its `::`-separated segments.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        Self(
            text.split(SEPARATOR)
                .filter(|segment| !segment.is_empty())
                .map(SymbolSegment::new)
                .collect(),
        )
    }

    /// Returns every segment in declaration order.
    #[must_use]
    pub fn segments(&self) -> &[SymbolSegment] {
        &self.0
    }

    /// Returns the innermost segment: the declaration's own name.
    #[must_use]
    pub fn leaf(&self) -> Option<&SymbolSegment> {
        self.0.last()
    }

    /// Returns whether this trail carries no segment.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Renders the trail with the shared `::` spelling.
    #[must_use]
    pub fn joined(&self) -> String {
        self.0
            .iter()
            .map(SymbolSegment::as_str)
            .collect::<Vec<_>>()
            .join(SEPARATOR)
    }
}

/// An eight-hex abbreviation of a stable key, for display only.
///
/// A tag is deliberately lossy and has no parser: a surface renders it so a
/// reader can tell two rows apart, and an agent that wants to address a row
/// passes back the [`Coordinate`] instead.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyTag([u8; 4]);

impl KeyTag {
    /// Abbreviates a stable 32-byte key.
    #[must_use]
    pub fn from_key(bytes: &[u8; 32]) -> Self {
        let mut head = [0_u8; 4];
        for (slot, byte) in head.iter_mut().zip(bytes.iter()) {
            *slot = *byte;
        }
        Self(head)
    }

    /// Returns the abbreviated bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 4] {
        self.0
    }
}

impl fmt::Display for KeyTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The stable key behind one identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IdentityKey {
    /// A package shelf row.
    Package(PackageKey),
    /// A declaration row.
    Symbol(SymbolKey),
    /// A row whose key the surface never observed.
    Absent,
}

impl IdentityKey {
    /// Returns the display abbreviation of this key.
    #[must_use]
    pub fn tag(self) -> Option<KeyTag> {
        match self {
            Self::Package(key) => Some(KeyTag::from_key(key.as_bytes())),
            Self::Symbol(key) => Some(KeyTag::from_key(key.as_bytes())),
            Self::Absent => None,
        }
    }

    /// Returns the complete stable key as canonical hexadecimal.
    #[must_use]
    pub fn encoded(self) -> Option<String> {
        match self {
            Self::Package(key) => Some(encode_id(key.as_bytes())),
            Self::Symbol(key) => Some(encode_id(key.as_bytes())),
            Self::Absent => None,
        }
    }
}

impl From<RowId> for IdentityKey {
    fn from(id: RowId) -> Self {
        match id {
            RowId::Package(key) => Self::Package(key),
            RowId::Symbol(key) => Self::Symbol(key),
            RowId::Object(_) => Self::Absent,
        }
    }
}

/// Which closed coordinate spelling one identity was parsed from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IdentityShape {
    /// The project root itself.
    Package,
    /// One source file or module inside a project.
    Module,
    /// One declaration at an exact file and line.
    Declaration,
    /// One declaration addressed by its immutable compiler generation.
    Semantic,
    /// One declaration resolved into another package's image.
    External,
    /// A spelling this model does not recognize, retained verbatim.
    Opaque,
}

/// One parsed row identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Identity {
    coordinate: Coordinate,
    shape: IdentityShape,
    project: Option<ProjectRef>,
    path: Option<PackagePath>,
    line: Option<LineNumber>,
    trail: SymbolTrail,
    key: IdentityKey,
}

impl Identity {
    /// Parses one engine coordinate into its typed parts.
    #[must_use]
    pub fn parse(label: &str) -> Self {
        Self::parse_with_key(label, IdentityKey::Absent)
    }

    /// Parses one engine coordinate and binds the stable key of its row.
    #[must_use]
    pub fn parse_with_key(label: &str, key: IdentityKey) -> Self {
        let mut identity = parse_parts(label);
        identity.key = key;
        identity
    }

    /// Parses a coordinate already known to live inside `project`.
    ///
    /// A project root that itself contains `::` cannot be recovered by the
    /// general parser; supplying the known root removes that ambiguity.
    #[must_use]
    pub fn parse_within(label: &str, project: &ProjectRef, key: IdentityKey) -> Self {
        let Some(rest) = label
            .strip_prefix(project.root())
            .and_then(|rest| rest.strip_prefix(SEPARATOR))
        else {
            return Self::parse_with_key(label, key);
        };
        let mut identity = parse_tail(rest);
        identity.coordinate = Coordinate::new(label);
        identity.project = Some(project.clone());
        identity.key = key;
        identity
    }

    /// Returns the exact coordinate the engine accepts for this row.
    #[must_use]
    pub const fn coordinate(&self) -> &Coordinate {
        &self.coordinate
    }

    /// Returns which closed spelling produced this identity.
    #[must_use]
    pub const fn shape(&self) -> IdentityShape {
        self.shape
    }

    /// Returns the owning project, when the coordinate named one.
    #[must_use]
    pub const fn project(&self) -> Option<&ProjectRef> {
        self.project.as_ref()
    }

    /// Returns the package-relative source path, when the coordinate named one.
    #[must_use]
    pub const fn path(&self) -> Option<&PackagePath> {
        self.path.as_ref()
    }

    /// Returns the one-based declaration line, when the coordinate carried one.
    #[must_use]
    pub const fn line(&self) -> Option<LineNumber> {
        self.line
    }

    /// Returns the declaration's symbol path.
    #[must_use]
    pub const fn trail(&self) -> &SymbolTrail {
        &self.trail
    }

    /// Returns the stable key behind this row.
    #[must_use]
    pub const fn key(&self) -> IdentityKey {
        self.key
    }

    /// Returns the declaration's own name, or the project name for a shelf row.
    #[must_use]
    pub fn name(&self) -> &str {
        self.trail.leaf().map_or_else(
            || {
                self.path.as_ref().map_or_else(
                    || self.project.as_ref().map_or("", ProjectRef::name),
                    PackagePath::file_name,
                )
            },
            SymbolSegment::as_str,
        )
    }

    /// Returns the source language implied by this coordinate's file extension.
    #[must_use]
    pub fn language(&self) -> Language {
        self.path
            .as_ref()
            .map_or(Language::Unknown, Language::from_path)
    }

    /// Renders the readable trail, dropping the project when it is `within`.
    #[must_use]
    pub fn trail_within(&self, within: Option<&ProjectRef>) -> String {
        let mut parts = Vec::with_capacity(3);
        if let Some(project) = self.project.as_ref()
            && within.is_none_or(|scope| scope.root() != project.root())
        {
            parts.push(project.name().to_owned());
        }
        if let Some(path) = self.path.as_ref() {
            parts.push(self.line.map_or_else(
                || path.as_str().to_owned(),
                |line| format!("{path}:{line}"),
            ));
        }
        if !self.trail.is_empty() {
            parts.push(self.trail.joined());
        }
        if parts.is_empty() {
            return self.coordinate.as_str().to_owned();
        }
        parts.join(TRAIL)
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.trail_within(None))
    }
}

fn parse_parts(label: &str) -> Identity {
    let coordinate = Coordinate::new(label);
    let Some((project, rest)) = label.split_once(SEPARATOR) else {
        return Identity {
            coordinate,
            shape: IdentityShape::Package,
            project: Some(ProjectRef::new(label)),
            path: None,
            line: None,
            trail: SymbolTrail::default(),
            key: IdentityKey::Absent,
        };
    };
    let mut identity = parse_tail(rest);
    identity.coordinate = coordinate;
    identity.project = Some(ProjectRef::new(project));
    identity
}

/// Parses everything after `<project>::`.
fn parse_tail(rest: &str) -> Identity {
    let mut parts = rest.splitn(2, SEPARATOR);
    let head = parts.next().unwrap_or_default();
    let (shape, path, line, trail) = match (head, parts.next()) {
        ("", None) => (
            IdentityShape::Opaque,
            None,
            None,
            SymbolTrail::default(),
        ),
        ("semantic", Some(tail)) => (
            IdentityShape::Semantic,
            None,
            None,
            SymbolTrail::parse(tail.split_once(SEPARATOR).map_or(tail, |(_, name)| name)),
        ),
        ("external", Some(tail)) => (
            IdentityShape::External,
            None,
            None,
            SymbolTrail::parse(tail),
        ),
        (head, None) => (
            IdentityShape::Module,
            Some(PackagePath::new(head)),
            None,
            SymbolTrail::default(),
        ),
        (head, Some(tail)) => {
            let (path, line) = split_line(head);
            (
                IdentityShape::Declaration,
                Some(PackagePath::new(path)),
                line,
                SymbolTrail::parse(tail),
            )
        }
    };
    Identity {
        coordinate: Coordinate::new(rest),
        shape,
        project: None,
        path,
        line,
        trail,
        key: IdentityKey::Absent,
    }
}

/// Splits `src/lib.rs:2` into its path and one-based line.
///
/// A Windows drive letter (`C:\src\lib.rs`) keeps its colon because the
/// trailing component is not a decimal line number.
fn split_line(head: &str) -> (&str, Option<LineNumber>) {
    let Some((path, digits)) = head.rsplit_once(':') else {
        return (head, None);
    };
    digits
        .parse::<u32>()
        .ok()
        .and_then(LineNumber::new)
        .map_or((head, None), |line| (path, Some(line)))
}

fn last_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
}
