//! The launch snapshot (W-Open I2): the pages the window last showed, saved
//! beside `desktop-state.json` so the next launch paints them before any
//! owner answers.
//!
//! The file is a cache, never a source of truth. It is written at idle and
//! on quit (atomically, like the session file); at launch only the restored
//! route's sections are read and decoded, on a thread beside the
//! platform's start. Anything that does not check out (magic, schema, the table's
//! hash, every section's hash, a requested decode) means the whole file is
//! ignored. The observed bytes are preserved as `.bad` without moving a path
//! that another writer may have replaced since the read.
//!
//! ```text
//! magic "NXSNAP\0\x01" | schema u32 LE | table length u32 LE | sha256(table) [32]
//! table (JSON: root, sections {key, offset, len, sha256})
//! payloads (JSON page models, one per section)
//! ```
//!
//! All restored values remain unserved. Matching root, build and hashes
//! prove historical integrity, never current read authority. Only a fresh
//! owner reply may promote a page; an equal reply preserves its content
//! stamp (`Landing::Unchanged`). Closed display projections contain bounded
//! words, exact destination/scope claims and historical observation bytes,
//! without callbacks, selected IR heads or source/editor capabilities.
//! Private platform IO protects both the cache and preserved diagnostics.
//!
//! A page is one [`SeedEntry`]: its key and its value are one value, so a
//! search cannot be decoded as Orbit and a symbol cannot be saved under a
//! package's name. A section's key is a [`SectionKey`], its hash a [`Digest`],
//! the owner's cursor its control bytes ([`Control`]) and the build a
//! [`Writer`]: nothing here is a string that stands for something else.

use crate::core::VersionedRoot;
use crate::model::pages::{PackageRef, PageKey, SeedEntry, SymbolRef};
use crate::navigation::Route;
use crate::model::retained_display::{DisplayAddress, RetainedDisplayWire};
mod admitted;
pub(crate) use admitted::RetainedDisplay;
mod capture;
pub(super) use capture::DisplayCapture;
use backend_platform::durable::BoundedWriter;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

/// The file's name inside the workspace's data directory.
pub const FILE_NAME: &str = "desktop-snapshot.nxs";

const MAGIC: [u8; 8] = *b"NXSNAP\0\x01";
/// Bump when a saved page model changes meaning without changing its shape
/// (a shape change already fails the decode, which ignores the file).
const SCHEMA: u32 = 4;
const HEADER: usize = 8 + 4 + 4 + 32;
/// Saved pages beyond this many bytes are left out (the route's pages are
/// a few hundred kilobytes; this bounds a pathological page, not a normal
/// one).
const CAP: usize = 8 << 20;
const TABLE_CAP: usize = 256 << 10;
const SECTION_CAP: usize = 64;
const DISPLAY_SECTION_CAP: usize = 3 << 20;
const FILE_CAP: usize = HEADER + TABLE_CAP + CAP;

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

/// Which build mapped a page: the exact executable bytes. The fingerprint is
/// prepared on the launch reader, never by the UI's root comparison. Unknown
/// identity always requires a worker read; two failed probes never match.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Writer {
    executable: Option<Digest>,
}

static WRITER: OnceLock<Writer> = OnceLock::new();

impl Writer {
    /// The fingerprint already prepared off the UI thread.
    fn this_build() -> Self {
        WRITER.get().copied().unwrap_or_default()
    }

    /// Called only by the launch read worker. The executable is streamed once
    /// through one held regular-file handle with a fixed 64KiB scratch buffer.
    fn prepare_this_build() {
        WRITER.get_or_init(|| Self {
            executable: fingerprint_running_executable(Self::prepare_this_build).ok(),
        });
    }
}

fn fingerprint_running_executable(anchor: fn()) -> io::Result<Digest> {
    let mut executable = backend_platform::executable_identity::open_running_executable(anchor)?;
    executable.with_verified_read(fingerprint_file)
}

/// A test helper for comparing arbitrary file contents. Production always
/// fingerprints the platform-verified running-image handle above.
#[cfg(test)]
fn fingerprint_executable(path: &Path) -> io::Result<Digest> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("executable has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("executable has no Unicode name"))?;
    let directory =
        backend_platform::directory::DirectoryCapability::open_read_only_source(parent)?;
    let mut file = directory.open_file_read(name)?;
    fingerprint_file(&mut file)
}

fn fingerprint_file(file: &mut File) -> io::Result<Digest> {
    use sha2::Digest as _;
    use std::io::Read as _;

    const MAX_EXECUTABLE_BYTES: u64 = 512 << 20;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other("executable is not a regular file"));
    }
    if before.len() > MAX_EXECUTABLE_BYTES {
        return Err(io::Error::other(
            "executable exceeds fingerprint byte limit",
        ));
    }
    let before_modified = before.modified()?;
    #[cfg(unix)]
    let before_identity = {
        use std::os::unix::fs::MetadataExt as _;
        (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec(),
        )
    };
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"nudox.desktop.mapping.executable.v1\0");
    let mut scratch = [0; 64 << 10];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut scratch)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_EXECUTABLE_BYTES {
            return Err(io::Error::other(
                "executable grew beyond fingerprint byte limit",
            ));
        }
        hasher.update(&scratch[..count]);
    }
    let after = file.metadata()?;
    #[cfg(unix)]
    let identity_changed = {
        use std::os::unix::fs::MetadataExt as _;
        before_identity != (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
    };
    #[cfg(not(unix))]
    let identity_changed = false;
    if total != before.len()
        || after.len() != before.len()
        || after.modified()? != before_modified
        || identity_changed
    {
        return Err(io::Error::other(
            "executable changed during fingerprint read",
        ));
    }
    Ok(Digest(hasher.finalize().into()))
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
        Self::with_writer(root, Writer::this_build())
    }

    fn with_writer(root: VersionedRoot, writer: Writer) -> Self {
        Self {
            epoch: root.producer_epoch(),
            cursor: Control(root.revision().encode_control().into_vec()),
            writer,
        }
    }

    /// Whether `root`, served now by this build, is the root these pages
    /// were read at. This is diagnostic equality, never current read authority.
    #[must_use]
    pub fn serves(&self, root: VersionedRoot) -> bool {
        self.serves_with_writer(root, Writer::this_build())
    }

    fn serves_with_writer(&self, root: VersionedRoot, writer: Writer) -> bool {
        !root.is_unserved()
            && self.writer.executable.is_some()
            && writer.executable.is_some()
            && *self == Self::with_writer(root, writer)
    }
}

/// What a launch reads back: the root and the route's pages.
#[derive(Debug)]
pub struct Seed {
    /// The root the pages were read at.
    pub root: SnapRoot,
    /// The route's pages, in the order asked.
    pub pages: Vec<SeedEntry>,
    /// Display-only projections never enter a current page slot.
    pub(crate) displays: Vec<Arc<RetainedDisplay>>,
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
    namespace: Option<Digest>,
}

/// Why a file was ignored.
#[derive(Debug)]
enum Refusal {
    TooShort,
    TooLarge,
    NotASnapshot,
    Schema(u32),
    TableLength,
    TableEnd,
    TableHash,
    Table(String),
    Sections,
    Namespace,
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
    Layout,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => formatter.write_str("shorter than its header"),
            Self::TooLarge => formatter.write_str("larger than its byte limit"),
            Self::NotASnapshot => formatter.write_str("not a snapshot"),
            Self::Schema(found) => write!(formatter, "schema {found}, this build reads {SCHEMA}"),
            Self::TableLength => formatter.write_str("table length"),
            Self::TableEnd => formatter.write_str("table past the end"),
            Self::TableHash => formatter.write_str("table hash"),
            Self::Table(error) => write!(formatter, "table: {error}"),
            Self::Sections => formatter.write_str("too many sections or duplicate section keys"),
            Self::Namespace => formatter.write_str("another private workspace"),
            Self::Section { key, fault } => match fault {
                SectionFault::PastTheEnd => write!(formatter, "{key}: past the end"),
                SectionFault::Hash => write!(formatter, "{key}: section hash"),
                SectionFault::Decode(error) => write!(formatter, "{key}: {error}"),
                SectionFault::Layout => write!(formatter, "{key}: noncanonical section layout"),
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
            namespace: data.is_absolute().then(|| backend_platform::NativePath::from_path(data).ok())
                .flatten().map(|path| Digest::of(path.key().as_bytes())),
        }
    }

    /// Where it lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the sections for `wanted`. `None` when there is no file, or it
    /// did not check out; keys the file does not
    /// hold are simply absent from the seed.
    #[must_use]
    pub fn read(&self, wanted: &[PageKey]) -> Option<Seed> { self.read_selected(wanted, None) }

    /// Select only the restored exact destination; table bytes never create a route.
    pub(crate) fn read_route(&self, route: &Route) -> Option<Seed> {
        self.read_selected(&kept_keys(route), Some(route))
    }

    fn read_selected(&self, wanted: &[PageKey], route: Option<&Route>) -> Option<Seed> {
        let namespace = self.namespace?;
        Writer::prepare_this_build();
        let reading = Instant::now();
        let bytes = match read_private_bounded(&self.path, FILE_CAP) {
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
        match decode_selected(&bytes, wanted, Some(namespace), route) {
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
                    "backend-desktop: ignored the launch snapshot {} ({why}); preserving observed bytes as {}",
                    self.path.display(),
                    bad.display()
                );
                // Keep the observed bytes privately, without renaming a
                // pathname that another writer may already have replaced.
                if !bad.exists() {
                    let _ = backend_platform::durable::write_private_atomic(&bad, &bytes);
                }
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
        self.write_displays(root, pages, &[])
    }

    pub(super) fn write_displays(&self, root: VersionedRoot, pages: &[SeedEntry], displays: &[Arc<RetainedDisplay>]) -> io::Result<usize> {
        if root.is_unserved() { return Ok(0); }
        let namespace = self.namespace.ok_or_else(|| io::Error::other("snapshot has no exact workspace namespace"))?;
        let bytes = encode_full(root, pages, Writer::this_build(), Some(namespace), displays)?;
        backend_platform::durable::write_private_atomic(&self.path, &bytes)?;
        Ok(bytes.len())
    }
}

fn read_private_bounded(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let file = backend_platform::durable::open_private_read(path)?;
    let length = usize::try_from(file.metadata()?.len()).map_err(|_| io::Error::other("snapshot size overflow"))?;
    if length > maximum { return Err(io::Error::other("snapshot exceeds byte budget")); }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(io::Error::other)?;
    file.take((maximum + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > maximum { return Err(io::Error::other("snapshot grew beyond byte budget")); }
    Ok(bytes)
}

/// Which page a section holds. Search, health and browsing pages are asked
/// fresh every time: they have no section.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum SectionKey {
    Symbol(AddressClaim),
    Source(AddressClaim),
    Package(AddressClaim),
    Orbit,
    Display(DisplayAddress),
}

/// Cache-table bytes, never an admitted read address. The original release
/// tree is part of identity even though page-model serde omits its provenance.
/// A decoded claim can only match a key the current route already requested.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct AddressClaim {
    coordinate: Arc<str>,
    origin_claim: Option<Arc<str>>,
}

impl AddressClaim {
    fn symbol(symbol: &SymbolRef) -> Self {
        Self {
            coordinate: Arc::from(symbol.as_str()),
            origin_claim: symbol.release_origin().map(Arc::from),
        }
    }

    fn package(package: &PackageRef) -> Self {
        Self {
            coordinate: Arc::from(package.as_str()),
            origin_claim: package.release_origin().map(Arc::from),
        }
    }
}

impl SectionKey {
    /// The section `key`'s page is kept in; `None` for a family the snapshot
    /// never keeps.
    fn of(key: &PageKey) -> Option<Self> {
        match key {
            PageKey::Symbol(symbol) => Some(Self::Symbol(AddressClaim::symbol(symbol))),
            PageKey::Source(symbol) => Some(Self::Source(AddressClaim::symbol(symbol))),
            PageKey::Package(package) => Some(Self::Package(AddressClaim::package(package))),
            PageKey::Orbit => Some(Self::Orbit),
            PageKey::CargoSource(_) | PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => None,
        }
    }
}

impl fmt::Display for SectionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol(symbol) => write!(formatter, "symbol {}", symbol.coordinate),
            Self::Source(symbol) => write!(formatter, "source {}", symbol.coordinate),
            Self::Package(package) => write!(formatter, "package {}", package.coordinate),
            Self::Orbit => formatter.write_str("orbit"),
            Self::Display(_) => formatter.write_str("retained display"),
        }
    }
}

impl From<&SeedEntry> for SectionKey {
    fn from(entry: &SeedEntry) -> Self {
        match entry {
            SeedEntry::Symbol(symbol, _) => Self::Symbol(AddressClaim::symbol(symbol)),
            SeedEntry::Source(symbol, _) => Self::Source(AddressClaim::symbol(symbol)),
            SeedEntry::Package(package, _) => Self::Package(AddressClaim::package(package)),
            SeedEntry::Orbit(_) => Self::Orbit,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Table {
    namespace: Option<Digest>,
    root: SnapRoot,
    #[serde(deserialize_with = "bounded_sections")]
    sections: Vec<Section>,
}

fn bounded_sections<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Section>, D::Error> {
    struct Sections;
    impl<'de> serde::de::Visitor<'de> for Sections {
        type Value = Vec<Section>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "at most {SECTION_CAP} snapshot sections")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut sections =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(SECTION_CAP));
            while let Some(section) = sequence.next_element()? {
                if sections.len() == SECTION_CAP {
                    return Err(serde::de::Error::custom("too many snapshot sections"));
                }
                sections.push(section);
            }
            Ok(sections)
        }
    }
    deserializer.deserialize_seq(Sections)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Section {
    key: SectionKey,
    offset: usize,
    len: usize,
    hash: Digest,
}

/// The file's bytes for `pages` read at `root`.
fn encode(root: VersionedRoot, pages: &[SeedEntry]) -> io::Result<Vec<u8>> {
    encode_with_writer(root, pages, Writer::this_build())
}

fn encode_with_writer(
    root: VersionedRoot,
    pages: &[SeedEntry],
    writer: Writer,
) -> io::Result<Vec<u8>> {
    encode_full(root, pages, writer, None, &[])
}

fn encode_full(root: VersionedRoot, pages: &[SeedEntry], writer: Writer, namespace: Option<Digest>, displays: &[Arc<RetainedDisplay>]) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    let mut sections = Vec::new();
    for entry in pages {
        if sections.len() == SECTION_CAP {
            break;
        }
        let key = SectionKey::from(entry);
        if sections.iter().any(|section: &Section| section.key == key) {
            continue;
        }
        let offset = payload.len();
        let writer = BoundedWriter::new(&mut payload, CAP)?;
        let encoded = match entry {
            SeedEntry::Symbol(_, page) => serde_json::to_writer(writer, page.as_ref()),
            SeedEntry::Source(_, view) => serde_json::to_writer(writer, view.as_ref()),
            SeedEntry::Package(_, dossier) => serde_json::to_writer(writer, dossier.as_ref()),
            SeedEntry::Orbit(model) => serde_json::to_writer(writer, model.as_ref()),
        };
        if encoded.is_err() {
            // An oversized or unserializable page never consumes the budget
            // of later, smaller pages and never leaves a partial section.
            payload.truncate(offset);
            continue;
        }
        sections.push(Section {
            key,
            offset,
            len: payload.len() - offset,
            hash: Digest::of(&payload[offset..]),
        });
    }
    for display in displays.iter().take(SECTION_CAP.saturating_sub(sections.len())) {
        if display.observation().cursor.as_slice() != root.revision().encode_control().as_ref()
            || display.observation().producer_epoch != root.producer_epoch() { continue; }
        let key = SectionKey::Display(display.wire().address.clone());
        if sections.iter().any(|section| section.key == key) { continue; }
        let offset = payload.len();
        if serde_json::to_writer(BoundedWriter::new(&mut payload, CAP)?, display.wire()).is_err() { payload.truncate(offset); continue; }
        if payload.len() - offset > DISPLAY_SECTION_CAP { payload.truncate(offset); continue; }
        sections.push(Section { key, offset, len: payload.len() - offset, hash: Digest::of(&payload[offset..]) });
    }
    let mut table = Vec::new();
    serde_json::to_writer(
        BoundedWriter::new(&mut table, TABLE_CAP)?,
        &Table {
            namespace,
            root: SnapRoot::with_writer(root, writer),
            sections,
        },
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut bytes = Vec::with_capacity(HEADER + table.len() + payload.len());
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&SCHEMA.to_le_bytes());
    bytes.extend_from_slice(&(table.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&Digest::of(&table).0);
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

fn decode(bytes: &[u8], wanted: &[PageKey]) -> Result<Seed, Refusal> { decode_selected(bytes, wanted, None, None) }

fn decode_selected(bytes: &[u8], wanted: &[PageKey], namespace: Option<Digest>, route: Option<&Route>) -> Result<Seed, Refusal> {
    if bytes.len() > FILE_CAP {
        return Err(Refusal::TooLarge);
    }
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
    if table_len > TABLE_CAP {
        return Err(Refusal::TableLength);
    }
    let payload_at = HEADER.checked_add(table_len).ok_or(Refusal::TableEnd)?;
    let table = bytes.get(HEADER..payload_at).ok_or(Refusal::TableEnd)?;
    if Digest::of(table).0[..] != header[16..48] {
        return Err(Refusal::TableHash);
    }
    let table: Table =
        serde_json::from_slice(table).map_err(|error| Refusal::Table(error.to_string()))?;
    if namespace.is_some() && table.namespace != namespace { return Err(Refusal::Namespace); }
    let payload = &bytes[payload_at..];
    if payload.len() > CAP {
        return Err(Refusal::TooLarge);
    }
    let mut end = 0;
    for (index, section) in table.sections.iter().enumerate() {
        let fault = |fault| Refusal::Section {
            key: section.key.clone(),
            fault,
        };
        if table.sections[..index]
            .iter()
            .any(|earlier| earlier.key == section.key)
        {
            return Err(Refusal::Sections);
        }
        if section.offset != end || section.len == 0 {
            return Err(fault(SectionFault::Layout));
        }
        end = end
            .checked_add(section.len)
            .ok_or_else(|| fault(SectionFault::PastTheEnd))?;
        let body = payload
            .get(section.offset..end)
            .ok_or_else(|| fault(SectionFault::PastTheEnd))?;
        if Digest::of(body) != section.hash {
            return Err(fault(SectionFault::Hash));
        }
    }
    if end != payload.len() {
        return Err(Refusal::TableEnd);
    }
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
        let entry =
            entry(key, body).map_err(|error| fault(SectionFault::Decode(error.to_string())))?;
        pages.push(entry);
    }
    let mut displays = Vec::new();
    if let Some(route) = route {
        if let Some(address) = DisplayAddress::for_route(route) {
            if let Some(section) = table.sections.iter().find(|section| section.key == SectionKey::Display(address.clone())) {
                let fault = |why| Refusal::Section { key: section.key.clone(), fault: why };
                if section.len > DISPLAY_SECTION_CAP { return Err(fault(SectionFault::Decode("retained display exceeds section budget".into()))); }
                let body = &payload[section.offset..section.offset + section.len];
                let display: RetainedDisplayWire = serde_json::from_slice(body).map_err(|error| fault(SectionFault::Decode(error.to_string())))?;
                if display.observation.cursor != table.root.cursor.0
                    || display.observation.producer_epoch != table.root.epoch {
                    return Err(fault(SectionFault::Decode("display address, observation or source scope mismatch".into())));
                }
                let display = RetainedDisplay::admit(display, route).ok_or_else(|| fault(SectionFault::Decode("display address or source scope mismatch".into())))?;
                displays.push(Arc::new(display));
            }
        }
    }
    Ok(Seed { root: table.root, pages, displays })
}

/// The page a section's payload holds.
fn entry(key: &PageKey, body: &[u8]) -> Result<SeedEntry, serde_json::Error> {
    Ok(match key {
        PageKey::Symbol(symbol) => SeedEntry::Symbol(symbol.clone(), serde_json::from_slice(body)?),
        PageKey::Source(symbol) => SeedEntry::Source(symbol.clone(), serde_json::from_slice(body)?),
        PageKey::Package(package) => {
            SeedEntry::Package(package.clone(), serde_json::from_slice(body)?)
        }
        PageKey::Orbit => SeedEntry::Orbit(serde_json::from_slice(body)?),
        // Only kept families reach this decoder. No table claim is parsed
        // into a SymbolRef, PackageRef or any producer capability.
        PageKey::CargoSource(_) | PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "unkept snapshot family",
            ));
        }
    })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
