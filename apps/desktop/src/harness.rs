//! The desktop capture adapter: the gallery command line (`capture`, `film`,
//! `motion-report`, `storm`, `matrix`, `lint`, `perf`, `verify`) over the
//! real desktop shell, on a real local index of fixed fixture crates.
//!
//! Settings arrive as the product's own intents (text size as `ZoomTo` on
//! the window's display).
//!
//! Determinism. The fixture owner indexes `crates/present`,
//! `frontends/rust/fixtures/rich_project`, `crates/runtime`,
//! `frontends/rust/fixtures/toml_pin`, and exact registry releases for the
//! page2 symbol prototypes and package-comparison fixtures from the local
//! Cargo cache once into `.local/harness/desktop/`; every later boot reuses
//! that index. Page
//! reads are real I/O on real threads, so each boot declares a quiescence
//! predicate: after the first frame and after every input instant the run
//! waits in real time (virtual time stands still) until the read pool is
//! empty, every result has landed, and the root has no engine work in
//! flight. I/O then takes zero virtual time and a frame at a virtual time is
//! a function of the script.
//!
//! The seams, each one line or one function: the window root is
//! [`ROOT`] (`crate::shell::open_shell`); a route description becomes a
//! product route in [`route::to_route`] (`Route::Symbol { id, at, view }`,
//! `Route::World`, `PackageRoute { at }`); settings acts become product
//! intents in [`adapt`].

use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::pages::{PackageRef, SymbolRef};
use crate::model::{
    AppSnapshot, AppearancePreference, ContrastPreference, DensityPreference, MotionPreference,
    ProjectPhase, WorkspaceProject, WorkspaceState,
};
use crate::navigation::Intent;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadPool, ReadRequest, SessionReader};
use crate::runtime::{CancellationToken, DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use backend_client::{LocalSubscriptionTransport, Session};
use backend_gui_harness::Act;
use facet::ActiveFacet;
use facet::gallery::{self, Scene};
use gpui::{AnyView, App, Global, Window};
use refusals::{Fate, Listing, Refusals, Reply, Row, Standing, fate, settled, standing};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub mod journey;
mod refusals;
mod startup;

/// The window root the adapter mounts: the one line to change when the
/// shell's constructor moves.
const ROOT: fn(&UiEntityGraph, &mut Window, &mut App) -> gpui::Entity<crate::shell::Shell> =
    crate::shell::open_shell;

const INDEX_DEADLINE: Duration = Duration::from_mins(15);

/// How long to wait for another process's owner to answer.
const OWNER_DEADLINE: Duration = Duration::from_mins(1);

fn review_diag(message: &str) {
    if std::env::var_os("NUDOX_REVIEW_DIAGNOSTICS").is_some() {
        eprintln!("[w-pages-review] {message}");
    }
}

/// A root the owner would not index, in the owner's own words.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    /// The root.
    pub root: PathBuf,
    /// What the owner said.
    pub reason: String,
}

/// What the fixture's pages are made of: how many roots the owner indexed
/// afresh, how many it serves from a prior generation because it refused the
/// refresh, and how many it has nothing to serve for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provenance {
    /// Indexed and served as the owner just built them.
    pub fresh: usize,
    /// Refused a refresh; served from what the owner kept.
    pub preserved: usize,
    /// Nothing to serve: the app's fault plate.
    pub failed: usize,
}

/// The fixture owner: a local index of fixed crates, shared by every boot in
/// this process.
pub struct Fixture {
    host: crate::DesktopHost,
    projects: Vec<PathBuf>,
    preserved: Vec<Refusal>,
    failed: Vec<Refusal>,
}

impl Fixture {
    /// The owner's endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        self.host.endpoint()
    }

    /// Every fixture root, indexed or not.
    #[must_use]
    pub fn projects(&self) -> &[PathBuf] {
        &self.projects
    }

    /// The roots the owner has NOTHING to serve for: it refused them and holds
    /// no content, or recorded their rows as failed. Their pages show the
    /// app's fault plate; every other root is served as usual.
    #[must_use]
    pub fn failed(&self) -> &[Refusal] {
        &self.failed
    }

    /// The roots whose refresh the owner refused and which it serves anyway,
    /// from the prior generation it kept. Their pages are real, not fresh: a
    /// capture of them is a capture of preserved data, and says so.
    #[must_use]
    pub fn preserved(&self) -> &[Refusal] {
        &self.preserved
    }

    /// What the pages are made of.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        provenance(self.projects.len(), self.preserved.len(), self.failed.len())
    }
}

/// `total` roots, of which `preserved` serve a prior generation and `failed`
/// serve nothing; the rest are fresh.
fn provenance(total: usize, preserved: usize, failed: usize) -> Provenance {
    Provenance { fresh: total.saturating_sub(preserved + failed), preserved, failed }
}

static FIXTURE: std::sync::OnceLock<std::sync::Mutex<Option<&'static Fixture>>> = std::sync::OnceLock::new();
static STATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static RESPONSIVE_STARTUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Native review windows paint immediately while fixture preparation runs.
/// Headless capture retains a failing process result when boot cannot finish.
pub fn responsive_startup() {
    RESPONSIVE_STARTUP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Keeps this process's fixture index in `dir` instead of
/// `.local/harness/desktop` (same roots, its own owner), so a long run such
/// as a journey never contends with scene runs for the index's lock. Call
/// it before the first [`fixture`]; later calls are ignored.
pub fn keep_index_in(dir: PathBuf) {
    let _ = STATE.set(dir);
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The pinned two-member workspace the `tree` route reads (`browse_tests.rs`
/// proves its numbers): never the live repository, whose `Cargo.lock` and
/// dependency graph drift as the workspace changes, which would make the
/// scene and its captures non-deterministic.
fn browse_tree_root() -> PathBuf {
    repo().join("apps/desktop/tests/fixtures/browse_tree")
}

/// The shelf's Library, with the tree's own project as the one project open
/// and ready: the body and the shelf then name the same workspace.
fn tree_workspace(project: &LocalProjectId) -> WorkspaceState {
    let mut row = WorkspaceProject::indexing_with_id(project.clone());
    row.phase = ProjectPhase::Ready;
    WorkspaceState {
        projects: vec![row].into(),
        active: Some(project.clone()),
        host: Some(project.clone()),
        path_error: None,
    }
}

/// The shelf's Library when Orbit is the start route, with `toml_pin` (J1's
/// "your project") as the one project open and ready: the same honest way
/// [`tree_workspace`] gives the Tree route its one project, from the
/// fixture's own indexed roots, never invented. Without this, Orbit boots
/// with an empty workspace and its centre draws nothing to click.
fn orbit_workspace(fixture: &Fixture) -> Result<WorkspaceState, String> {
    let root = fixture
        .projects()
        .iter()
        .find(|project| project.ends_with("toml_pin"))
        .ok_or_else(|| "toml_pin is not among the fixture's indexed projects".to_owned())?;
    let project = LocalProjectId::from_path(root).map_err(|error| format!("toml_pin project identity: {error:?}"))?;
    Ok(tree_workspace(&project))
}

/// A crate's unpacked source in the local cargo registry cache
/// (`$CARGO_HOME/registry/src/<index>/<name-version>`): indexed offline,
/// never fetched.
fn registry_source(release: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("CARGO_HOME").map_or_else(
        || std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")),
        |home| Some(PathBuf::from(home)),
    );
    let src = home
        .ok_or_else(|| "neither CARGO_HOME nor HOME is set".to_owned())?
        .join("registry/src");
    let mut found = std::fs::read_dir(&src)
        .map_err(|error| format!("{}: {error}", src.display()))?
        .filter_map(|index| Some(index.ok()?.path().join(release)))
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect::<Vec<_>>();
    found.sort();
    found.pop().map_or_else(
        || Err(format!("{release} is not in the cargo registry cache under {}; fetch it once with cargo", src.display())),
        |path| path.canonicalize().map_err(|error| format!("{}: {error}", path.display())),
    )
}

/// The owner's endpoint for the index in `data`: one per data directory,
/// not per process. Ownership is one lock on `data`, so a second process
/// cannot own it; with the same endpoint it reaches the running owner
/// (`DesktopHost::start_with_paths` attaches to a live one) instead of
/// dialling a socket nobody listens on. Under `/tmp`: a socket path under
/// the repository exceeds `sockaddr_un`.
fn endpoint_for(data: &Path) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let data = data
        .canonicalize()
        .map_err(|error| format!("{}: {error}", data.display()))?;
    let digest = sha2::Sha256::digest(data.as_os_str().as_encoded_bytes());
    let hex = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(PathBuf::from(format!("/tmp/nx-harness-{hex}.sock")))
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Starts (or reuses) the fixture owner and waits until every fixture crate
/// is indexed and the row count is stable.
///
/// When another process already owns the index, this one attaches to that
/// owner (the endpoint is per data directory). It still asks for every root
/// it knows and waits for each to be Ready: an owner started by an older
/// binary with fewer roots indexes the missing ones (the other process then
/// sees them too), and nothing is served until they are ready or the
/// deadline passes. The attached process depends on the owner's process: if
/// that exits mid-run, this one's reads fail.
///
/// # Errors
/// The owner cannot start, or indexing does not settle in 15 minutes.
pub fn fixture() -> Result<&'static Fixture, String> {
    fixture_with_progress(|_| {})
}

fn fixture_with_progress(progress: impl Fn(&str)) -> Result<&'static Fixture, String> {
    // Native startup retries use fresh worker threads. Keep the successfully
    // opened owner alive at process scope and serialize concurrent attempts;
    // errors leave the cache empty so retry can try again.
    let fixture_cache = FIXTURE.get_or_init(|| std::sync::Mutex::new(None));
    let mut cached = fixture_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(fixture) = *cached {
        return Ok(fixture);
    }
    let repo = repo()
        .canonicalize()
        .map_err(|error| format!("repository root: {error}"))?;
    let projects = vec![
        repo.join("crates/present"),
        repo.join("frontends/rust/fixtures/rich_project"),
        // The journeys' subjects (gui-plan §3 item 10, "Data"): J5 reads
        // `crates/runtime`; "your project" pins toml 0.8.23, whose source is
        // indexed offline from the local cargo registry cache.
        repo.join("crates/runtime"),
        repo.join("frontends/rust/fixtures/toml_pin"),
        registry_source("toml-0.8.23")?,
        // The five page2 symbol prototypes and package-comparison candidates.
        // Registry roots are pinned to the prototype extraction's exact
        // releases, and are resolved from the local Cargo cache only.
        registry_source("serde_json-1.0.151")?,
        registry_source("serde_core-1.0.229")?,
        registry_source("smallvec-1.16.0")?,
        registry_source("basic-toml-0.1.10")?,
        registry_source("toml_edit-0.22.27")?,
        // The symbol page's record, in three languages (W-Page): toml's
        // Datetime (Rust), zod's issue shapes (TypeScript, a pinned excerpt)
        // and spf13/pflag (Go, a pinned copy). TypeScript and Go index through
        // the checker and oracle the development shell provides.
        registry_source("toml_datetime-0.6.11")?,
        // The package page's richest subject (W-Folio): 25 features, 33 public
        // modules, network/files/programs/unsafe all read from its source.
        registry_source("tokio-1.53.1")?,
        repo.join("apps/desktop/tests/fixtures/lang/ts/zod"),
        repo.join("apps/desktop/tests/fixtures/lang/go/pflag"),
    ];
    let mut preflight_roots = projects.clone();
    preflight_roots.push(browse_tree_root());
    preflight_fixture_manifests(&preflight_roots)?;
    progress("Preparing the fixture owner");
    // `NUDOX_HARNESS_STATE` keeps a run's index (and its owner's advisory
    // authority) apart from the shared one other runs use.
    let configured = STATE
        .get()
        .cloned()
        .or_else(|| std::env::var_os("NUDOX_HARNESS_STATE").map(PathBuf::from))
        .unwrap_or_else(|| repo.join(".local/harness/desktop"));
    private_umask();
    let (host, endpoint, state) = match open_owner(&configured, &projects[0]) {
        Ok(opened) => opened,
        // The owner refused the directory itself (a mode it will not accept,
        // an index written by another build): index into a clean one this
        // harness owns instead of failing every capture, and say so once.
        Err(Opening::Refused(reason)) => {
            let fresh = fallback_state(&repo, &configured);
            if fresh == configured {
                return Err(format!("fixture owner: {} refused: {reason}", configured.display()));
            }
            report_once(format!(
                "state dir {} refused by the owner ({reason}); using the clean state dir {}",
                configured.display(),
                fresh.display()
            ));
            open_owner(&fresh, &projects[0]).map_err(|second| {
                format!(
                    "fixture owner: {} refused ({reason}); the clean state dir {} failed too: {}",
                    configured.display(),
                    fresh.display(),
                    second.words()
                )
            })?
        }
        Err(Opening::Failed(reason)) => return Err(format!("fixture owner: {reason}")),
    };
    let indexing = Instant::now();
    let mut session = Session::connect(&endpoint).map_err(|error| format!("session: {error}"))?;
    // A refusal is a property of the owner, its toolchain and the root's own
    // bytes, not of the run: repeating the request for every capture costs
    // minutes per root for the same answer. What was refused before under the
    // same three is read back from the state directory and not asked again
    // (`refusals`).
    let refusals = Refusals::load(&state, &projects);
    let mut roots = index_roots(&mut session, &endpoint, &projects, &refusals, &progress)?;
    refusals.save(&roots.iter().filter_map(|root| match &root.reply {
        Reply::Refused(words) => Some((root.path.clone(), words.clone())),
        Reply::Accepted => None,
    }).collect());
    crate::runtime::trace::span("boot.index_requests", indexing, format_args!("{} roots", roots.len()));
    progress("Waiting for indexed sources");
    let started = Instant::now();
    let rows = await_settled(&mut session, &mut roots)?;
    crate::runtime::trace::span("boot.index_settled", started, format_args!("{rows} rows"));
    let (mut preserved, mut failed) = (Vec::new(), Vec::new());
    for root in &roots {
        match &root.standing {
            Standing::Preserved(reason) => preserved.push(Refusal { root: root.path.clone(), reason: reason.clone() }),
            Standing::Failed(reason) => failed.push(Refusal { root: root.path.clone(), reason: reason.clone() }),
            Standing::Ready | Standing::Pending => {}
        }
    }
    report_provenance(projects.len(), &preserved, &failed);
    let fixture: &'static Fixture = Box::leak(Box::new(Fixture { host, projects, preserved, failed }));
    *cached = Some(fixture);
    Ok(fixture)
}

/// One fixture root through the boot: what the owner answered to its index
/// request, and where it stands now.
struct Indexed {
    path: PathBuf,
    reply: Reply,
    standing: Standing,
}

/// How many times an index request is put again when the connection died
/// under it (never when the owner said no).
const INDEX_ATTEMPTS: u32 = 3;

/// How many polls (300 ms each) an owner that lists no rows at all is given
/// before "no row for this root" is believed.
const LISTING_POLLS: u32 = 10;

/// Asks the owner to index `root`. Its "no" (a compile failure, say) is an
/// answer; a dead connection is not: it is asked again on a new one, and if
/// it stays dead the boot fails rather than record a fault of the line as a
/// fault of the root.
fn ask_index(session: &mut Session, endpoint: &Path, root: &Path) -> Result<Reply, String> {
    let coordinate = utf8(root)?;
    for attempt in 1..=INDEX_ATTEMPTS {
        let Err(error) = session.index(coordinate) else { return Ok(Reply::Accepted) };
        match fate(&error) {
            Fate::Refused(words) => return Ok(Reply::Refused(words)),
            Fate::Retry if attempt < INDEX_ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
                if let Ok(fresh) = Session::connect(endpoint) {
                    *session = fresh;
                }
            }
            Fate::Retry | Fate::Fatal => return Err(format!("index {}: {error}", root.display())),
        }
    }
    Err(format!("index {}: the owner never answered", root.display()))
}

/// Puts every root to the owner (or reads its refusal back from `refusals`).
/// A root the owner refuses is not a reason to stop: the other roots are
/// indexed and captured as usual.
fn index_roots(
    session: &mut Session,
    endpoint: &Path,
    projects: &[PathBuf],
    refusals: &Refusals,
    progress: &impl Fn(&str),
) -> Result<Vec<Indexed>, String> {
    let total = projects.len();
    let mut roots = Vec::with_capacity(total);
    for (index, project) in projects.iter().enumerate() {
        progress(&format!("Indexing {} ({}/{total})", project.file_name().and_then(|name| name.to_str()).unwrap_or("fixture"), index + 1));
        let reply = match refusals.recorded(project) {
            Some(reason) => {
                report_root_refused(project, &format!("{reason} (refused earlier by this same owner, toolchain and root; not asked again)"));
                Reply::Refused(reason.to_owned())
            }
            None => {
                let reply = ask_index(session, endpoint, project)?;
                if let Reply::Refused(words) = &reply {
                    report_root_refused(project, words);
                }
                reply
            }
        };
        roots.push(Indexed { path: project.clone(), reply, standing: Standing::Pending });
    }
    Ok(roots)
}

/// Polls the owner until every root is Ready, Preserved or Failed and the row
/// count has held still for three polls. Returns the rows.
fn await_settled(session: &mut Session, roots: &mut [Indexed]) -> Result<u64, String> {
    let started = Instant::now();
    let (mut last_rows, mut stable, mut polls) = (0, 0, 0_u32);
    loop {
        polls += 1;
        if let Ok(backend_library::CommandReply::Packages(snapshot)) = session.packages().map(|reply| reply.reply) {
            let listing = if snapshot.root.rows().is_empty() && polls < LISTING_POLLS { Listing::Awaited } else { Listing::Settled };
            for root in roots.iter_mut() {
                let row = snapshot
                    .root
                    .rows()
                    .iter()
                    .find(|row| Some(row.label.as_str()) == root.path.to_str())
                    .map(|row| Row { state: row.state, words: row_words(row) });
                root.standing = standing(&root.reply, row.as_ref(), listing);
                if let Standing::Failed(reason) = &root.standing {
                    report_root_failed(&root.path, reason);
                }
            }
            // With NUDOX_REVIEW_DIAGNOSTICS, say which roots are still not
            // ready, and how, every ~10 s of waiting.
            if polls.is_multiple_of(33) {
                for root in roots.iter().filter(|root| root.standing != Standing::Ready) {
                    review_diag(&format!("waiting on {}: {:?}", root.path.display(), root.standing));
                }
            }
        }
        let rows = session.health().map_or(0, |health| health.row_count());
        // With every root failed there are no rows to wait for: the pages
        // are the app's fault plates and the wait is over once it is quiet.
        let all_failed = roots.iter().all(|root| matches!(root.standing, Standing::Failed(_)));
        let standings = roots.iter().map(|root| root.standing.clone()).collect::<Vec<_>>();
        stable = if settled(&standings) && (rows > 0 || all_failed) && rows == last_rows { stable + 1 } else { 0 };
        last_rows = rows;
        if stable >= 3 {
            return Ok(rows);
        }
        if started.elapsed() > INDEX_DEADLINE {
            let pending = roots.iter().filter(|root| root.standing == Standing::Pending).map(|root| root.path.display().to_string()).collect::<Vec<_>>();
            return Err(format!("the fixture index never settled; still pending: {}", pending.join(", ")));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Says once, on stderr, what the captures of this boot are made of: a run
/// that captures preserved data (the owner refused the refresh and serves the
/// prior generation) must not look like one that captured fresh data.
fn report_provenance(total: usize, preserved: &[Refusal], failed: &[Refusal]) {
    if failed.len() == total {
        report_once(format!("no fixture root is indexed ({total} of {total} failed): every package and symbol page shows the app's fault plate"));
    } else if !failed.is_empty() {
        report_once(format!("{} of {total} fixture roots have nothing to serve: their pages show the app's fault plate", failed.len()));
    }
    if let Some(first) = preserved.first() {
        report_once(format!(
            "capturing PRESERVED data for {} of {total} fixture roots: the owner refused their refresh and serves the prior generation it kept (first: {}: {})",
            preserved.len(),
            first.root.display(),
            first.reason.chars().take(90).collect::<String>()
        ));
    }
}

/// The prose a row carries, in order.
fn row_words(row: &backend_library::Row) -> String {
    row.document
        .iter()
        .filter_map(|fragment| match fragment {
            backend_library::Fragment::Text(text) | backend_library::Fragment::Code(text) => Some(text.as_str()),
            backend_library::Fragment::Link { label, .. } => Some(label.as_str()),
            backend_library::Fragment::Break => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

/// Says `message` on stderr the first time only: a run that retries its
/// boot, or polls a failed root every 300 ms, names it once.
fn report_once(message: String) {
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !said.contains(&message) {
        eprintln!("[harness] {message}");
        said.push(message);
    }
}

fn report_root_refused(project: &Path, reason: &str) {
    report_once(format!("root {} was refused by the owner, capturing the rest: {reason}", project.display()));
}

fn report_root_failed(project: &Path, reason: &str) {
    report_once(format!("root {} has nothing to serve: {reason}", project.display()));
}

/// How the engine's `WorkspaceError::AlreadyOwned` reads inside a composition
/// refusal (its `Display` is `workspace error: {self:?}`). The error type is
/// not re-exported to this crate, so the name is held here once, and
/// `the_engines_held_lock_error_is_still_named_as_the_harness_reads_it` reads
/// the engine's source so that renaming the variant fails a test instead of
/// turning a live lock into "refused".
const HELD_LOCK_WORDS: &str = "AlreadyOwned";

/// Why the owner would not start on a state directory.
enum Opening {
    /// The owner refused the directory itself: its mode or owner, or an index
    /// written in a format this build does not read. A clean directory may
    /// still work.
    Refused(String),
    /// Anything else: the path is not UTF-8, another owner never answered.
    Failed(String),
}

impl Opening {
    fn words(&self) -> &str {
        match self {
            Self::Refused(words) | Self::Failed(words) => words,
        }
    }
}

/// Whether the owner refused the workspace itself rather than another live
/// process still opening it (which is worth waiting for). Composition
/// reports every refusal as prose (`ProcessError::Profile`), so the held lock
/// is told by the engine's own name for it ([`HELD_LOCK_WORDS`]).
fn state_refused(error: &crate::HostError) -> bool {
    match error {
        crate::HostError::Runtime(_) | crate::HostError::Service(_) => true,
        crate::HostError::Contended { refusal, .. } => {
            let held = matches!(**refusal, backend_local_service::ProcessError::Listener(_))
                || refusal.to_string().contains(HELD_LOCK_WORDS);
            !held
        }
        crate::HostError::UnsupportedPathEncoding { .. } => false,
    }
}

/// A private umask for a process that owns durable state. The owner creates
/// its own subdirectories with the process umask and refuses to write beneath
/// any that is group- or world-accessible (`crates/platform/durable.rs`), so
/// under a shell's usual 022 the first boot succeeds and every later boot of
/// the same state directory is refused. Files this process writes afterwards
/// (captures included) are 0600.
fn private_umask() {
    #[cfg(unix)]
    rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o077));
}

/// A directory only this user can enter: the owner refuses anything looser
/// (`crates/platform/durable.rs`), and `create_dir_all` alone honours the
/// umask (0755).
fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// The clean state directory a run falls back to when `configured` is
/// refused: one per refused directory under `.local/harness/fallback-*`, kept
/// between runs so its index stays warm.
fn fallback_state(repo: &Path, configured: &Path) -> PathBuf {
    let name = configured
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    repo.join(".local/harness").join(format!("fallback-{name}"))
}

/// Starts (or attaches to) the owner of the index in `state`.
///
/// Another process may hold the index lock and still be opening it (the host
/// waits 2 s for its endpoint; a debug owner can take longer): keep asking
/// until it answers or the lock frees, for up to a minute. A directory the
/// owner refuses is reported at once instead.
fn open_owner(state: &Path, project: &Path) -> Result<(crate::DesktopHost, PathBuf, PathBuf), Opening> {
    private_dir(&state.join("data")).map_err(|error| Opening::Refused(format!("{}: {error}", state.display())))?;
    let endpoint = endpoint_for(&state.join("data")).map_err(Opening::Failed)?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(project.to_path_buf()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .map_err(|error| Opening::Failed(format!("workspace paths: {error}")))?;
    let attaching = Instant::now();
    loop {
        match crate::DesktopHost::start_with_paths(paths.clone()) {
            Ok(host) => {
                crate::runtime::trace::span("boot.owner_start", attaching, format_args!("{:?}", host.mode()));
                return Ok((host, endpoint, state.to_path_buf()));
            }
            Err(error) if state_refused(&error) => return Err(Opening::Refused(error.to_string())),
            Err(crate::HostError::Contended { .. }) if attaching.elapsed() < OWNER_DEADLINE => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(Opening::Failed(error.to_string())),
        }
    }
}

/// Reject an unsupported source-language edition before starting an owner or
/// walking any indexed source tree. Workspace-inherited edition declarations
/// are resolved from the nearest workspace root, matching Cargo's profile.
fn preflight_fixture_manifests(projects: &[PathBuf]) -> Result<(), String> {
    for project in projects {
        let manifest = project.join("Cargo.toml");
        // A TypeScript or Go root has no Rust edition to check.
        if !manifest.exists() && (project.join("package.json").exists() || project.join("go.mod").exists()) {
            continue;
        }
        let bytes = std::fs::read_to_string(&manifest)
            .map_err(|error| format!("{}: {error}", manifest.display()))?;
        let parsed: toml::Value = toml::from_str(&bytes)
            .map_err(|error| format!("{}: {error}", manifest.display()))?;
        if parsed.get("package").is_some() {
            preflight_package_edition(project, &manifest, &parsed)?;
            continue;
        }
        let members = parsed
            .get("workspace")
            .and_then(toml::Value::as_table)
            .and_then(|workspace| workspace.get("members"))
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("{} has no [package] or [workspace.members]", manifest.display()))?;
        for member in members {
            let member = member.as_str().ok_or_else(|| format!("{} has a non-string workspace member", manifest.display()))?;
            let member_root = project.join(member);
            let member_manifest = member_root.join("Cargo.toml");
            let bytes = std::fs::read_to_string(&member_manifest)
                .map_err(|error| format!("{}: {error}", member_manifest.display()))?;
            let parsed: toml::Value = toml::from_str(&bytes)
                .map_err(|error| format!("{}: {error}", member_manifest.display()))?;
            preflight_package_edition(&member_root, &member_manifest, &parsed)?;
        }
    }
    Ok(())
}

fn preflight_package_edition(project: &Path, manifest: &Path, parsed: &toml::Value) -> Result<(), String> {
    let package = parsed
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("{} has no [package] table", manifest.display()))?;
    let declared = package.get("edition").ok_or_else(|| format!("{} has no package.edition", manifest.display()))?;
    let edition = if let Some(edition) = declared.as_str() {
        edition.to_owned()
    } else if declared.as_table().and_then(|inherit| inherit.get("workspace")).and_then(toml::Value::as_bool) == Some(true) {
        let mut ancestor = project.parent();
        let mut inherited = None;
        while let Some(path) = ancestor {
            let workspace_manifest = path.join("Cargo.toml");
            if let Ok(bytes) = std::fs::read_to_string(&workspace_manifest) {
                if let Ok(value) = toml::from_str::<toml::Value>(&bytes) {
                    inherited = value.get("workspace").and_then(|w| w.get("package"))
                        .and_then(|p| p.get("edition")).and_then(toml::Value::as_str).map(str::to_owned);
                    if inherited.is_some() { break; }
                }
            }
            ancestor = path.parent();
        }
        inherited.ok_or_else(|| format!("{} inherits edition but no workspace.package.edition was found", manifest.display()))?
    } else {
        return Err(format!("{} has an unsupported package.edition declaration", manifest.display()));
    };
    if !matches!(edition.as_str(), "2015" | "2018" | "2021" | "2024") {
        return Err(format!("{} declares unsupported rust edition {edition}", manifest.display()));
    }
    Ok(())
}

/// Route descriptions scripts and scenes use, and the one function that
/// turns them into product routes.
pub mod route {
    use super::{Fixture, resolve_package, resolve_symbol_kind};
    use crate::core::PackageId;
    use crate::navigation::{
        Coordinate, OrbitRoute, PackageLane, PackageRoute, ReleaseId, Route, SymbolRoute,
    };
    pub use crate::navigation::View;

    /// A route, as a script names it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Target {
        /// Home.
        Orbit,
        /// The world graph, no symbol.
        World,
        /// This repository's dependency tree (the Library page).
        Tree,
        /// Indexed answers for a query.
        Find(String),
        /// Compare two to four fixture packages.
        Compare(Vec<String>),
        /// A package by name or path (`present`), optionally at a release.
        Package {
            /// Name or path.
            id: String,
            /// Release (`0.3.0`).
            at: Option<String>,
        },
        /// A symbol by path (`present::glyph::RelationLabel`), optionally
        /// at a release, in a view. `kind=enum|struct|trait|...` and
        /// `path=src/file.rs` pin a fixture route to exact declaration evidence.
        Symbol {
            /// Path.
            id: String,
            /// Release (`0.3.0`).
            at: Option<String>,
            /// View.
            view: View,
            /// Optional exact declaration kind to disambiguate same-name items.
            kind: Option<backend_library::DeclarationKind>,
            /// Optional exact source path.
            path: Option<String>,
        },
    }

    fn options(kind: &str, words: &[&str], view: &mut Option<View>) -> Result<Option<String>, String> {
        let mut at = None;
        for option in words {
            match option.split_once('=') {
                Some(("view", "page")) if view.is_some() => *view = Some(View::Page),
                Some(("view", "code")) if view.is_some() => *view = Some(View::Code),
                Some(("view", "graph")) if view.is_some() => *view = Some(View::Graph),
                Some(("at", release)) if !release.is_empty() => at = Some(release.to_owned()),
                _ => return Err(format!("route {kind}: unknown option `{option}`")),
            }
        }
        Ok(at)
    }

    /// Parses `orbit`, `world`, `package ID [at=RELEASE]`, or
    /// `symbol ID [view=page|code|graph] [at=RELEASE] [kind=KIND] [path=PATH]`.
    ///
    /// # Errors
    /// Anything else, in words.
    pub fn parse(words: &str) -> Result<Target, String> {
        let parts = words.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["orbit"] => Ok(Target::Orbit),
            ["world"] => Ok(Target::World),
            ["tree"] => Ok(Target::Tree),
            ["find"] => Ok(Target::Find(String::new())),
            ["find", words @ ..] if !words.is_empty() => Ok(Target::Find(words.join(" "))),
            ["compare", packages @ ..] if (2..=4).contains(&packages.len()) => Ok(Target::Compare(packages.iter().map(|package| (*package).to_owned()).collect())),
            ["package", id, rest @ ..] => Ok(Target::Package {
                id: (*id).to_owned(),
                at: options("package", rest, &mut None)?,
            }),
            ["symbol", id, rest @ ..] => {
                let mut view = Some(View::Page);
                let mut kind = None;
                let mut path = None;
                let mut options_without_kind = Vec::with_capacity(rest.len());
                for option in rest {
                    if let Some(("kind", name)) = option.split_once('=') {
                        if kind.is_some() { return Err("route symbol: duplicate kind option".to_owned()); }
                        kind = Some(match name {
                            "class" => backend_library::DeclarationKind::Class,
                            "enum" => backend_library::DeclarationKind::Enum,
                            "function" => backend_library::DeclarationKind::Function,
                            "interface" => backend_library::DeclarationKind::Interface,
                            "struct" => backend_library::DeclarationKind::Struct,
                            "trait" => backend_library::DeclarationKind::Trait,
                            "type" => backend_library::DeclarationKind::Type,
                            "union" => backend_library::DeclarationKind::Union,
                            _ => return Err(format!("route symbol: unknown declaration kind `{name}`")),
                        });
                    } else if let Some(("path", source)) = option.split_once('=') {
                        if source.is_empty() || path.is_some() { return Err("route symbol: invalid or duplicate path option".to_owned()); }
                        path = Some(source.to_owned());
                    } else {
                        options_without_kind.push(*option);
                    }
                }
                let at = options("symbol", &options_without_kind, &mut view)?;
                Ok(Target::Symbol {
                    id: (*id).to_owned(),
                    at,
                    view: view.unwrap_or_default(),
                    kind,
                    path,
                })
            }
            _ => Err(format!(
                "route `{words}`: expected orbit, world, package ID [at=RELEASE], or symbol ID [view=page|code|graph] [at=RELEASE]"
            )),
        }
    }

    fn release(at: Option<&String>) -> Result<Option<ReleaseId>, String> {
        at.map(|release| {
            ReleaseId::new(release).map_err(|error| format!("release `{release}`: {error}"))
        })
        .transpose()
    }

    /// The product route for `target`: names resolve against the fixture
    /// index, everything else maps one to one onto the shell's route model.
    ///
    /// # Errors
    /// A name the index does not resolve to exactly one declaration.
    pub fn to_route(target: &Target, fixture: &Fixture) -> Result<Route, String> {
        match target {
            Target::Orbit => Ok(Route::Orbit(OrbitRoute::Home)),
            Target::World => Ok(Route::World),
            Target::Tree => {
                let root = super::browse_tree_root().canonicalize().map_err(|error| format!("route tree: {error}"))?;
                crate::core::LocalProjectId::from_path(&root)
                    .map(|project| Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Tree(project))))
                    .map_err(|error| format!("route tree: {error:?}"))
            }
            Target::Find(text) if text.is_empty() => Ok(Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::FindHome))),
            Target::Find(text) => crate::model::pages::SearchQuery::new(text, 200)
                .map(|query| Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query))))
                .map_err(|error| format!("route find: {error:?}")),
            Target::Compare(names) => {
                let packages = names.iter().map(|name| resolve_package(name, fixture)).collect::<Result<Vec<_>, _>>()?;
                let selection = crate::navigation::CompareSet::new(packages).map_err(|error| format!("route compare: {error:?}"))?;
                Ok(Route::Orbit(OrbitRoute::Browse(crate::navigation::BrowseRoute::Compare(selection))))
            }
            Target::Package { id, at } => {
                let package = resolve_package(id, fixture)?;
                Ok(Route::Package(PackageRoute {
                    project: None,
                    package: PackageId::new(package.as_str())
                        .map_err(|error| format!("route package {id}: {error}"))?,
                    lane: PackageLane::Overview,
                    selected: None,
                    at: release(at.as_ref())?,
                }))
            }
            Target::Symbol { id, at, view, kind, path } => {
                let (symbol, _line) = resolve_symbol_kind(id, fixture, *kind, path.as_deref())?;
                let package = symbol
                    .package()
                    .ok_or_else(|| format!("route symbol {id}: its coordinate names no package"))?;
                Ok(Route::Symbol(SymbolRoute {
                    project: None,
                    package: PackageId::new(package.as_str())
                        .map_err(|error| format!("route symbol {id}: {error}"))?,
                    id: Coordinate::new(symbol.as_str())
                        .map_err(|error| format!("route symbol {id}: {error}"))?,
                    at: release(at.as_ref())?,
                    view: *view,
                    // The code view opens at the declaration's own line.
                    line: None,
                    selected: None,
                }))
            }
        }
    }
}

fn read(fixture: &Fixture, request: &ReadRequest) -> Result<crate::model::pages::PageValue, String> {
    let mut reader = SessionReader::connect(fixture.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext {
        worker: 0,
        cancel: &cancel,
        outlines: &outlines,
    };
    reader
        .read(request, &context)
        .map_err(|error| format!("read {request:?}: {error:?}"))
}

/// The fixture package named `id` (a crate name or a path).
///
/// # Errors
/// No fixture package matches.
pub fn resolve_package(id: &str, fixture: &Fixture) -> Result<PackageRef, String> {
    let root = fixture
        .projects()
        .iter()
        .find(|project| project.ends_with(id) || project.to_str() == Some(id))
        .ok_or_else(|| {
            format!(
                "no fixture package `{id}` (have: {})",
                fixture
                    .projects()
                    .iter()
                    .filter_map(|project| project.file_name()?.to_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    PackageRef::parse(utf8(root)?).map_err(|error| format!("package {id}: {error:?}"))
}

/// The coordinate (and line) of `present::glyph::RelationLabel`: the last
/// segment is the name, the first names the fixture package, the ones in
/// between must appear in the declaration's path or coordinate.
///
/// # Errors
/// No declaration or more than one matches.
pub fn resolve_symbol(id: &str, fixture: &Fixture) -> Result<(SymbolRef, u32), String> {
    resolve_symbol_kind(id, fixture, None, None)
}

fn resolve_symbol_kind(
    id: &str,
    fixture: &Fixture,
    expected_kind: Option<backend_library::DeclarationKind>,
    expected_path: Option<&str>,
) -> Result<(SymbolRef, u32), String> {
    let segments = id.split("::").collect::<Vec<_>>();
    let (Some(name), Some(package)) = (segments.last(), segments.first()) else {
        return Err(format!("symbol `{id}`: expected package::path::Name"));
    };
    let package = resolve_package(package, fixture)?;
    // A package whose root failed to index has no outline to resolve against:
    // open the page for the coordinate the scene names, at line 1, so the
    // capture shows the app's own fault plate instead of dying at boot.
    if let Some(Refusal { reason, .. }) = fixture.failed().iter().find(|refusal| refusal.root.to_str() == Some(package.as_str())) {
        let file = expected_path.unwrap_or("lib.rs");
        report_once(format!(
            "symbol `{id}`: its package failed to index ({}); opening it unresolved at {file}:1 for the fault plate",
            reason.chars().take(90).collect::<String>()
        ));
        let coordinate = SymbolRef::new(&format!("{}::{file}:1::{name}", package.as_str()))
            .map_err(|error| format!("symbol `{id}`: {error:?}"))?;
        return Ok((coordinate, 1));
    }
    let package_ref = package.clone();
    let mut reader = SessionReader::connect(fixture.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
    let value = reader
        .read(&ReadRequest::Package(package_ref), &context)
        .map_err(|error| format!("read package outline for {package}: {error:?}"))?;
    let crate::model::pages::PageValue::Package(dossier) = value else {
        return Err(format!("package outline for {package} returned a non-package page"));
    };
    let outline = dossier.outline.known().ok_or_else(|| {
        format!("package outline for {package} is unavailable: {}", dossier.outline.gap().expect("unknown has a gap"))
    })?;
    if !outline.complete {
        return Err(format!(
            "package outline for {package} is incomplete after {} declarations; refusing exact symbol resolution",
            outline.count(),
        ));
    }
    let middle = &segments[1..segments.len().saturating_sub(1)];
    let matches = outline_symbol_candidates(outline, name, package.as_str(), middle, expected_kind, expected_path);
    choose_outline_symbol_candidate(&matches, id, package.as_str(), outline.complete, outline.count())
}

fn outline_symbol_candidates<'a>(
    outline: &'a crate::model::pages::OutlineTree,
    name: &str,
    package: &str,
    middle: &[&str],
    expected_kind: Option<backend_library::DeclarationKind>,
    expected_path: Option<&str>,
) -> Vec<&'a crate::model::pages::DeclRef> {
    outline
        .walk()
        .map(|node| &node.decl)
        .filter(|decl| {
            decl.name.as_ref() == name
                && expected_kind.is_none_or(|kind| decl.kind == Some(kind))
                && expected_path.is_none_or(|path| decl.path.as_deref() == Some(path))
                && decl.coordinate.package().is_some_and(|owner| owner.as_str() == package)
                && middle.iter().all(|segment| {
                    decl.path.as_deref().is_some_and(|path| path.contains(segment))
                        || decl.coordinate.as_str().contains(segment)
                })
        })
        .collect()
}

fn choose_outline_symbol_candidate(
    matches: &[&crate::model::pages::DeclRef],
    id: &str,
    package: &str,
    complete: bool,
    scanned: usize,
) -> Result<(SymbolRef, u32), String> {
    if !complete {
        return Err(format!(
            "package outline for {package} is incomplete after scanning {scanned} declarations; refusing exact symbol resolution"
        ));
    }
    match matches {
        [decl] => Ok((decl.coordinate.clone(), decl.line.unwrap_or(1))),
        [] => Err(format!(
            "symbol `{id}`: no exact candidate in package {package} after scanning {scanned} declarations of its complete outline"
        )),
        many => Err(format!(
            "symbol `{id}` is ambiguous: {}",
            many.iter()
                .map(|decl| decl.coordinate.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The booted graph of the window being captured.
struct Booted {
    graph: UiEntityGraph,
    fixture: &'static Fixture,
    shell: gpui::Entity<crate::shell::Shell>,
    /// Render counts and landed reads at the previous annotation.
    last: std::cell::Cell<(crate::shell::RenderCounts, u64)>,
}

impl Global for Booted {}

/// Worker-safe inputs assembled before a native window mounts the product.
/// It deliberately contains no GPUI entities; installing those remains a UI
/// thread operation in [`mount`].
pub(super) struct PreparedBoot {
    fixture: &'static Fixture,
    snapshot: AppSnapshot,
    actor: EngineActor,
    reads: ReadPool,
    route: Option<crate::navigation::Route>,
}

/// Boots the desktop on the fixture in `window` at `start`, with the
/// current facet's settings applied through the product's own intents.
///
/// # Errors
/// The fixture, the owner handshake, the actor, the read pool, or the
/// start route failing.
pub fn boot(start: &str, window: &mut Window, cx: &mut App) -> Result<AnyView, String> {
    mount(prepare(start)?, window, cx)
}

pub(super) fn prepare(start: &str) -> Result<PreparedBoot, String> {
    prepare_with_progress(start, |_| {})
}

pub(super) fn prepare_with_progress(
    start: &str,
    progress: impl Fn(&str),
) -> Result<PreparedBoot, String> {
    review_diag(&format!("prepare worker entered for {start}"));
    let report = |stage: &str| {
        review_diag(&format!("prepare: {stage}"));
        progress(stage);
    };
    report("Preparing the fixture owner");
    let fixture = fixture_with_progress(&report)?;
    report(&format!("Resolving {start}"));
    let endpoint = fixture.endpoint().to_path_buf();
    let bootstrapping = Instant::now();
    let mut subscription = LocalSubscriptionTransport::connect(&endpoint)
        .map_err(|error| format!("subscription: {error}"))?;
    let (view, revision) = subscription
        .bootstrap_root()
        .map_err(|error| format!("bootstrap root: {error}"))?;
    crate::runtime::trace::span("boot.bootstrap_root", bootstrapping, "harness subscription");
    let resolving = Instant::now();
    if view.root() != revision.root() {
        return Err("the owner returned mismatched startup identities".to_owned());
    }
    let target = route::parse(start)?;
    let route = (target != route::Target::Orbit)
        .then(|| route::to_route(&target, fixture))
        .transpose()?;
    let mut snapshot = AppSnapshot::empty(VersionedRoot::from_revision(1, revision, 0));
    // The tree route reads a project directly (never through the fixture
    // owner's index), so the shelf must be told the same project is open;
    // otherwise the Library head reads "0 projects" beside a body that
    // shows one, which is the workspace the reader is looking at read twice
    // and disagreeing.
    if let Some(crate::navigation::Route::Orbit(crate::navigation::OrbitRoute::Browse(
        crate::navigation::BrowseRoute::Tree(project),
    ))) = &route
    {
        snapshot = snapshot.with_workspace(tree_workspace(project));
    } else if target == route::Target::Orbit {
        // Orbit is the first-look start (J1, gui-plan §3 item 10): "your
        // project" is `toml_pin`, so Orbit's own boot needs a workspace the
        // same way the Tree route already gets one, or the centre has no
        // project to draw.
        snapshot = snapshot.with_workspace(orbit_workspace(fixture)?);
    }
    crate::runtime::trace::span("boot.resolve_route", resolving, start);
    let project = LocalProjectId::from_path(&fixture.projects()[0])
        .map_err(|error| format!("project identity: {error}"))?;
    let actor = EngineActor::start(LocalEngineClient::new(&endpoint, project), 32)
        .map_err(|error| format!("engine actor: {error}"))?;
    let reads = ReadPool::start(3, move |_| SessionReader::connect(&endpoint))
        .map_err(|error| format!("read pool: {error}"))?;
    report("Opening page");
    review_diag("prepare worker completed successfully");
    Ok(PreparedBoot { fixture, snapshot, actor, reads, route })
}

pub(super) fn mount(
    prepared: PreparedBoot,
    window: &mut Window,
    cx: &mut App,
) -> Result<AnyView, String> {
    review_diag("UI mount starting");
    let mounting = Instant::now();
    let PreparedBoot { fixture, snapshot, actor, reads, route } = prepared;
    let runtime = DesktopRuntime::new(snapshot, actor);
    let graph = UiEntityGraph::install_with_reads(cx, runtime, None, Some(reads));
    // The shot's facet becomes the product's settings.
    let facet = cx.facet();
    for intent in settings_intents(&facet) {
        graph.root.update(cx, |root, cx| root.dispatch(intent, cx));
    }
    if let Some(route) = route {
        graph
            .root
            .update(cx, |root, cx| root.dispatch(Intent::Navigate(route), cx));
    }
    gallery::declare_quiet(quiet, cx);
    gallery::declare_adapter(adapt, cx);
    gallery::declare_annotator(annotate, cx);
    gallery::declare_state(sample_state, cx);
    let shell = ROOT(&graph, window, cx);
    // Text size is a per-display zoom: set this window's display to the
    // shot's percent through the product's own intent.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = (facet.text_scale * 100.0).round() as u16;
    let display = shell.read(cx).display_key();
    graph
        .root
        .update(cx, |root, cx| root.dispatch(Intent::ZoomTo { display, percent }, cx));
    cx.set_global(Booted {
        graph,
        fixture,
        shell: shell.clone(),
        last: std::cell::Cell::new((crate::shell::RenderCounts::default(), 0)),
    });
    review_diag("UI mount completed");
    crate::runtime::trace::span("boot.mount", mounting, "UiEntityGraph + shell");
    Ok(shell.into())
}

/// Which regions re-rendered since the previous frame, and how many reads
/// landed: what a slow frame did.
fn annotate(cx: &mut App) -> String {
    // The latency ledger joins each played frame's real draw time to the
    // read marks (`NUDOX_TRACE`); the virtual clock alone cannot see I/O.
    crate::runtime::trace::frames_now();
    let note = annotate_regions(cx);
    crate::runtime::trace::mark("harness.frame", &note);
    note
}

fn annotate_regions(cx: &mut App) -> String {
    let Some(booted) = cx.try_global::<Booted>() else {
        return String::new();
    };
    let counts = booted.shell.read(cx).render_counts(cx);
    let landed = booted.graph.store.read(cx).stats().landed;
    let (before, landed_before) = booted.last.replace((counts, landed));
    let regions = [
        ("shell", counts.shell, before.shell),
        ("titlebar", counts.titlebar, before.titlebar),
        ("shelf", counts.shelf, before.shelf),
        ("reader", counts.reader, before.reader),
        ("status", counts.status, before.status),
        ("pins", counts.pins, before.pins),
        ("ask", counts.ask, before.ask),
    ]
    .into_iter()
    .filter(|(_, now, then)| now > then)
    .map(|(name, now, then)| format!("{name} x{}", now - then))
    .collect::<Vec<_>>();
    format!(
        "re-rendered [{}]{}; graph {}",
        regions.join(", "),
        if landed > landed_before {
            format!(", {} read(s) landed", landed - landed_before)
        } else {
            String::new()
        },
        booted.shell.read(cx).graph_report(cx)
    )
}

fn browse_route_state(route: &crate::navigation::BrowseRoute) -> gallery::json::Json {
    use facet::gallery::json::Json;
    use crate::navigation::BrowseRoute;
    match route {
        BrowseRoute::Tree(project) => Json::obj([
            ("kind", Json::str("tree")),
            ("project", Json::str(project.display_lossy())),
        ]),
        BrowseRoute::FindHome => Json::obj([("kind", Json::str("find-home"))]),
        BrowseRoute::Find(query) => Json::obj([
            ("kind", Json::str("find")),
            ("query", Json::str(query.text.to_string())),
            ("limit", Json::num(query.limit)),
        ]),
        BrowseRoute::Compare(selection) => Json::obj([
            ("kind", Json::str("compare")),
            (
                "packages",
                Json::Arr(
                    selection
                        .packages()
                        .iter()
                        .map(|package| Json::str(package.as_str()))
                        .collect(),
                ),
            ),
        ]),
    }
}

/// Samples the mounted product route and retained map after every probed draw.
fn sample_state(cx: &mut App, _: &facet::probe::Ledger) -> gallery::json::Json {
    use facet::gallery::json::Json;
    fn route_state(route: &crate::navigation::Route) -> Json {
        use crate::navigation::{OrbitRoute, Route};
        match route {
            Route::World => Json::obj([("kind", Json::str("world"))]),
            Route::Orbit(route) => Json::obj([
                ("kind", Json::str("orbit")),
                ("project", match route { OrbitRoute::Home | OrbitRoute::Browse(_) => Json::Null, OrbitRoute::Project(project) => Json::num(project.get().get() as f64) }),
                ("browse", match route { OrbitRoute::Browse(browse) => browse_route_state(browse), _ => Json::Null }),
            ]),
            Route::Package(route) => Json::obj([
                ("kind", Json::str("package")), ("package", Json::str(route.package.as_str())),
                ("lane", Json::str(format!("{:?}", route.lane))),
                ("release", route.at.as_ref().map_or(Json::Null, |at| Json::str(at.as_str()))),
            ]),
            Route::Symbol(route) => Json::obj([
                ("kind", Json::str("symbol")), ("package", Json::str(route.package.as_str())),
                ("symbol", Json::str(route.id.as_str())), ("view", Json::str(route.view.as_str())),
                ("release", route.at.as_ref().map_or(Json::Null, |at| Json::str(at.as_str()))),
                ("line", Json::opt(route.line)),
            ]),
        }
    }
    let Some(booted) = cx.try_global::<Booted>() else { return Json::Null };
    let snapshot = booted.graph.store.read(cx).snapshot();
    Json::obj([
        ("route", route_state(snapshot.route())),
        ("root", Json::str(format!("{:?}", snapshot.key()))),
        ("back", Json::num(snapshot.session().back.len() as f64)),
        ("graph", booted.shell.read(cx).graph_state(cx)),
        // What the pages are made of: a capture of preserved data says so.
        ("data", {
            let data = booted.fixture.provenance();
            Json::obj([("fresh", Json::num(data.fresh as f64)), ("preserved", Json::num(data.preserved as f64)), ("failed", Json::num(data.failed as f64))])
        }),
    ])
}

fn settings_intents(facet: &facet::Facet) -> Vec<Intent> {
    vec![
        Intent::SetAppearance(match facet.appearance {
            facet::Appearance::Abyss => AppearancePreference::Abyss,
            facet::Appearance::Glacier => AppearancePreference::Glacier,
        }),
        Intent::SetDensity(match facet.density {
            facet::Density::Comfortable => DensityPreference::Comfortable,
            facet::Density::Compact => DensityPreference::Compact,
            facet::Density::Dense => DensityPreference::Dense,
        }),
        Intent::SetContrast(match facet.contrast {
            facet::Contrast::Normal => ContrastPreference::Normal,
            facet::Contrast::High => ContrastPreference::High,
        }),
        Intent::SetMotion(if facet.reduced_motion {
            MotionPreference::Reduced
        } else {
            MotionPreference::Full
        }),
    ]
}

/// Every count of async work the app exposes that a frame can be waiting for,
/// by name. A frame captured while any is non-zero can show a placeholder
/// that a moment later is content. `runtime::offload::Memo`s count
/// themselves in one process-wide number, so a NEW `Memo` is waited for
/// without being listed here (`a_memo_nobody_registered_is_still_waited_for`);
/// detached work that is not a `Memo` adds one line.
fn in_flight(cx: &App) -> Vec<(&'static str, usize)> {
    vec![
        ("offload memos", crate::runtime::offload::in_flight()),
        ("declaration lines", crate::shell::bodies::symbol_lines_in_flight(cx)),
    ]
}

/// Nothing in flight: the read pool is empty, every finished read has
/// landed (drained here), the root has no engine work pending, and no
/// off-thread cache is still computing what a view asked for ([`in_flight`]).
fn quiet(cx: &mut App) -> bool {
    let Some(booted) = cx.try_global::<Booted>() else {
        return startup::settled(cx);
    };
    let (store, root, shell) = (booted.graph.store.clone(), booted.graph.root.clone(), booted.shell.clone());
    store.update(cx, |store, cx| {
        store.drain(cx);
    });
    let idle_pool = store.read(cx).pool_load() == (0, 0);
    idle_pool
        && !root.read(cx).has_pending_work()
        && shell.read(cx).graph_ready(cx)
        && !crate::runtime::fixture_world::is_loading(cx)
        && in_flight(cx).iter().all(|(_, count)| *count == 0)
}

/// Script acts without a platform event, through the product: settings
/// become intents, `route` navigates. ⌘/⌥ holds arrive as real modifier
/// events and the shell reveals on its own.
fn adapt(act: &Act, _window: &mut Window, cx: &mut App) {
    // After the platform dispatched `act` (a key, a move) and before a
    // route act navigates: the ledger's "input happened" instant.
    crate::runtime::trace::mark("harness.act", act);
    let Some(booted) = cx.try_global::<Booted>() else {
        return;
    };
    let (root, fixture, shell) = (booted.graph.root.clone(), booted.fixture, booted.shell.clone());
    let intent = match act {
        Act::TextScale { percent } => Intent::ZoomTo {
            display: shell.read(cx).display_key(),
            percent: *percent,
        },
        Act::Density { name } => Intent::SetDensity(match name.as_str() {
            "compact" => DensityPreference::Compact,
            "dense" => DensityPreference::Dense,
            _ => DensityPreference::Comfortable,
        }),
        Act::Theme { name } => Intent::SetAppearance(if name == "glacier" {
            AppearancePreference::Glacier
        } else {
            AppearancePreference::Abyss
        }),
        Act::Contrast { name } => Intent::SetContrast(if name == "high" {
            ContrastPreference::High
        } else {
            ContrastPreference::Normal
        }),
        Act::Motion { on } => Intent::SetMotion(if *on {
            MotionPreference::Full
        } else {
            MotionPreference::Reduced
        }),
        Act::Route { target } => {
            match route::parse(target).and_then(|target| route::to_route(&target, fixture)) {
                Ok(route) => Intent::Navigate(route),
                Err(error) => panic!("route {target}: {error}"),
            }
        }
        _ => return,
    };
    root.update(cx, |root, cx| root.dispatch(intent, cx));
}

fn build(start: &'static str, window: &mut Window, cx: &mut App) -> AnyView {
    if RESPONSIVE_STARTUP.load(std::sync::atomic::Ordering::Relaxed) {
        startup::open(start, window, cx)
    } else {
        boot(start, window, cx).unwrap_or_else(|error| panic!("desktop capture boot at `{start}`: {error}"))
    }
}

/// Every desktop scene: the real shell on the fixture, booted at a route.
#[must_use]
pub fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            id: "desktop-orbit",
            title: "The desktop at home (Orbit) on the fixture index",
            size: (1440, 900),
            build: |window, cx| build("orbit", window, cx),
        },
        Scene {
            id: "desktop-package",
            title: "The `present` package dossier",
            size: (1440, 900),
            build: |window, cx| build("package present", window, cx),
        },
        Scene {
            id: "desktop-package-toml",
            title: "The `toml` package dossier, at the release `toml_pin` pins",
            size: (1440, 900),
            build: |window, cx| build("package toml-0.8.23", window, cx),
        },
        Scene {
            id: "desktop-package-serde-json",
            title: "The `serde_json` package dossier: a library that re-exports its API from private modules",
            size: (1440, 900),
            build: |window, cx| build("package serde_json-1.0.151", window, cx),
        },
        Scene {
            id: "desktop-package-tokio",
            title: "The `tokio` package dossier: features, heads-up findings, 33 modules",
            size: (1440, 900),
            build: |window, cx| build("package tokio-1.53.1", window, cx),
        },
        Scene {
            id: "desktop-symbol",
            title: "present::glyph::RelationLabel, its page",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel", window, cx),
        },
        Scene {
            id: "desktop-code",
            title: "present::glyph::RelationLabel, its source",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel view=code", window, cx),
        },
        Scene {
            id: "desktop-graph",
            title: "present::glyph::RelationLabel, its graph",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel view=graph", window, cx),
        },
        Scene {
            id: "desktop-world",
            title: "The world graph, nothing selected",
            size: (1440, 900),
            build: |window, cx| build("world", window, cx),
        },
        Scene {
            id: "desktop-tree",
            title: "This repository's tree: the Library page",
            size: (1440, 900),
            build: |window, cx| build("tree", window, cx),
        },
        Scene {
            id: "desktop-find-home",
            title: "Find before a query, with real local package choices",
            size: (1440, 900),
            build: |window, cx| build("find", window, cx),
        },
        Scene {
            id: "desktop-find",
            title: "Find indexed package answers for from_str",
            size: (1440, 900),
            build: |window, cx| build("find from_str", window, cx),
        },
        Scene {
            id: "desktop-find-empty",
            title: "Find with no matching indexed declarations",
            size: (1440, 900),
            build: |window, cx| build("find no_such_symbol_w_pages_932", window, cx),
        },
        Scene {
            id: "desktop-compare",
            title: "Compare toml with toml_edit and basic-toml",
            size: (1440, 900),
            build: |window, cx| build("compare toml-0.8.23 toml_edit-0.22.27 basic-toml-0.1.10", window, cx),
        },
        Scene {
            id: "desktop-value",
            title: "toml::Value, structured symbol page",
            size: (1440, 900),
            build: |window, cx| build("symbol toml-0.8.23::value::Value kind=enum path=src/value.rs", window, cx),
        },
        Scene {
            id: "desktop-from-str",
            title: "serde_json::from_str, structured symbol page",
            size: (1440, 900),
            build: |window, cx| build("symbol serde_json-1.0.151::de::from_str kind=function path=src/de.rs", window, cx),
        },
        Scene {
            id: "desktop-serialize",
            title: "serde::Serialize, structured symbol page",
            size: (1440, 900),
            build: |window, cx| build("symbol serde_core-1.0.229::ser::Serialize kind=trait path=src/ser/mod.rs", window, cx),
        },
        Scene {
            id: "desktop-record-rust",
            title: "toml_datetime::Datetime: the record specimen, Rust",
            size: (1440, 900),
            build: |window, cx| build("symbol toml_datetime-0.6.11::datetime::Datetime kind=struct path=src/datetime.rs", window, cx),
        },
        Scene {
            id: "desktop-record-ts",
            title: "zod's $ZodIssueTooSmall: the record specimen, TypeScript",
            size: (1440, 900),
            build: |window, cx| build("symbol zod::$ZodIssueTooSmall path=src/errors.ts", window, cx),
        },
        Scene {
            id: "desktop-record-go",
            title: "pflag.Flag: the record specimen, Go",
            size: (1440, 900),
            build: |window, cx| build("symbol pflag::Flag path=flag.go", window, cx),
        },
        Scene {
            id: "desktop-smallvec",
            title: "smallvec::SmallVec, structured symbol page",
            size: (1440, 900),
            build: |window, cx| build("symbol smallvec-1.16.0::SmallVec kind=struct path=src/lib.rs", window, cx),
        },
        Scene {
            id: "desktop-error",
            title: "serde_json::Error, structured symbol page",
            size: (1440, 900),
            build: |window, cx| build("symbol serde_json-1.0.151::error::Error kind=struct path=src/error.rs", window, cx),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::route::{Target, View, parse};
    use super::{browse_tree_root, repo};

    #[test]
    fn route_words_parse_into_targets_and_reject_the_rest() {
        assert_eq!(parse("orbit"), Ok(Target::Orbit));
        assert_eq!(parse("world"), Ok(Target::World));
        assert_eq!(parse("tree"), Ok(Target::Tree));
        assert_eq!(parse("find parse toml"), Ok(Target::Find("parse toml".into())));
        assert_eq!(parse("compare toml present"), Ok(Target::Compare(vec!["toml".into(), "present".into()])));
        assert_eq!(
            parse("compare toml-0.8.23 toml_edit-0.22.27 basic-toml-0.1.10"),
            Ok(Target::Compare(vec![
                "toml-0.8.23".into(),
                "toml_edit-0.22.27".into(),
                "basic-toml-0.1.10".into(),
            ]))
        );
        assert_eq!(
            parse("symbol serde_json-1.0.151::de::from_str"),
            Ok(Target::Symbol {
                id: "serde_json-1.0.151::de::from_str".to_owned(),
                at: None,
                view: View::Page,
                kind: None,
                path: None,
            })
        );
        assert_eq!(
            parse("symbol serde_core-1.0.229::ser::Serialize"),
            Ok(Target::Symbol {
                id: "serde_core-1.0.229::ser::Serialize".to_owned(),
                at: None,
                view: View::Page,
                kind: None,
                path: None,
            })
        );
        assert_eq!(
            parse("symbol toml-0.8.23::value::Value kind=enum"),
            Ok(Target::Symbol {
                id: "toml-0.8.23::value::Value".to_owned(),
                at: None,
                view: View::Page,
                kind: Some(backend_library::DeclarationKind::Enum),
                path: None,
            })
        );
        assert_eq!(
            parse("symbol toml-0.8.23::value::Value kind=enum path=src/value.rs"),
            Ok(Target::Symbol {
                id: "toml-0.8.23::value::Value".to_owned(),
                at: None,
                view: View::Page,
                kind: Some(backend_library::DeclarationKind::Enum),
                path: Some("src/value.rs".to_owned()),
            })
        );
        assert!(parse("symbol X kind=unknown").is_err());
        assert!(parse("symbol X kind=enum kind=struct").is_err());
        assert!(parse("symbol X path=one.rs path=two.rs").is_err());
        assert_eq!(parse("find"), Ok(Target::Find(String::new())));
        assert!(parse("compare toml").is_err());
        assert!(parse("compare a b c d e").is_err());
        assert_eq!(
            parse("package present at=0.1.0"),
            Ok(Target::Package {
                id: "present".to_owned(),
                at: Some("0.1.0".to_owned()),
            })
        );
        assert!(parse("package present view=graph").is_err());
        assert_eq!(
            parse("symbol present::glyph::RelationLabel view=graph at=0.3.0"),
            Ok(Target::Symbol {
                id: "present::glyph::RelationLabel".to_owned(),
                at: Some("0.3.0".to_owned()),
                view: View::Graph,
                kind: None,
                path: None,
            })
        );
        assert!(parse("symbol X view=map").is_err());
        assert!(parse("package").is_err());
        assert!(parse("elsewhere").is_err());
    }

    #[test]
    fn sampled_browse_route_keeps_query_and_ordered_package_identity() {
        use crate::navigation::{BrowseRoute, CompareSet};
        use crate::model::pages::{PackageRef, SearchQuery};

        let find = BrowseRoute::Find(SearchQuery::new("from_str", 200).expect("query"));
        assert_eq!(
            super::browse_route_state(&find).to_string(),
            r#"{"kind":"find","query":"from_str","limit":200}"#,
        );

        let first = PackageRef::parse("pkg:cargo/toml@0.8.23").expect("first package");
        let second = PackageRef::parse("pkg:cargo/toml_edit@0.22.27").expect("second package");
        let route = BrowseRoute::Compare(
            CompareSet::new([first.clone(), second.clone()]).expect("two packages"),
        );
        assert_eq!(
            super::browse_route_state(&route).to_string(),
            r#"{"kind":"compare","packages":["pkg:cargo/toml@0.8.23","pkg:cargo/toml_edit@0.22.27"]}"#,
        );
        let reversed = BrowseRoute::Compare(
            CompareSet::new([second, first]).expect("two packages"),
        );
        assert_ne!(super::browse_route_state(&route), super::browse_route_state(&reversed));
    }

    fn outline_node(
        package: &str,
        name: &str,
        kind: backend_library::DeclarationKind,
        path: &str,
        line: u32,
    ) -> crate::model::pages::OutlineNode {
        let decl = crate::model::pages::DeclRef::from_label(
            &format!("{package}::{path}:{line}::{name}"),
            None,
            Some(kind),
            Some((path, line)),
        ).expect("synthetic declaration");
        crate::model::pages::OutlineNode { decl, children: std::sync::Arc::from([]) }
    }

    #[test]
    fn exact_symbol_route_uses_complete_package_outline_not_ranked_search() {
        use super::{choose_outline_symbol_candidate, outline_symbol_candidates};
        use crate::model::pages::OutlineTree;

        let package = "pkg:cargo/toml@0.8.23";
        let outline = OutlineTree {
            roots: vec![
                outline_node(package, "Value", backend_library::DeclarationKind::Type, "src/value.rs", 1382),
                outline_node(package, "Value", backend_library::DeclarationKind::Enum, "src/value.rs", 25),
                outline_node(package, "Value", backend_library::DeclarationKind::Enum, "src/other.rs", 100),
                outline_node("pkg:cargo/toml_edit@0.22.27", "Value", backend_library::DeclarationKind::Enum, "src/value.rs", 8),
            ].into(),
            complete: true,
        };
        let broad = outline_symbol_candidates(&outline, "Value", package, &[], None, None);
        assert_eq!(broad.len(), 3);
        assert!(choose_outline_symbol_candidate(&broad, "toml::Value", package, true, outline.count())
            .expect_err("same-name declarations remain ambiguous").contains("ambiguous"));

        let exact = outline_symbol_candidates(
            &outline,
            "Value",
            package,
            &["value"],
            Some(backend_library::DeclarationKind::Enum),
            Some("src/value.rs"),
        );
        let selected = choose_outline_symbol_candidate(
            &exact,
            "toml::value::Value kind=enum path=src/value.rs",
            package,
            true,
            outline.count(),
        ).expect("exact package, path and kind selector");
        assert_eq!(selected.1, 25);
        assert_eq!(selected.0.as_str(), "pkg:cargo/toml@0.8.23::src/value.rs:25::Value");
    }

    #[test]
    fn exact_symbol_route_rejects_partial_or_ambiguous_package_outlines() {
        use super::{choose_outline_symbol_candidate, outline_symbol_candidates};
        use crate::model::pages::OutlineTree;

        let package = "pkg:cargo/serde_json@1.0.151";
        let node = outline_node(package, "Error", backend_library::DeclarationKind::Struct, "src/error.rs", 17);
        let partial = OutlineTree { roots: vec![node.clone()].into(), complete: false };
        let match_on_partial = outline_symbol_candidates(
            &partial, "Error", package, &["error"],
            Some(backend_library::DeclarationKind::Struct), Some("src/error.rs"),
        );
        assert!(choose_outline_symbol_candidate(&match_on_partial, "serde_json::error::Error", package, partial.complete, partial.count())
            .expect_err("one hit in a partial outline cannot prove uniqueness").contains("incomplete"));

        let complete = OutlineTree {
            roots: vec![
                node,
                outline_node(package, "Error", backend_library::DeclarationKind::Struct, "src/error.rs", 18),
            ].into(),
            complete: true,
        };
        let ambiguous = outline_symbol_candidates(
            &complete, "Error", package, &["error"],
            Some(backend_library::DeclarationKind::Struct), Some("src/error.rs"),
        );
        assert!(choose_outline_symbol_candidate(&ambiguous, "serde_json::error::Error", package, complete.complete, complete.count())
            .expect_err("two exact declarations keep the route ambiguous").contains("ambiguous"));
    }

    #[test]
    fn page2_scenes_cover_each_pinned_symbol_and_the_three_package_comparison() {
        let ids = super::scenes()
            .into_iter()
            .map(|scene| scene.id)
            .collect::<Vec<_>>();
        for id in [
            "desktop-value",
            "desktop-from-str",
            "desktop-serialize",
            "desktop-smallvec",
            "desktop-error",
            "desktop-compare",
        ] {
            assert!(ids.contains(&id), "missing fixture scene {id}");
        }
    }

    #[test]
    fn only_a_refused_workspace_falls_back_never_one_another_process_holds() {
        use super::state_refused;
        use crate::HostError;
        use backend_local_service::{ListenerError, ProcessError};

        let contended = |refusal| HostError::Contended {
            endpoint: "/tmp/nx-harness-test.sock".into(),
            data: "/tmp/data".into(),
            refusal: Box::new(refusal),
        };
        // An index another build wrote: no owner will ever answer, so switch at once.
        assert!(state_refused(&contended(ProcessError::Profile("unsupported view DTO version".to_owned()))));
        // A live owner still opening the same workspace: wait for it instead.
        assert!(!state_refused(&contended(ProcessError::Profile(format!("workspace error: {}", super::HELD_LOCK_WORDS)))));
        assert!(!state_refused(&contended(ProcessError::Listener(ListenerError::AlreadyRunning))));
        assert!(!state_refused(&HostError::UnsupportedPathEncoding { path: "/tmp".into() }));
    }

    #[test]
    fn the_engines_held_lock_error_is_still_named_as_the_harness_reads_it() {
        let source = std::fs::read_to_string(super::repo().join("crates/engine/src/workspace/owner/error.rs")).expect("the engine's workspace error");
        assert!(
            source.contains(&format!("    {},", super::HELD_LOCK_WORDS)) && source.contains("write!(f, \"workspace error: {self:?}\")"),
            "`WorkspaceError` no longer has a variant named {} shown through `{{self:?}}`: `state_refused` would take a live lock for a refusal",
            super::HELD_LOCK_WORDS
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_state_dir_the_harness_creates_is_one_the_owner_accepts() {
        use super::{fallback_state, private_dir, repo};
        use std::os::unix::fs::PermissionsExt as _;

        // The owner refuses a private state directory that is group- or
        // world-accessible (`crates/platform/durable.rs`); the umask alone
        // makes `create_dir_all` produce 0755.
        let root = std::env::temp_dir().join(format!("nudox-state-dir-{}", std::process::id()));
        let data = root.join("state/data");
        private_dir(&data).expect("create private dirs");
        for dir in [&root, &root.join("state"), &data] {
            let mode = std::fs::metadata(dir).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
        }
        std::fs::remove_dir_all(&root).expect("remove the test's own scratch");
        // The fallback lives beside the shared index, named for the refused one.
        assert_eq!(fallback_state(&repo(), &repo().join(".local/harness/desktop")), repo().join(".local/harness/fallback-desktop"));
    }

    /// The owner creates its own subdirectories (`compiler/`, `forge/`,
    /// `registry*/`) with the process's umask and then refuses to write
    /// beneath any that is group- or world-accessible: under a shell's usual
    /// 022 the first boot works and every later boot of the same state
    /// directory is refused. `private_umask` is the fix; the directory the
    /// process makes after it is the proof.
    #[cfg(unix)]
    #[test]
    fn a_directory_the_process_makes_after_private_umask_is_one_the_owner_accepts() {
        use rustix::fs::Mode;
        use std::os::unix::fs::PermissionsExt as _;

        /// Puts the process's umask back, even if the test panics.
        struct Restore(Mode);
        impl Drop for Restore {
            fn drop(&mut self) {
                rustix::process::umask(self.0);
            }
        }
        let _restore = Restore(rustix::process::umask(Mode::from_bits_truncate(0o022)));
        super::private_umask();
        let dir = std::env::temp_dir().join(format!("nudox-umask-{}", std::process::id()));
        std::fs::create_dir(&dir).expect("a directory made by the process");
        let mode = std::fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o700, "a directory made after `private_umask` is {mode:o}: the owner refuses it");
    }

    #[test]
    fn what_the_fixtures_pages_are_made_of_counts_fresh_preserved_and_failed_roots() {
        use super::{Provenance, provenance};
        assert_eq!(provenance(14, 0, 0), Provenance { fresh: 14, preserved: 0, failed: 0 });
        assert_eq!(provenance(14, 13, 1), Provenance { fresh: 0, preserved: 13, failed: 1 });
        assert_eq!(provenance(14, 3, 2), Provenance { fresh: 9, preserved: 3, failed: 2 });
    }

    /// An off-thread cache nobody told the harness about is still waited for:
    /// `Memo`s count themselves, so `quiet` sees a flight the moment it
    /// starts and a capture never draws the placeholder a value replaces.
    #[gpui::test]
    fn a_memo_nobody_registered_is_still_waited_for(cx: &mut gpui::TestAppContext) {
        use super::in_flight;
        use crate::runtime::offload::{Asker, Memo};

        let memo: Memo<u32, u32> = Memo::new(std::num::NonZeroUsize::new(4).expect("a capacity"), |key| key + 1);
        cx.update(|cx| {
            let _ = memo.ask(&7, Asker::Everyone, cx);
        });
        let waiting = cx.update(|cx| in_flight(cx));
        let memos = waiting.iter().find(|(name, _)| *name == "offload memos").map(|(_, count)| *count);
        assert!(memos.is_some_and(|count| count >= 1), "a memo's flight is not counted: {waiting:?}");
        cx.run_until_parked();
        assert_eq!(memo.peek(&7).as_deref(), Some(&8), "the flight lands when the executor runs");
    }

    /// `tree` must resolve to the pinned two-member fixture
    /// (`apps/desktop/tests/fixtures/browse_tree`, proven deterministic by
    /// `shell::browse_tests`), never to the live repository: the lead's walk
    /// of the 16:30 harness build caught the body reading the live tree
    /// ("Your 45 packages…, and 884 in all", which drifts with every commit)
    /// beside a shelf that still read the fixture ("Library 0 projects").
    #[test]
    fn the_tree_route_is_pinned_to_the_fixture_workspace_not_the_live_repository() {
        let root = browse_tree_root();
        assert!(
            root.ends_with("apps/desktop/tests/fixtures/browse_tree"),
            "{}",
            root.display()
        );
        assert_ne!(root, repo(), "the tree route must never read the live repository directly");
    }
}
