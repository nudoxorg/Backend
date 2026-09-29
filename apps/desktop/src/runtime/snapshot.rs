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
//! table (JSON: root, writer, sections {key, offset, len, sha256})
//! payloads (JSON page models, one per section)
//! ```
//!
//! Its values land at the unserved root. When the owner answers, a
//! snapshot read at the root the owner serves (by the same build) is
//! current as it is; otherwise each value is revalidated quietly and only a
//! different one redraws (`PageStore::seed`, `Landing::Unchanged`).

use crate::core::VersionedRoot;
use crate::model::pages::{
    OrbitModel, PackageDossier, PageKey, PageValue, SourceView, SymbolPage,
};
use crate::navigation::Route;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// The file's name inside the workspace's data directory.
pub const FILE_NAME: &str = "desktop-snapshot.nxs";

const MAGIC: [u8; 8] = *b"NXSNAP\0\x01";
/// Bump when a saved page model changes meaning without changing its shape
/// (a shape change already fails the decode, which ignores the file).
const SCHEMA: u32 = 1;
const HEADER: usize = 8 + 4 + 4 + 32;
/// Saved pages beyond this many bytes are left out (the route's pages are
/// a few hundred kilobytes; this bounds a pathological page, not a normal
/// one).
const CAP: usize = 8 << 20;

/// The root a snapshot's pages were read at, and the build that read them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapRoot {
    epoch: u64,
    /// The owner's cursor in its own control encoding: compared whole,
    /// never turned back into a cursor (`Cursor::encode_control`).
    cursor: String,
    /// Which build read them: a different mapping of the same root is a
    /// different page.
    writer: String,
}

impl SnapRoot {
    fn of(root: VersionedRoot) -> Self {
        Self {
            epoch: root.producer_epoch(),
            cursor: hex(&root.revision().encode_control()),
            writer: writer().to_owned(),
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
    pub pages: Vec<(PageKey, PageValue)>,
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

/// One page to save, shared with the store (never copied to save it).
#[derive(Clone, Debug)]
pub enum Kept {
    /// A declaration page.
    Symbol(Arc<SymbolPage>),
    /// A source view.
    Source(Arc<SourceView>),
    /// A package dossier.
    Package(Arc<PackageDossier>),
    /// The Orbit model.
    Orbit(Arc<OrbitModel>),
}

/// The pages worth saving for a route: what it shows, the dossier its
/// shelf reads, and the Orbit model the shelf lists the index from.
#[must_use]
pub fn kept_keys(route: &Route) -> Vec<PageKey> {
    let mut keys = super::store::route_keys(route)
        .into_iter()
        .filter(|key| spell(key).is_some())
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
struct Bad(String);

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
                    format_args!("{} of {} wanted, {} bytes", seed.pages.len(), wanted.len(), bytes.len()),
                );
                Some(seed)
            }
            Err(Bad(why)) => {
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
    pub fn write(&self, root: VersionedRoot, pages: &[(PageKey, Kept)]) -> std::io::Result<usize> {
        if root.is_unserved() {
            return Ok(0);
        }
        let bytes = encode(root, pages);
        backend_platform::durable::write_atomic(&self.path, &bytes)?;
        Ok(bytes.len())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Table {
    root: SnapRoot,
    sections: Vec<Section>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Section {
    key: String,
    offset: usize,
    len: usize,
    hash: String,
}

/// The file's bytes for `pages` read at `root`.
#[must_use]
fn encode(root: VersionedRoot, pages: &[(PageKey, Kept)]) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut sections = Vec::new();
    for (key, kept) in pages {
        let Some(key) = spell(key) else { continue };
        let encoded = match kept {
            Kept::Symbol(page) => serde_json::to_vec(page.as_ref()),
            Kept::Source(view) => serde_json::to_vec(view.as_ref()),
            Kept::Package(dossier) => serde_json::to_vec(dossier.as_ref()),
            Kept::Orbit(model) => serde_json::to_vec(model.as_ref()),
        };
        let Ok(encoded) = encoded else { continue };
        if payload.len() + encoded.len() > CAP {
            continue;
        }
        sections.push(Section {
            key,
            offset: payload.len(),
            len: encoded.len(),
            hash: hex(&digest(&encoded)),
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
    bytes.extend_from_slice(&digest(&table));
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(&payload);
    bytes
}

fn decode(bytes: &[u8], wanted: &[PageKey]) -> Result<Seed, Bad> {
    let header = bytes.get(..HEADER).ok_or_else(|| Bad("shorter than its header".into()))?;
    if header[..8] != MAGIC {
        return Err(Bad("not a snapshot".into()));
    }
    let word = |at: usize| u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    let schema = word(8);
    if schema != SCHEMA {
        return Err(Bad(format!("schema {schema}, this build reads {SCHEMA}")));
    }
    let table_len = usize::try_from(word(12)).map_err(|_| Bad("table length".into()))?;
    let table = bytes
        .get(HEADER..HEADER + table_len)
        .ok_or_else(|| Bad("table past the end".into()))?;
    if digest(table)[..] != header[16..48] {
        return Err(Bad("table hash".into()));
    }
    let table: Table = serde_json::from_slice(table).map_err(|error| Bad(format!("table: {error}")))?;
    let payload = &bytes[HEADER + table_len..];
    let mut pages = Vec::new();
    for key in wanted {
        let Some(spelled) = spell(key) else { continue };
        let Some(section) = table.sections.iter().find(|section| section.key == spelled) else {
            continue;
        };
        let body = section
            .offset
            .checked_add(section.len)
            .and_then(|end| payload.get(section.offset..end))
            .ok_or_else(|| Bad(format!("{spelled}: past the end")))?;
        if hex(&digest(body)) != section.hash {
            return Err(Bad(format!("{spelled}: section hash")));
        }
        let value = page(key, body).map_err(|error| Bad(format!("{spelled}: {error}")))?;
        pages.push((key.clone(), value));
    }
    Ok(Seed {
        root: table.root,
        pages,
    })
}

fn page(key: &PageKey, body: &[u8]) -> Result<PageValue, serde_json::Error> {
    Ok(match key {
        PageKey::Symbol(_) => PageValue::Symbol(serde_json::from_slice(body)?),
        PageKey::Source(_) => PageValue::Source(serde_json::from_slice(body)?),
        PageKey::Package(_) => PageValue::Package(serde_json::from_slice(body)?),
        _ => PageValue::Orbit(serde_json::from_slice(body)?),
    })
}

/// A key's name in the table; `None` for families the snapshot never keeps
/// (searches, health, browsing: asked fresh every time).
fn spell(key: &PageKey) -> Option<String> {
    match key {
        PageKey::Symbol(symbol) => Some(format!("symbol {}", symbol.as_str())),
        PageKey::Source(symbol) => Some(format!("source {}", symbol.as_str())),
        PageKey::Package(package) => Some(format!("package {}", package.as_str())),
        PageKey::Orbit => Some("orbit".to_owned()),
        PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => None,
    }
}

/// This build, as far as a saved page can tell: the running executable's
/// size and modification time (one `stat`, once).
fn writer() -> &'static str {
    static WRITER: OnceLock<String> = OnceLock::new();
    WRITER.get_or_init(|| {
        std::env::current_exe()
            .and_then(std::fs::metadata)
            .map(|meta| {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |at| at.as_nanos());
                format!("{}:{modified}", meta.len())
            })
            .unwrap_or_default()
    })
}

/// The content hash that checks a table and each section (SHA-256: the
/// desktop's one hash dependency, hardware-accelerated on Apple silicon).
fn digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
