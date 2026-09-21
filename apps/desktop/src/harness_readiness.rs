//! Production readiness for the visual desktop harness.
//!
//! A screenshot scenario is allowed to touch a reader route only after the
//! local service has admitted the source request and the exact route anchor
//! is visible in one committed view revision.  This module owns that small
//! state machine so the desktop adapter does not grow one wait loop per page.

use backend_client::{ClientError, Session};
use backend_library::{
    CommandReply, Row, RowId, SurfaceCommand, SurfaceReply, ViewRoot, encode_id, package_key,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

/// The typed reader surface whose prerequisites the harness must prove.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReaderSurface {
    /// Browse the local shelf.
    Browse,
    /// Read one local project.
    Project,
    /// Read one registry package.
    Package,
    /// Read one local declaration.
    Declaration,
    /// Read source for one local declaration.
    Source,
    /// Read one package's source/code route.
    Code,
    /// Read package documentation.
    Docs,
    /// Read one declaration's graph.
    Graph,
    /// Read a package's dependencies.
    Dependencies,
    /// Read packages depending on one package.
    Dependents,
    /// Read package releases.
    Releases,
    /// Read package security facts.
    Security,
    /// Search source/code symbols.
    CodeSearch,
}

impl ReaderSurface {
    /// Returns whether this route needs the local source projection.
    #[must_use]
    pub const fn needs_local_index(self) -> bool {
        matches!(
            self,
            Self::Project
                | Self::Declaration
                | Self::Source
                | Self::Code
                | Self::Docs
                | Self::Graph
                | Self::CodeSearch
        )
    }

    /// Returns whether this route needs a declaration anchor.
    #[must_use]
    pub const fn needs_declaration(self) -> bool {
        matches!(
            self,
            Self::Declaration
                | Self::Source
                | Self::Code
                | Self::Docs
                | Self::Graph
                | Self::CodeSearch
        )
    }

    /// Returns whether this route needs the local registry page.
    #[must_use]
    pub const fn needs_registry(self) -> bool {
        matches!(
            self,
            Self::Package | Self::Dependencies | Self::Dependents | Self::Releases | Self::Security
        )
    }
}

/// Bounds for the observable readiness loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadinessOptions {
    /// Total time allowed for index/route readiness observations.
    pub timeout: Duration,
    /// Delay between bounded service observations.
    pub poll_interval: Duration,
}

impl Default for ReadinessOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(80),
        }
    }
}

impl ReadinessOptions {
    /// Reads optional bounded test controls from the process environment.
    ///
    /// The normal desktop has no reason to set these values. They are useful
    /// for a deterministic failure lane, where a short timeout proves the
    /// harness fails closed rather than waiting forever.
    pub fn from_env() -> Result<Self, ReadinessError> {
        let defaults = Self::default();
        let timeout = duration_env(
            [
                "NUDOX_GUI_HARNESS_READINESS_TIMEOUT_MS",
                "BACKEND_GUI_HARNESS_READINESS_TIMEOUT_MS",
            ],
            defaults.timeout,
            "readiness timeout",
        )?;
        let poll_interval = duration_env(
            [
                "NUDOX_GUI_HARNESS_READINESS_POLL_MS",
                "BACKEND_GUI_HARNESS_READINESS_POLL_MS",
            ],
            defaults.poll_interval,
            "readiness poll interval",
        )?;
        if timeout.is_zero() || poll_interval.is_zero() {
            return Err(ReadinessError::InvalidOptions(
                "readiness timeout and poll interval must be positive".to_owned(),
            ));
        }
        Ok(Self {
            timeout,
            poll_interval,
        })
    }
}

fn duration_env<const N: usize>(
    names: [&str; N],
    default: Duration,
    label: &str,
) -> Result<Duration, ReadinessError> {
    let Some((name, value)) = names
        .into_iter()
        .find_map(|name| std::env::var(name).ok().map(|value| (name, value)))
    else {
        return Ok(default);
    };
    let millis = value.parse::<u64>().map_err(|_| {
        ReadinessError::InvalidOptions(format!("{name} must be an integer number of milliseconds"))
    })?;
    if millis == 0 {
        return Err(ReadinessError::InvalidOptions(format!(
            "{label} from {name} must be positive"
        )));
    }
    Ok(Duration::from_millis(millis))
}

/// Stable identity of an admitted view revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RevisionIdentity {
    /// Content revision of the view.
    pub revision: String,
    /// Canonical row-relation root of the view.
    pub root: String,
}

impl RevisionIdentity {
    fn from_root(root: &ViewRoot) -> Self {
        Self {
            revision: encode_id(root.version().as_bytes()),
            root: encode_id(root.root().as_bytes()),
        }
    }
}

/// One selected declaration or package identity, retained by stable key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SelectedIdentity {
    /// Canonical coordinate exposed by the producer.
    pub coordinate: String,
    /// Stable row key used by the producer relation.
    pub stable_id: String,
}

/// One observable readiness event retained in the run report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadinessEvent {
    /// Poll number at which this event was observed.
    pub poll: u32,
    /// Milliseconds elapsed since readiness began.
    pub elapsed_ms: u128,
    /// Stable event kind.
    pub kind: String,
    /// Revision observed with the event, when a view reply supplied one.
    pub revision: Option<RevisionIdentity>,
    /// Owner root observed by a constant-size health reply.
    ///
    /// Health deliberately does not carry a `ViewVersion`, so this field
    /// keeps the root evidence without manufacturing a revision identity.
    pub observed_root: Option<String>,
    /// Bounded detail useful when a route waits or fails.
    pub detail: Option<String>,
}

/// Proof attached to one live capture before the GPUI route is applied.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadinessReport {
    /// Project/workspace root selected by the desktop host.
    pub workspace_root: PathBuf,
    /// Typed route requested by the capture.
    pub requested_surface: ReaderSurface,
    /// Revision before any index request was made.
    pub requested_revision: RevisionIdentity,
    /// Revision after the route prerequisite was observed.
    pub admitted_revision: RevisionIdentity,
    /// The requested relation root, repeated as a report-friendly field.
    pub requested_root: String,
    /// The admitted relation root, repeated as a report-friendly field.
    pub admitted_root: String,
    /// Whether the existing exact source projection was reused.
    pub reused_existing_projection: bool,
    /// Whether the production index/add API was requested.
    pub index_requested: bool,
    /// Bounded wall duration of readiness observations.
    pub duration_ms: u128,
    /// Number of service polls performed.
    pub polls: u32,
    /// Observable readiness timeline.
    pub events: Vec<ReadinessEvent>,
    /// Declaration selected from the admitted route prerequisite.
    pub selected_declaration: Option<SelectedIdentity>,
    /// Package selected from the admitted route prerequisite.
    pub selected_package: Option<SelectedIdentity>,
}

impl ReadinessReport {
    /// Checks that a later subscription bootstrap retained the admitted root.
    ///
    /// The full `ViewVersion` is supplied by the subscription bootstrap, not
    /// by the constant-size health reply used during polling. Call
    /// [`Self::pin_admitted_root`] after this check to retain that exact
    /// version in the report.
    pub fn matches_root(&self, root: &ViewRoot) -> bool {
        self.admitted_root == encode_id(root.root().as_bytes())
    }

    /// Pins the report to the exact full identity hydrated by a subscription.
    ///
    /// # Errors
    /// Returns [`ReadinessError::RevisionChanged`] when the subscription root
    /// differs from the route prerequisite observed by the readiness loop.
    pub fn pin_admitted_root(&mut self, root: &ViewRoot) -> Result<(), ReadinessError> {
        if !self.matches_root(root) {
            return Err(ReadinessError::RevisionChanged {
                expected: self.admitted_revision.clone(),
                observed: RevisionIdentity::from_root(root),
            });
        }
        self.admitted_revision = RevisionIdentity::from_root(root);
        self.admitted_root = self.admitted_revision.root.clone();
        self.events.push(ReadinessEvent {
            poll: self.polls,
            elapsed_ms: self.duration_ms,
            kind: "subscription-root-admitted".to_owned(),
            revision: Some(self.admitted_revision.clone()),
            observed_root: Some(self.admitted_root.clone()),
            detail: None,
        });
        Ok(())
    }
}

/// The typed reason a bounded readiness wait could not complete.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ReadinessTimeoutCause {
    /// The owner never published the requested index rows.
    IndexNotCommitted,
    /// The owner kept changing revisions before a route could be pinned.
    RevisionDidNotSettle,
    /// A local project row was never admitted.
    ProjectAnchorMissing,
    /// A declaration row was never admitted.
    DeclarationAnchorMissing,
    /// The registry never supplied a package row.
    PackageAnchorMissing,
}

impl std::fmt::Display for ReadinessTimeoutCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::IndexNotCommitted => "index-not-committed",
            Self::RevisionDidNotSettle => "revision-did-not-settle",
            Self::ProjectAnchorMissing => "project-anchor-missing",
            Self::DeclarationAnchorMissing => "declaration-anchor-missing",
            Self::PackageAnchorMissing => "package-anchor-missing",
        })
    }
}

/// Failure from the production readiness seam.
#[derive(Debug, Error)]
pub enum ReadinessError {
    /// Readiness controls were malformed.
    #[error("readiness options are invalid: {0}")]
    InvalidOptions(String),
    /// The production service rejected or failed an observation.
    #[error("readiness service observation failed during {operation}: {source}")]
    Service {
        /// Bounded operation that failed.
        operation: &'static str,
        /// Typed client failure.
        source: ClientError,
    },
    /// The route never reached its typed prerequisite before the deadline.
    #[error(
        "readiness timed out for {surface:?} after {duration_ms}ms and {polls} polls ({cause}); last revision={last_revision:?}"
    )]
    Timeout {
        /// Requested route.
        surface: ReaderSurface,
        /// Elapsed bounded duration.
        duration_ms: u128,
        /// Number of bounded observations.
        polls: u32,
        /// Typed wait reason.
        cause: ReadinessTimeoutCause,
        /// Last exact revision observed, if any.
        last_revision: Option<RevisionIdentity>,
    },
    /// A final subscription bootstrap did not retain the route's admitted root.
    #[error(
        "readiness revision changed before capture: expected {expected:?}, observed {observed:?}"
    )]
    RevisionChanged {
        /// Root admitted by the route query.
        expected: RevisionIdentity,
        /// Root hydrated for the GPUI model.
        observed: RevisionIdentity,
    },
    /// A successful service reply had an unexpected typed shape.
    #[error("readiness {operation} reply changed shape")]
    ReplyShape {
        /// Operation whose reply was malformed.
        operation: &'static str,
    },
}

struct LocalObservation {
    owner_root: backend_library::ViewStateRoot,
    declaration: Option<SelectedIdentity>,
    package: Option<SelectedIdentity>,
}

/// Requests missing local indexing and waits for a route prerequisite.
///
/// The service request and every readiness observation use the same client
/// command API as CLI/MCP.  The returned report is the only value a capture
/// needs before constructing its production `Workspace`.
pub fn await_readiness(
    session: &mut Session,
    workspace_root: &Path,
    surface: ReaderSurface,
    requested_root: &ViewRoot,
    options: ReadinessOptions,
) -> Result<ReadinessReport, ReadinessError> {
    let started = Instant::now();
    let requested_revision = RevisionIdentity::from_root(requested_root);
    let workspace_root = workspace_root.to_path_buf();
    let workspace_coordinate = workspace_root.to_string_lossy().into_owned();
    let mut events = Vec::new();
    let mut polls = 0_u32;
    let mut index_requested = false;
    let mut reused_existing_projection = false;
    let mut observed_revision = false;
    let mut revision_changed = false;

    event(
        &mut events,
        0,
        started,
        "requested",
        Some(requested_revision.clone()),
        Some(format!("surface={surface:?}")),
    );

    if surface.needs_registry() {
        return await_registry(
            session,
            workspace_root,
            surface,
            requested_revision,
            options,
            started,
            events,
        );
    }

    if !surface.needs_local_index() {
        return Ok(report(
            workspace_root,
            surface,
            requested_revision.clone(),
            requested_revision.root.clone(),
            started,
            polls,
            events,
            reused_existing_projection,
            index_requested,
            None,
            None,
        ));
    }

    // A row already present for this exact project means an index intent has
    // been admitted or a ready source snapshot is already durable.  The
    // service owns source-version comparison; the harness must not submit a
    // second add while that exact identity is settling.
    let existing = existing_local_anchor(requested_root, &workspace_coordinate);
    if existing.has_any_project_row {
        reused_existing_projection = true;
        event(
            &mut events,
            0,
            started,
            "reuse-existing-source",
            Some(requested_revision.clone()),
            Some("source identity already admitted by local service".to_owned()),
        );
    } else {
        session
            .index(&workspace_coordinate)
            .map_err(|source| ReadinessError::Service {
                operation: "index-request",
                source,
            })?;
        index_requested = true;
        event(
            &mut events,
            0,
            started,
            "index-request-accepted",
            None,
            Some(workspace_coordinate.clone()),
        );
    }

    loop {
        if started.elapsed() >= options.timeout {
            return Err(ReadinessError::Timeout {
                surface,
                duration_ms: started.elapsed().as_millis(),
                polls,
                cause: timeout_cause(
                    surface,
                    index_requested,
                    observed_revision,
                    revision_changed,
                ),
                // Health supplies only a root; the exact ViewVersion is
                // pinned by the subscription bootstrap on success.
                last_revision: None,
            });
        }
        polls = polls.saturating_add(1);
        let health = session.health().map_err(|source| ReadinessError::Service {
            operation: "health-readiness",
            source,
        })?;
        let health_root = health.revision().root();
        event_with_root(
            &mut events,
            polls,
            started,
            "health-observed",
            None,
            Some(encode_id(health_root.as_bytes())),
            Some(format!("rows={}", health.row_count())),
        );

        let local = match observe_local(session, surface, &workspace_coordinate, health_root)? {
            Some(local) => local,
            None => {
                revision_changed = true;
                event(
                    &mut events,
                    polls,
                    started,
                    "revision-changed-during-observation",
                    None,
                    Some("retrying against the owner's next committed root".to_owned()),
                );
                continue;
            }
        };
        let owner_root = encode_id(local.owner_root.as_bytes());
        observed_revision = true;
        event_with_root(
            &mut events,
            polls,
            started,
            "route-prerequisites-observed",
            None,
            Some(owner_root.clone()),
            Some(format!(
                "declaration={} package={}",
                local.declaration.is_some(),
                local.package.is_some()
            )),
        );
        let ready = if surface.needs_declaration() {
            local.declaration.is_some()
        } else {
            local.package.is_some()
        };
        if ready {
            event_with_root(
                &mut events,
                polls,
                started,
                "route-ready",
                None,
                Some(owner_root.clone()),
                None,
            );
            return Ok(report(
                workspace_root,
                surface,
                requested_revision,
                owner_root,
                started,
                polls,
                events,
                reused_existing_projection,
                index_requested,
                local.declaration,
                local.package,
            ));
        }
        thread::sleep(
            options
                .poll_interval
                .min(options.timeout.saturating_sub(started.elapsed())),
        );
    }
}

fn await_registry(
    session: &mut Session,
    workspace_root: PathBuf,
    surface: ReaderSurface,
    requested_revision: RevisionIdentity,
    options: ReadinessOptions,
    started: Instant,
    mut events: Vec<ReadinessEvent>,
) -> Result<ReadinessReport, ReadinessError> {
    let mut polls = 0_u32;
    let mut last_revision = None;
    let mut revision_changed = false;
    loop {
        if started.elapsed() >= options.timeout {
            return Err(ReadinessError::Timeout {
                surface,
                duration_ms: started.elapsed().as_millis(),
                polls,
                // A changing owner root is a distinct failure from an
                // empty registry page; the final bootstrap cannot safely
                // choose a route while the requested revision keeps moving.
                cause: if revision_changed {
                    ReadinessTimeoutCause::RevisionDidNotSettle
                } else {
                    ReadinessTimeoutCause::PackageAnchorMissing
                },
                last_revision,
            });
        }
        polls = polls.saturating_add(1);
        let health = session.health().map_err(|source| ReadinessError::Service {
            operation: "health-readiness",
            source,
        })?;
        let health_root = health.revision().root();
        let observed_root = encode_id(health_root.as_bytes());
        event_with_root(
            &mut events,
            polls,
            started,
            "health-observed",
            None,
            Some(observed_root.clone()),
            Some(format!("rows={}", health.row_count())),
        );
        // Registry records are owned by the same local service, but the
        // product surface does not expose them as the local package-view
        // snapshot.  Keep the capture pinned to the requested owner root;
        // a concurrent local index publication must be observed by the final
        // subscription bootstrap and rejected if it changes the root.
        if observed_root != requested_revision.root {
            revision_changed = true;
            event(
                &mut events,
                polls,
                started,
                "registry-root-changed",
                None,
                Some("waiting for the requested committed root".to_owned()),
            );
            thread::sleep(
                options
                    .poll_interval
                    .min(options.timeout.saturating_sub(started.elapsed())),
            );
            continue;
        }
        // The requested ViewRoot is the exact full identity hydrated before
        // this registry-only command. Health proves its root; retain the
        // requested full identity only after that root matches, rather than
        // pretending the constant-size health reply carried a version.
        last_revision = Some(requested_revision.clone());
        let reply = session
            .surface(SurfaceCommand::Explore {
                query: None,
                limit: 24,
            })
            .map_err(|source| ReadinessError::Service {
                operation: "registry-explore",
                source,
            })?;
        let SurfaceReply::Explored(rows) = reply else {
            return Err(ReadinessError::ReplyShape {
                operation: "registry-explore",
            });
        };
        let package = rows.first().map(|row| SelectedIdentity {
            coordinate: row.coordinate.as_str().to_owned(),
            stable_id: format!(
                "package:{}",
                encode_id(package_key(row.coordinate.as_str()).as_bytes())
            ),
        });
        event(
            &mut events,
            polls,
            started,
            "registry-prerequisites-observed",
            Some(requested_revision.clone()),
            Some(format!("packages={}", rows.len())),
        );
        if package.is_some() {
            event(
                &mut events,
                polls,
                started,
                "route-ready",
                Some(requested_revision.clone()),
                None,
            );
            return Ok(report(
                workspace_root,
                surface,
                requested_revision.clone(),
                requested_revision.root.clone(),
                started,
                polls,
                events,
                true,
                false,
                None,
                package,
            ));
        }
        thread::sleep(
            options
                .poll_interval
                .min(options.timeout.saturating_sub(started.elapsed())),
        );
    }
}

fn observe_local(
    session: &mut Session,
    surface: ReaderSurface,
    workspace_coordinate: &str,
    health_root: backend_library::ViewStateRoot,
) -> Result<Option<LocalObservation>, ReadinessError> {
    let packages = session
        .packages()
        .map_err(|source| ReadinessError::Service {
            operation: "package-anchors",
            source,
        })?;
    let CommandReply::Packages(packages) = packages.reply else {
        return Err(ReadinessError::ReplyShape {
            operation: "package-anchors",
        });
    };
    let package_root = packages.root.basis().root;
    if package_root != health_root {
        return Ok(None);
    }
    let package = first_project_identity(&packages.root, workspace_coordinate);
    let declaration = if surface.needs_declaration() && package.is_some() {
        let outline = session
            .outline_page(workspace_coordinate, 200, None)
            .map_err(|source| ReadinessError::Service {
                operation: "declaration-anchors",
                source,
            })?;
        let CommandReply::ProjectionPage(outline) = outline.reply else {
            return Err(ReadinessError::ReplyShape {
                operation: "declaration-anchors",
            });
        };
        if outline.snapshot.root.basis().root != health_root {
            return Ok(None);
        }
        first_declaration_identity(&outline.snapshot.root, workspace_coordinate)
    } else {
        None
    };
    Ok(Some(LocalObservation {
        owner_root: package_root,
        declaration,
        package,
    }))
}

struct ExistingAnchor {
    has_any_project_row: bool,
}

fn existing_local_anchor(root: &ViewRoot, workspace_coordinate: &str) -> ExistingAnchor {
    let package = package_key(workspace_coordinate);
    let has_any_project_row = root
        .rows()
        .iter()
        .any(|row| row.id == RowId::Package(package));
    ExistingAnchor {
        // A package row in Requested/Loading state is itself evidence that
        // the production add intent was admitted. Re-submitting it here
        // would create duplicate work while the same source snapshot settles;
        // the bounded loop below reports a missing declaration if it never
        // arrives.
        has_any_project_row,
    }
}

fn first_project_identity(root: &ViewRoot, workspace_coordinate: &str) -> Option<SelectedIdentity> {
    let package = package_key(workspace_coordinate);
    root.rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Package(_)))
        .find(|row| row.id == RowId::Package(package))
        .map(identity_of_row)
}

fn first_declaration_identity(
    root: &ViewRoot,
    workspace_coordinate: &str,
) -> Option<SelectedIdentity> {
    let package = package_key(workspace_coordinate);
    root.rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Symbol(_)))
        .find(|row| row.package == Some(package))
        .map(identity_of_row)
}

fn identity_of_row(row: &Row) -> SelectedIdentity {
    // Keep coordinate spelling in the same typed projection used by the
    // desktop/CLI/MCP record surfaces.  A row label is a producer payload and
    // may be shared by duplicate declarations; its stable row ID is the
    // authority that disambiguates the selected route identity.
    let identity = backend_present::Record::from_row(row);
    SelectedIdentity {
        coordinate: identity.identity().coordinate().as_str().to_owned(),
        stable_id: row.id.stable_key(),
    }
}

fn timeout_cause(
    surface: ReaderSurface,
    index_requested: bool,
    observed_revision: bool,
    revision_changed: bool,
) -> ReadinessTimeoutCause {
    if revision_changed {
        return ReadinessTimeoutCause::RevisionDidNotSettle;
    }
    if surface.needs_registry() {
        return ReadinessTimeoutCause::PackageAnchorMissing;
    }
    if surface.needs_declaration() {
        if !observed_revision && index_requested {
            ReadinessTimeoutCause::IndexNotCommitted
        } else {
            ReadinessTimeoutCause::DeclarationAnchorMissing
        }
    } else if !observed_revision && index_requested {
        ReadinessTimeoutCause::IndexNotCommitted
    } else {
        ReadinessTimeoutCause::ProjectAnchorMissing
    }
}

fn report(
    workspace_root: PathBuf,
    requested_surface: ReaderSurface,
    requested_revision: RevisionIdentity,
    admitted_root: String,
    started: Instant,
    polls: u32,
    events: Vec<ReadinessEvent>,
    reused_existing_projection: bool,
    index_requested: bool,
    selected_declaration: Option<SelectedIdentity>,
    selected_package: Option<SelectedIdentity>,
) -> ReadinessReport {
    ReadinessReport {
        workspace_root,
        requested_surface,
        requested_root: requested_revision.root.clone(),
        admitted_root,
        requested_revision: requested_revision.clone(),
        // A health/packages observation proves the owner root but does not
        // carry the full ViewVersion. The caller pins this field after the
        // subscription bootstrap hydrates that exact root.
        admitted_revision: requested_revision,
        reused_existing_projection,
        index_requested,
        duration_ms: started.elapsed().as_millis(),
        polls,
        events,
        selected_declaration,
        selected_package,
    }
}

fn event(
    events: &mut Vec<ReadinessEvent>,
    poll: u32,
    started: Instant,
    kind: &str,
    revision: Option<RevisionIdentity>,
    detail: Option<String>,
) {
    event_with_root(events, poll, started, kind, revision, None, detail);
}

fn event_with_root(
    events: &mut Vec<ReadinessEvent>,
    poll: u32,
    started: Instant,
    kind: &str,
    revision: Option<RevisionIdentity>,
    observed_root: Option<String>,
    detail: Option<String>,
) {
    events.push(ReadinessEvent {
        poll,
        elapsed_ms: started.elapsed().as_millis(),
        kind: kind.to_owned(),
        revision,
        observed_root,
        detail,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_prerequisites_are_shared_across_reader_surfaces() {
        assert!(ReaderSurface::Declaration.needs_local_index());
        assert!(ReaderSurface::Declaration.needs_declaration());
        assert!(!ReaderSurface::Declaration.needs_registry());
        assert!(ReaderSurface::Package.needs_registry());
        assert!(!ReaderSurface::Package.needs_local_index());
        assert!(!ReaderSurface::Browse.needs_local_index());
    }

    #[test]
    fn timeout_causes_distinguish_uncommitted_index_from_missing_anchor() {
        assert_eq!(
            timeout_cause(ReaderSurface::Declaration, true, false, false),
            ReadinessTimeoutCause::IndexNotCommitted
        );
        assert_eq!(
            timeout_cause(ReaderSurface::Declaration, true, true, false),
            ReadinessTimeoutCause::DeclarationAnchorMissing
        );
        assert_eq!(
            timeout_cause(ReaderSurface::Project, false, true, false),
            ReadinessTimeoutCause::ProjectAnchorMissing
        );
        assert_eq!(
            timeout_cause(ReaderSurface::Package, false, false, false),
            ReadinessTimeoutCause::PackageAnchorMissing
        );
        assert_eq!(
            timeout_cause(ReaderSurface::Declaration, true, true, true),
            ReadinessTimeoutCause::RevisionDidNotSettle
        );
    }

    #[test]
    fn options_have_a_bounded_default() {
        let options = ReadinessOptions::default();
        assert!(options.timeout > options.poll_interval);
        assert!(!options.timeout.is_zero());
        assert!(!options.poll_interval.is_zero());
    }

    #[test]
    fn duplicate_coordinates_keep_stable_row_identity() {
        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(b"readiness-test"),
        );
        let first = Row::new(
            RowId::Symbol(backend_library::symbol_key("duplicate:first")),
            basis,
            "/workspace::src/lib.rs:1::duplicate",
        );
        let second = Row::new(
            RowId::Symbol(backend_library::symbol_key("duplicate:second")),
            basis,
            "/workspace::src/lib.rs:1::duplicate",
        );
        let first = identity_of_row(&first);
        let second = identity_of_row(&second);
        assert_eq!(first.coordinate, second.coordinate);
        assert_ne!(first.stable_id, second.stable_id);
    }
}
