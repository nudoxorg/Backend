//! The launch snapshot (W-Open I2): the pages the window last showed, saved
//! beside `desktop-state.json` so the next launch paints them before any
//! owner answers.
//!
//! The file is a cache, never a source of truth. It is written at idle and
//! on quit (atomically, like the session file); at launch only the restored
//! route's sections are read and decoded, on a thread beside the
//! platform's start. Anything that does not check out (magic, schema, the table's
//! hash, a section's hash, a decode) means the whole file is ignored and
//! renamed `.bad`: it is never trusted in part and never an error.
//!
//! ```text
//! magic "NXSNAP\0\x01" | schema u32 LE | table length u32 LE | sha256(table) [32]
//! table (JSON: root, sections {key, offset, len, sha256})
//! payloads (JSON page models, one per section)
//! ```
//!
//! Its values land at the unserved root. When the owner answers, a
//! snapshot read at the root the owner serves (by the same build) is
//! current as it is; otherwise each value is revalidated quietly and only a
//! different one redraws (`PageStore::seed`, `Landing::Unchanged`).
//!
//! A page is one [`SeedEntry`]: its key and its value are one value, so a
//! search cannot be decoded as Orbit and a symbol cannot be saved under a
//! package's name. A section's key is a [`SectionKey`], its hash a [`Digest`],
//! the owner's cursor its control bytes ([`Control`]) and the build a
//! [`Writer`]: nothing here is a string that stands for something else.

use crate::core::VersionedRoot;
use crate::model::pages::{PackageRef, PageKey, SeedEntry, SymbolRef};
use crate::navigation::Route;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The file's name inside the workspace's data directory.
pub const FILE_NAME: &str = "desktop-snapshot.nxs";

const MAGIC: [u8; 8] = *b"NXSNAP\0\x01";
/// Bump when a saved page model changes meaning without changing its shape
/// (a shape change already fails the decode, which ignores the file).
const SCHEMA: u32 = 2;
const HEADER: usize = 8 + 4 + 4 + 32;
/// Saved pages beyond this many bytes are left out (the route's pages are
/// a few hundred kilobytes; this bounds a pathological page, not a normal
/// one).
const CAP: usize = 8 << 20;

/// A SHA-256 digest, spelled as 64 hex digits in the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Digest([u8; 32]);

impl Digest {
    fn of(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        Self(sha2::Sha256::digest(bytes).into())
    }

    fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 || !hex.is_ascii() {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl serde::Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_hex(&hex).ok_or_else(|| serde::de::Error::custom("not a 64-digit hex digest"))
    }
}

/// The owner's cursor in its own control encoding: compared whole, never
/// turned back into a cursor (`Cursor::encode_control`).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Control(Vec<u8>);

/// Which build read a page: the running executable's size and modification
/// time. A different mapping of the same root is a different page, so a new
/// build never trusts an old build's snapshot as current.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Writer {
    len: u64,
    /// Since the epoch; zero when the build's time cannot be read.
    modified: Duration,
}

impl Writer {
    /// This build (one `stat`, once).
    fn this_build() -> Self {
        static WRITER: OnceLock<Writer> = OnceLock::new();
        *WRITER.get_or_init(|| {
            std::env::current_exe()
                .and_then(std::fs::metadata)
                .map(|meta| Self {
                    len: meta.len(),
                    modified: meta
                        .modified()
                        .ok()
                        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                        .unwrap_or_default(),
                })
                .unwrap_or_default()
        })
    }
}

/// The root a snapshot's pages were read at, and the build that read them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapRoot {
    epoch: u64,
    cursor: Control,
    writer: Writer,
}

impl SnapRoot {
    fn of(root: VersionedRoot) -> Self {
        Self {
            epoch: root.producer_epoch(),
            cursor: Control(root.revision().encode_control().into_vec()),
            writer: Writer::this_build(),
        }
    }

    /// Whether `root`, served now by this build, is the root these pages
    /// were read at: then they are current as they are.
    #[must_use]
    pub fn serves(&self, root: VersionedRoot) -> bool {
        !root.is_unserved() && *self == Self::of(root)
    }
}

/// What a launch reads back: the root and the route's pages.
#[derive(Debug)]
pub struct Seed {
    /// The root the pages were read at.
    pub root: SnapRoot,
    /// The route's pages, in the order asked.
    pub pages: Vec<SeedEntry>,
}

/// What a window keeps across launches: where its pages are saved, and the
/// ones read back for this launch.
#[derive(Debug)]
pub struct Keep {
    /// The workspace's snapshot file.
    pub file: SnapshotFile,
    /// The restored route's pages, when the file held them.
    pub seed: Option<Seed>,
}

/// The pages worth saving for a route: what it shows, the dossier its
/// shelf reads, and the Orbit model the shelf lists the index from.
#[must_use]
pub fn kept_keys(route: &Route) -> Vec<PageKey> {
    let mut keys = super::store::route_keys(route)
        .into_iter()
        .filter(|key| SectionKey::of(key).is_some())
        .collect::<Vec<_>>();
    // The code view draws the declaration's page beside its source.
    if let Route::Symbol(symbol) = route
        && symbol.view == crate::navigation::View::Code
        && let Some(id) = super::store::route_symbol(route)
    {
        keys.push(PageKey::Symbol(id));
    }
    if let Some(package) = super::store::route_package(route).map(PageKey::Package)
        && !keys.contains(&package)
    {
        keys.push(package);
    }
    if !keys.contains(&PageKey::Orbit) {
        keys.push(PageKey::Orbit);
    }
    keys
}

/// The launch snapshot file of one workspace.
#[derive(Clone, Debug)]
pub struct SnapshotFile {
    path: PathBuf,
}

/// Why a file was ignored (said once on stderr, then renamed `.bad`).
#[derive(Debug)]
enum Refusal {
    TooShort,
    NotASnapshot,
    Schema(u32),
    TableLength,
    TableEnd,
    TableHash,
    Table(String),
    Section {
        key: SectionKey,
        fault: SectionFault,
    },
}

/// What is wrong with one section.
#[derive(Debug)]
enum SectionFault {
    PastTheEnd,
    Hash,
    Decode(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => formatter.write_str("shorter than its header"),
            Self::NotASnapshot => formatter.write_str("not a snapshot"),
            Self::Schema(found) => write!(formatter, "schema {found}, this build reads {SCHEMA}"),
            Self::TableLength => formatter.write_str("table length"),
            Self::TableEnd => formatter.write_str("table past the end"),
            Self::TableHash => formatter.write_str("table hash"),
            Self::Table(error) => write!(formatter, "table: {error}"),
            Self::Section { key, fault } => match fault {
                SectionFault::PastTheEnd => write!(formatter, "{key}: past the end"),
                SectionFault::Hash => write!(formatter, "{key}: section hash"),
                SectionFault::Decode(error) => write!(formatter, "{key}: {error}"),
            },
        }
    }
}

impl SnapshotFile {
    /// The snapshot inside `data` (beside `desktop-state.json`).
    #[must_use]
    pub fn in_data(data: &Path) -> Self {
        Self {
            path: data.join(FILE_NAME),
        }
    }

    /// Where it lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the sections for `wanted`. `None` when there is no file, or it
    /// did not check out (then it is renamed `.bad`); keys the file does not
    /// hold are simply absent from the seed.
    #[must_use]
    pub fn read(&self, wanted: &[PageKey]) -> Option<Seed> {
        let reading = Instant::now();
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                super::trace::span("boot.snapshot", reading, "no snapshot");
                return None;
            }
            Err(error) => {
                eprintln!("backend-desktop: read {}: {error}", self.path.display());
                return None;
            }
        };
        match decode(&bytes, wanted) {
            Ok(seed) => {
                super::trace::span(
                    "boot.snapshot",
                    reading,
                    format_args!(
                        "{} of {} wanted, {} bytes",
                        seed.pages.len(),
                        wanted.len(),
                        bytes.len()
                    ),
                );
                Some(seed)
            }
            Err(why) => {
                let bad = self.path.with_extension("bad");
                eprintln!(
                    "backend-desktop: ignored the launch snapshot {} ({why}); kept as {}",
                    self.path.display(),
                    bad.display()
                );
                let _ = std::fs::rename(&self.path, &bad);
                super::trace::span("boot.snapshot", reading, format_args!("ignored: {why}"));
                None
            }
        }
    }

    /// Saves `pages` as read at `root`, atomically. Returns the bytes
    /// written; an unserved root saves nothing.
    ///
    /// # Errors
    /// The atomic write's I/O error.
    pub fn write(&self, root: VersionedRoot, pages: &[SeedEntry]) -> std::io::Result<usize> {
        if root.is_unserved() {
            return Ok(0);
        }
        let bytes = encode(root, pages);
        backend_platform::durable::write_atomic(&self.path, &bytes)?;
        Ok(bytes.len())
    }
}

/// Which page a section holds. Search, health and browsing pages are asked
/// fresh every time: they have no section.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum SectionKey {
    Symbol(SymbolRef),
    Source(SymbolRef),
    Package(PackageRef),
    Orbit,
}

impl SectionKey {
    /// The section `key`'s page is kept in; `None` for a family the snapshot
    /// never keeps.
    fn of(key: &PageKey) -> Option<Self> {
        match key {
            PageKey::Symbol(symbol) => Some(Self::Symbol(symbol.clone())),
            PageKey::Source(symbol) => Some(Self::Source(symbol.clone())),
            PageKey::Package(package) => Some(Self::Package(package.clone())),
            PageKey::Orbit => Some(Self::Orbit),
            PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => None,
        }
    }
}

impl fmt::Display for SectionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol(symbol) => write!(formatter, "symbol {}", symbol.as_str()),
            Self::Source(symbol) => write!(formatter, "source {}", symbol.as_str()),
            Self::Package(package) => write!(formatter, "package {}", package.as_str()),
            Self::Orbit => formatter.write_str("orbit"),
        }
    }
}

impl From<&SeedEntry> for SectionKey {
    fn from(entry: &SeedEntry) -> Self {
        match entry {
            SeedEntry::Symbol(symbol, _) => Self::Symbol(symbol.clone()),
            SeedEntry::Source(symbol, _) => Self::Source(symbol.clone()),
            SeedEntry::Package(package, _) => Self::Package(package.clone()),
            SeedEntry::Orbit(_) => Self::Orbit,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Table {
    root: SnapRoot,
    sections: Vec<Section>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Section {
    key: SectionKey,
    offset: usize,
    len: usize,
    hash: Digest,
}

/// The file's bytes for `pages` read at `root`.
#[must_use]
fn encode(root: VersionedRoot, pages: &[SeedEntry]) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut sections = Vec::new();
    for entry in pages {
        let encoded = match entry {
            SeedEntry::Symbol(_, page) => serde_json::to_vec(page.as_ref()),
            SeedEntry::Source(_, view) => serde_json::to_vec(view.as_ref()),
            SeedEntry::Package(_, dossier) => serde_json::to_vec(dossier.as_ref()),
            SeedEntry::Orbit(model) => serde_json::to_vec(model.as_ref()),
        };
        let Ok(encoded) = encoded else { continue };
        if payload.len() + encoded.len() > CAP {
            continue;
        }
        sections.push(Section {
            key: SectionKey::from(entry),
            offset: payload.len(),
            len: encoded.len(),
            hash: Digest::of(&encoded),
        });
        payload.extend_from_slice(&encoded);
    }
    let table = serde_json::to_vec(&Table {
        root: SnapRoot::of(root),
        sections,
    })
    .unwrap_or_default();
    let mut bytes = Vec::with_capacity(HEADER + table.len() + payload.len());
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&SCHEMA.to_le_bytes());
    bytes.extend_from_slice(&u32::try_from(table.len()).unwrap_or(u32::MAX).to_le_bytes());
    bytes.extend_from_slice(&Digest::of(&table).0);
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(&payload);
    bytes
}

fn decode(bytes: &[u8], wanted: &[PageKey]) -> Result<Seed, Refusal> {
    let header = bytes.get(..HEADER).ok_or(Refusal::TooShort)?;
    if header[..8] != MAGIC {
        return Err(Refusal::NotASnapshot);
    }
    let word = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let schema = word(8);
    if schema != SCHEMA {
        return Err(Refusal::Schema(schema));
    }
    let table_len = usize::try_from(word(12)).map_err(|_| Refusal::TableLength)?;
    let table = bytes
        .get(HEADER..HEADER + table_len)
        .ok_or(Refusal::TableEnd)?;
    if Digest::of(table).0[..] != header[16..48] {
        return Err(Refusal::TableHash);
    }
    let table: Table =
        serde_json::from_slice(table).map_err(|error| Refusal::Table(error.to_string()))?;
    let payload = &bytes[HEADER + table_len..];
    let mut pages = Vec::new();
    for key in wanted {
        let Some(wanted) = SectionKey::of(key) else {
            continue;
        };
        let Some(section) = table.sections.iter().find(|section| section.key == wanted) else {
            continue;
        };
        let fault = |fault| Refusal::Section {
            key: wanted.clone(),
            fault,
        };
        let body = section
            .offset
            .checked_add(section.len)
            .and_then(|end| payload.get(section.offset..end))
            .ok_or_else(|| fault(SectionFault::PastTheEnd))?;
        if Digest::of(body) != section.hash {
            return Err(fault(SectionFault::Hash));
        }
        let entry = entry(wanted.clone(), body)
            .map_err(|error| fault(SectionFault::Decode(error.to_string())))?;
        pages.push(entry);
    }
    Ok(Seed {
        root: table.root,
        pages,
    })
}

/// The page a section's payload holds.
fn entry(key: SectionKey, body: &[u8]) -> Result<SeedEntry, serde_json::Error> {
    Ok(match key {
        SectionKey::Symbol(symbol) => SeedEntry::Symbol(symbol, serde_json::from_slice(body)?),
        SectionKey::Source(symbol) => SeedEntry::Source(symbol, serde_json::from_slice(body)?),
        SectionKey::Package(package) => SeedEntry::Package(package, serde_json::from_slice(body)?),
        SectionKey::Orbit => SeedEntry::Orbit(serde_json::from_slice(body)?),
    })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
