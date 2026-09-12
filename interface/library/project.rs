//! Defines project behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the project invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Projects: folders of pinned packages, optionally bound to a lockfile they stay in sync with.

use core::{fmt, num::NonZeroU32};
use std::path::{Path, PathBuf};

use interface_core::PackageEcosystem;
use interface_documents::{Count, Text};
use interface_identity::PackageCoordinate;

use crate::{LibraryEpoch, Timestamp};

/// Most folders one library holds.
pub const MAX_PROJECTS: usize = 256;
/// Most members one folder holds.
pub const MAX_PROJECT_MEMBERS: usize = 1024;
/// Longest folder name.
pub const MAX_PROJECT_NAME_BYTES: usize = 64;

/// Stable folder identity, minted by the store and never reused.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectId(NonZeroU32);

impl ProjectId {
    /// Wraps a nonzero identity.
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    /// The identity.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A validated folder name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectName(Box<str>);

/// Exact folder-name admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectNameError {
    /// Nothing but whitespace was supplied.
    Empty,
    /// The text exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
    /// The text carries a control byte or a line break.
    Character,
}

impl ProjectName {
    /// Admits one folder name.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, or control-bearing text.
    pub fn new(text: &str) -> Result<Self, ProjectNameError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ProjectNameError::Empty);
        }
        if trimmed.len() > MAX_PROJECT_NAME_BYTES {
            return Err(ProjectNameError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_PROJECT_NAME_BYTES,
            });
        }
        if trimmed.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(ProjectNameError::Character);
        }
        Ok(Self(trimmed.into()))
    }

    /// The exact name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The accent hue a folder tile is drawn with, one of the palette's chromatic accents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProjectHue {
    /// Warm brown-gold.
    #[default]
    Caramel,
    /// Teal.
    Teal,
    /// Deep green.
    Forest,
    /// Bright green.
    Green,
    /// Red.
    Red,
    /// Olive.
    Olive,
    /// Pale sand.
    Sand,
}

impl ProjectHue {
    /// Every hue in palette order.
    pub const ALL: [Self; 7] = [
        Self::Caramel,
        Self::Teal,
        Self::Forest,
        Self::Green,
        Self::Red,
        Self::Olive,
        Self::Sand,
    ];

    /// The stable word.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Caramel => "caramel",
            Self::Teal => "teal",
            Self::Forest => "forest",
            Self::Green => "green",
            Self::Red => "red",
            Self::Olive => "olive",
            Self::Sand => "sand",
        }
    }

    /// Parses the stable word.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|hue| hue.name() == text)
    }

    /// The hue a fresh folder takes, cycling through the palette by folder ordinal.
    #[must_use]
    pub const fn for_ordinal(ordinal: u32) -> Self {
        match ordinal % 7 {
            0 => Self::Caramel,
            1 => Self::Teal,
            2 => Self::Forest,
            3 => Self::Green,
            4 => Self::Red,
            5 => Self::Olive,
            _ => Self::Sand,
        }
    }
}

/// The lockfile grammars a folder can bind to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockfileKind {
    /// `Cargo.lock`.
    CargoLock,
    /// `package-lock.json`.
    PackageLockJson,
    /// `pnpm-lock.yaml`.
    PnpmLock,
    /// `yarn.lock`.
    YarnLock,
    /// `uv.lock`.
    UvLock,
    /// `poetry.lock`.
    PoetryLock,
    /// `requirements.txt` with pinned `==` rows.
    Requirements,
    /// `go.mod`.
    GoMod,
    /// `pom.xml`.
    PomXml,
    /// `packages.lock.json`.
    PackagesLockJson,
    /// `conan.lock`.
    ConanLock,
    /// `vcpkg.json` with pinned overrides.
    VcpkgJson,
}

impl LockfileKind {
    /// Every grammar in display order.
    pub const ALL: [Self; 12] = [
        Self::CargoLock,
        Self::PackageLockJson,
        Self::PnpmLock,
        Self::YarnLock,
        Self::UvLock,
        Self::PoetryLock,
        Self::Requirements,
        Self::GoMod,
        Self::PomXml,
        Self::PackagesLockJson,
        Self::ConanLock,
        Self::VcpkgJson,
    ];

    /// The file name each grammar lives in.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::CargoLock => "Cargo.lock",
            Self::PackageLockJson => "package-lock.json",
            Self::PnpmLock => "pnpm-lock.yaml",
            Self::YarnLock => "yarn.lock",
            Self::UvLock => "uv.lock",
            Self::PoetryLock => "poetry.lock",
            Self::Requirements => "requirements.txt",
            Self::GoMod => "go.mod",
            Self::PomXml => "pom.xml",
            Self::PackagesLockJson => "packages.lock.json",
            Self::ConanLock => "conan.lock",
            Self::VcpkgJson => "vcpkg.json",
        }
    }

    /// The ecosystem every member parsed from this grammar belongs to.
    #[must_use]
    pub const fn ecosystem(self) -> PackageEcosystem {
        match self {
            Self::CargoLock => PackageEcosystem::Cargo,
            Self::PackageLockJson | Self::PnpmLock | Self::YarnLock => PackageEcosystem::Npm,
            Self::UvLock | Self::PoetryLock | Self::Requirements => PackageEcosystem::Pypi,
            Self::GoMod => PackageEcosystem::Golang,
            Self::PomXml => PackageEcosystem::Maven,
            Self::PackagesLockJson => PackageEcosystem::Nuget,
            Self::ConanLock | Self::VcpkgJson => PackageEcosystem::Generic,
        }
    }

    /// Recognises a lockfile by its file name.
    #[must_use]
    pub fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        Self::ALL.into_iter().find(|kind| kind.file_name() == name)
    }
}

/// One folder's binding to a lockfile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockfileBinding {
    /// Absolute path.
    pub path: Box<Path>,
    /// Grammar.
    pub kind: LockfileKind,
}

impl LockfileBinding {
    /// Binds one path, recognising its grammar by file name.
    ///
    /// # Errors
    ///
    /// Rejects a relative path or an unrecognised file name.
    pub fn new(path: PathBuf) -> Result<Self, ProjectError> {
        if !path.is_absolute() {
            return Err(ProjectError::Lockfile {
                path: path.into_boxed_path(),
                detail: "the lockfile path must be absolute".into(),
            });
        }
        let kind = LockfileKind::detect(&path).ok_or_else(|| ProjectError::UnknownLockfile {
            path: path.clone().into_boxed_path(),
        })?;
        Ok(Self {
            path: path.into_boxed_path(),
            kind,
        })
    }
}

/// One folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    /// Identity.
    pub id: ProjectId,
    /// Name.
    pub name: ProjectName,
    /// Tile hue.
    pub hue: ProjectHue,
    /// Pinned members in insertion order.
    pub members: Box<[PackageCoordinate]>,
    /// Lockfile binding when the folder tracks one.
    pub binding: Option<LockfileBinding>,
    /// When the folder was created.
    pub created_at: Timestamp,
    /// When it was last reconciled with its lockfile.
    pub synced_at: Option<Timestamp>,
}

/// Every folder at one epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Projects {
    /// Rows in creation order.
    pub rows: Box<[Project]>,
    /// Epoch the rows were read at.
    pub epoch: LibraryEpoch,
}

impl Projects {
    /// Finds one folder by selector.
    #[must_use]
    pub fn find(&self, selector: &ProjectSelector) -> Option<&Project> {
        self.rows.iter().find(|row| match selector {
            ProjectSelector::Id(id) => row.id == *id,
            ProjectSelector::Name(name) => row.name == *name,
        })
    }
}

/// How a caller names a folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectSelector {
    /// By identity.
    Id(ProjectId),
    /// By name.
    Name(ProjectName),
}

impl ProjectSelector {
    /// Parses digits as an identity and anything else as a name.
    ///
    /// # Errors
    ///
    /// Returns the exact name refusal.
    pub fn parse(text: &str) -> Result<Self, ProjectNameError> {
        let trimmed = text.trim();
        if let Some(id) = trimmed.parse::<u32>().ok().and_then(NonZeroU32::new) {
            return Ok(Self::Id(ProjectId(id)));
        }
        ProjectName::new(trimmed).map(Self::Name)
    }
}

impl fmt::Display for ProjectSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "{id}"),
            Self::Name(name) => formatter.write_str(name.as_str()),
        }
    }
}

/// One create request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateProject {
    /// Name.
    pub name: ProjectName,
    /// Lockfile to track, when any.
    pub binding: Option<LockfileBinding>,
    /// Tile hue; `None` takes the next in the palette cycle.
    pub hue: Option<ProjectHue>,
}

/// One member add or remove.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberChange {
    /// Which folder.
    pub selector: ProjectSelector,
    /// Which package.
    pub coordinate: PackageCoordinate,
}

/// Whether a sync compiles what it adds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyncCompile {
    /// Only reconcile the member list.
    #[default]
    Never,
    /// Also admit an add for every member not yet on the shelf.
    Missing,
}

/// One sync request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncRequest {
    /// Which folder.
    pub selector: ProjectSelector,
    /// Whether to compile new members.
    pub compile: SyncCompile,
}

/// One member whose pinned version moved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repin {
    /// Before.
    pub from: PackageCoordinate,
    /// After.
    pub to: PackageCoordinate,
}

/// What one sync changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncReport {
    /// The folder as it now stands.
    pub project: Project,
    /// Members the lockfile added.
    pub added: Box<[PackageCoordinate]>,
    /// Members the lockfile no longer pins.
    pub removed: Box<[PackageCoordinate]>,
    /// Members whose version moved.
    pub repinned: Box<[Repin]>,
    /// Members unchanged.
    pub unchanged: Count,
    /// Lockfile rows this build could not read, verbatim.
    pub unreadable: Box<[Text]>,
    /// Members whose add was admitted because of [`SyncCompile::Missing`].
    pub compiles: Box<[PackageCoordinate]>,
}

/// Exact project failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectError {
    /// The project store failed.
    Store {
        /// Bounded description.
        detail: Box<str>,
    },
    /// No folder matches.
    Unknown {
        /// What was asked.
        selector: ProjectSelector,
    },
    /// A folder already carries this name.
    Duplicate {
        /// The name.
        name: ProjectName,
    },
    /// The folder tracks no lockfile, so it cannot sync.
    Unbound {
        /// The folder.
        project: ProjectId,
    },
    /// The lockfile could not be read.
    Lockfile {
        /// Exact path.
        path: Box<Path>,
        /// Bounded description.
        detail: Box<str>,
    },
    /// The file name is not a lockfile this build reads.
    UnknownLockfile {
        /// Exact path.
        path: Box<Path>,
    },
    /// The store or the folder is full.
    Full {
        /// Fixed maximum.
        maximum: usize,
    },
    /// The name was refused.
    Name(ProjectNameError),
}

impl ProjectError {
    /// Stable cause slug, the same word on every surface.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::Store { .. } => "projects-store",
            Self::Unknown { .. } => "project-unknown",
            Self::Duplicate { .. } => "project-duplicate",
            Self::Unbound { .. } => "project-unbound",
            Self::Lockfile { .. } => "lockfile-unreadable",
            Self::UnknownLockfile { .. } => "lockfile-unknown",
            Self::Full { .. } => "projects-full",
            Self::Name(_) => "project-name",
        }
    }

    /// The exact operand that was refused.
    #[must_use]
    pub fn operand(&self) -> String {
        match self {
            Self::Store { .. } | Self::Full { .. } | Self::Name(_) => String::new(),
            Self::Unknown { selector } => selector.to_string(),
            Self::Duplicate { name } => name.to_string(),
            Self::Unbound { project } => project.to_string(),
            Self::Lockfile { path, .. } | Self::UnknownLockfile { path } => {
                path.display().to_string()
            }
        }
    }

    /// One line in the failure's own words.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Store { detail } => format!("the project store failed: {detail}"),
            Self::Unknown { .. } => "no project folder matches".to_owned(),
            Self::Duplicate { .. } => "a project folder already carries this name".to_owned(),
            Self::Unbound { .. } => "this folder tracks no lockfile; bind one to sync".to_owned(),
            Self::Lockfile { detail, .. } => format!("the lockfile could not be read: {detail}"),
            Self::UnknownLockfile { .. } => format!(
                "the file name is not a lockfile this build reads; accepted: {}",
                LockfileKind::ALL.map(LockfileKind::file_name).join(" ")
            ),
            Self::Full { maximum } => format!("the maximum of {maximum} was reached"),
            Self::Name(ProjectNameError::Empty) => "the name was empty".to_owned(),
            Self::Name(ProjectNameError::TooLong { observed, maximum }) => {
                format!("the name has {observed} bytes, maximum {maximum}")
            }
            Self::Name(ProjectNameError::Character) => "the name carries a control byte".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfiles_are_recognised_by_file_name_only() {
        assert_eq!(
            LockfileKind::detect(Path::new("/w/Cargo.lock")),
            Some(LockfileKind::CargoLock)
        );
        assert_eq!(
            LockfileKind::detect(Path::new("/w/pnpm-lock.yaml")).map(LockfileKind::ecosystem),
            Some(PackageEcosystem::Npm)
        );
        assert_eq!(LockfileKind::detect(Path::new("/w/Cargo.toml")), None);
        assert!(matches!(
            LockfileBinding::new(PathBuf::from("relative/Cargo.lock")),
            Err(ProjectError::Lockfile { .. })
        ));
    }

    #[test]
    fn selectors_read_digits_as_identities() {
        assert!(matches!(ProjectSelector::parse("7"), Ok(ProjectSelector::Id(_))));
        assert!(matches!(ProjectSelector::parse("backend"), Ok(ProjectSelector::Name(_))));
        assert_eq!(ProjectSelector::parse("   "), Err(ProjectNameError::Empty));
        assert_eq!(ProjectHue::for_ordinal(8), ProjectHue::Teal);
    }
}
